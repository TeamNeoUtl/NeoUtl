use std::ffi::{CString, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use ffmpeg_sys_next as sys;

use crate::frame::VideoFrame;
use crate::index::{FrameIndex, build_index};

use super::codec_registry::codec_kind_of;
use super::hw_device::{
    HwPixFmtBox, find_decoder_for_codec_id, hw_get_format, is_force_sw_decode,
    poisoned_hw_types_for, sw_forced_get_format, try_init_hw_device,
};
use super::packet_queue::{PacketQueue, SeekLock};
use super::{effective_decode_thread_cap, hw_decode_extra_frames, shared_wgpu_queue};

pub(crate) struct OpenContext {
    pub(crate) fmt_ctx: *mut sys::AVFormatContext,
    pub(crate) dec_ctx: *mut sys::AVCodecContext,
    pub(crate) stream_index: i32,
    pub(crate) fps: f64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) index: FrameIndex,
    pub(crate) hw_device_ctx: *mut sys::AVBufferRef,
    pub(crate) hw_pix_fmt_box: Option<Box<HwPixFmtBox>>,
    pub(crate) gpu_device: Option<Arc<wgpu::Device>>,
    pub(crate) gpu_queue: Option<Arc<wgpu::Queue>>,
    pub(crate) last_good_frame: Option<VideoFrame>,
    pub(crate) packet_queue: Arc<PacketQueue>,
    pub(crate) seek_lock: Arc<SeekLock>,
    pub(crate) reader_stop: Arc<AtomicBool>,
    pub(crate) reader_join: Option<JoinHandle<()>>,
    pub(crate) hw_device_type: i32,
    pub(crate) source_path: PathBuf,
    pub(crate) hw_poisoned: bool,
}

unsafe impl Send for OpenContext {}

impl Drop for OpenContext {
    fn drop(&mut self) {
        self.reader_stop.store(true, Ordering::Release);
        self.packet_queue.flush();
        if let Some(join) = self.reader_join.take() {
            let _ = join.join();
        }
        if self.hw_poisoned {
            eprintln!(
                "[neoutl-video-decoder][診断][hw_poisoned] dec_ctx/hw_device_ctx解放省略（ドライバクラッシュ回避、意図的リーク）"
            );
            unsafe {
                if !self.fmt_ctx.is_null() {
                    sys::avformat_close_input(&mut self.fmt_ctx);
                }
            }
            return;
        }
        unsafe {
            if !self.dec_ctx.is_null() {
                sys::avcodec_free_context(&mut self.dec_ctx);
            }
            if !self.fmt_ctx.is_null() {
                sys::avformat_close_input(&mut self.fmt_ctx);
            }
            if !self.hw_device_ctx.is_null() {
                sys::av_buffer_unref(&mut self.hw_device_ctx);
            }
        }
    }
}

pub(crate) fn open_input(
    path: &Path,
    gpu_device: &Option<Arc<wgpu::Device>>,
) -> Result<OpenContext, String> {
    unsafe {
        let mut fmt_ctx: *mut sys::AVFormatContext = ptr::null_mut();
        let c_path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|e| format!("パスにNUL文字が含まれる: {e}"))?;
        if sys::avformat_open_input(
            &mut fmt_ctx,
            c_path.as_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
        ) != 0
        {
            return Err(format!("avformat_open_input失敗: {}", path.display()));
        }
        if sys::avformat_find_stream_info(fmt_ctx, ptr::null_mut()) < 0 {
            sys::avformat_close_input(&mut fmt_ctx);
            return Err("avformat_find_stream_info失敗".to_owned());
        }

        let stream_index = sys::av_find_best_stream(
            fmt_ctx,
            sys::AVMediaType::AVMEDIA_TYPE_VIDEO,
            -1,
            -1,
            ptr::null_mut(),
            0,
        );
        if stream_index < 0 {
            sys::avformat_close_input(&mut fmt_ctx);
            return Err("映像ストリームが見つからない".to_owned());
        }

        let stream = *(*fmt_ctx).streams.add(stream_index as usize);
        let mut fps =
            (*stream).avg_frame_rate.num as f64 / (*stream).avg_frame_rate.den.max(1) as f64;
        if fps <= 0.0 {
            fps = (*stream).r_frame_rate.num as f64 / (*stream).r_frame_rate.den.max(1) as f64;
        }

        let codec = find_decoder_for_codec_id((*(*stream).codecpar).codec_id);
        if codec.is_null() {
            sys::avformat_close_input(&mut fmt_ctx);
            return Err("対応デコーダなし".to_owned());
        }

        let dec_ctx = sys::avcodec_alloc_context3(codec);
        if dec_ctx.is_null() {
            sys::avformat_close_input(&mut fmt_ctx);
            return Err("avcodec_alloc_context3失敗".to_owned());
        }
        if sys::avcodec_parameters_to_context(dec_ctx, (*stream).codecpar) < 0 {
            sys::avcodec_free_context(&mut { dec_ctx });
            sys::avformat_close_input(&mut fmt_ctx);
            return Err("avcodec_parameters_to_context失敗".to_owned());
        }

        let mut hw_device_ctx: *mut sys::AVBufferRef = ptr::null_mut();
        let mut hw_pix_fmt_box: Option<Box<HwPixFmtBox>> = None;
        let mut hw_device_type_i32: i32 = 0;

        let stream_sw_format =
            std::mem::transmute::<i32, sys::AVPixelFormat>((*(*stream).codecpar).format);

        let excluded = poisoned_hw_types_for(path);
        let codec_kind = codec_kind_of((*(*stream).codecpar).codec_id);
        let force_sw = codec_kind.is_some_and(is_force_sw_decode);
        let hw_result = if force_sw {
            None
        } else {
            try_init_hw_device(codec, stream_sw_format, gpu_device, &excluded)
        };

        if let Some((created_hw_ctx, hw_pix_fmt, device_type_i32)) = hw_result {
            hw_device_ctx = created_hw_ctx;
            hw_device_type_i32 = device_type_i32;
            let boxed = Box::new(HwPixFmtBox {
                pix_fmt: hw_pix_fmt,
            });
            (*dec_ctx).opaque = boxed.as_ref() as *const HwPixFmtBox as *mut c_void;
            (*dec_ctx).get_format = Some(hw_get_format);
            (*dec_ctx).hw_device_ctx = sys::av_buffer_ref(hw_device_ctx);
            (*dec_ctx).extra_hw_frames = hw_decode_extra_frames();
            hw_pix_fmt_box = Some(boxed);
        } else {
            let capabilities = (*codec).capabilities;
            let thread_cap = effective_decode_thread_cap().max(1);
            if (capabilities & sys::AV_CODEC_CAP_FRAME_THREADS as i32) != 0 {
                (*dec_ctx).thread_type = sys::FF_THREAD_FRAME;
                (*dec_ctx).thread_count = thread_cap;
            } else if (capabilities & sys::AV_CODEC_CAP_SLICE_THREADS as i32) != 0 {
                (*dec_ctx).thread_type = sys::FF_THREAD_SLICE;
                (*dec_ctx).thread_count = thread_cap;
            }
            (*dec_ctx).get_format = Some(sw_forced_get_format);
        }

        if sys::avcodec_open2(dec_ctx, codec, ptr::null_mut()) != 0 {
            if !hw_device_ctx.is_null() {
                sys::av_buffer_unref(&mut hw_device_ctx);
            }
            sys::avcodec_free_context(&mut { dec_ctx });
            sys::avformat_close_input(&mut fmt_ctx);
            return Err("avcodec_open2失敗".to_owned());
        }

        let width = (*dec_ctx).width.max(0) as u32;
        let height = (*dec_ctx).height.max(0) as u32;

        let gpu_device_owned = gpu_device.clone();
        let gpu_queue_owned = shared_wgpu_queue();

        if sys::av_seek_frame(fmt_ctx, stream_index, 0, sys::AVSEEK_FLAG_BACKWARD) < 0 {
            sys::av_seek_frame(fmt_ctx, -1, 0, sys::AVSEEK_FLAG_BACKWARD);
        }
        let index = build_index(fmt_ctx, stream_index);
        if sys::av_seek_frame(fmt_ctx, stream_index, 0, sys::AVSEEK_FLAG_BACKWARD) < 0 {
            sys::av_seek_frame(fmt_ctx, -1, 0, sys::AVSEEK_FLAG_BACKWARD);
        }
        sys::avcodec_flush_buffers(dec_ctx);

        Ok(OpenContext {
            fmt_ctx,
            dec_ctx,
            stream_index,
            fps: if fps > 0.0 { fps } else { 30.0 },
            width,
            height,
            index,
            hw_device_ctx,
            hw_pix_fmt_box,
            gpu_device: gpu_device_owned,
            gpu_queue: gpu_queue_owned,
            last_good_frame: None,
            packet_queue: Arc::new(PacketQueue::new()),
            seek_lock: Arc::new(SeekLock::new()),
            reader_stop: Arc::new(AtomicBool::new(false)),
            reader_join: None,
            hw_poisoned: false,
            hw_device_type: hw_device_type_i32,
            source_path: path.to_path_buf(),
        })
    }
}

/// ISOBMFF av1Cのバイトレイアウト: byte0=marker+version, byte1=seq_profile/seq_level_idx_0,
/// byte2=フラグ群, byte3=予約+initial_presentation_delay, byte4以降=configOBUs
/// (sequence_header_obuを含む生OBU列)。AV1 Codec Configuration Box (av1C) 仕様上固定。
const AV1C_CONFIG_OBUS_OFFSET: usize = 4;

/// avcodec_flush_buffers()はFFmpegネイティブAV1デコーダの内部sequence header状態を破棄する。
/// MKV/MP4のAV1格納形式 (av1C) はsequence header OBUをextradataに一度だけ格納し、
/// GOPごとに再送しないため、flush後最初のkeyframeパケットだけではデコーダが
/// sequence headerを再取得できず「No sequence header available」で失敗し続ける。
/// dec_ctx.extradata中のconfigOBUs (sequence header OBUを含む) のみを合成パケットとして
/// send_packetすることで、実データを変更せずデコーダ内部状態のみを復元する。
/// codec_idがAV1以外の場合、およびextradataがAV1C_CONFIG_OBUS_OFFSET以下の場合は何もしない。
unsafe fn resend_av1_sequence_header_if_needed(dec_ctx: *mut sys::AVCodecContext) {
    unsafe {
        if (*dec_ctx).codec_id != sys::AVCodecID::AV_CODEC_ID_AV1 {
            return;
        }
        let extradata = (*dec_ctx).extradata;
        let extradata_size = (*dec_ctx).extradata_size.max(0) as usize;
        if extradata.is_null() || extradata_size <= AV1C_CONFIG_OBUS_OFFSET {
            return;
        }
        let obu_ptr = extradata.add(AV1C_CONFIG_OBUS_OFFSET);
        let obu_len = extradata_size - AV1C_CONFIG_OBUS_OFFSET;

        let pkt = sys::av_packet_alloc();
        if pkt.is_null() {
            return;
        }
        if sys::av_new_packet(pkt, obu_len as i32) < 0 {
            sys::av_packet_free(&mut { pkt });
            return;
        }
        ptr::copy_nonoverlapping(obu_ptr, (*pkt).data, obu_len);
        sys::avcodec_send_packet(dec_ctx, pkt);
        sys::av_packet_free(&mut { pkt });
    }
}

pub(crate) fn seek_to_keyframe(ctx: &mut OpenContext, keyframe_index: i64) {
    let _seek_guard = ctx.seek_lock.0.lock().expect("seek lock poisoned");
    ctx.packet_queue.flush();
    unsafe {
        (*ctx.dec_ctx).skip_loop_filter = sys::AVDiscard::AVDISCARD_NONREF;

        let seek_pts = ctx.index.pts_at(keyframe_index);
        if sys::avformat_seek_file(
            ctx.fmt_ctx,
            ctx.stream_index,
            i64::MIN,
            seek_pts,
            seek_pts,
            sys::AVSEEK_FLAG_BACKWARD,
        ) < 0
        {
            sys::av_seek_frame(
                ctx.fmt_ctx,
                ctx.stream_index,
                seek_pts,
                sys::AVSEEK_FLAG_BACKWARD,
            );
        }
        sys::avcodec_flush_buffers(ctx.dec_ctx);
        resend_av1_sequence_header_if_needed(ctx.dec_ctx);
    }
}
