use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use ffmpeg_sys_next as sys;

use crate::cache::{GopCache, GopCacheBlock, RamFrameCache};
use crate::colorconv::P0xxGpuResources;
use crate::frame::VideoFrameStore;

use super::av_errors::{averror_eagain, averror_eof, ignore_send_packet_result};
use super::frame_convert::{compose_output_frame, convert_frame};
use super::hw_device::mark_hw_device_poisoned;
use super::open::{OpenContext, open_input, seek_to_keyframe};
use super::packet_queue::{SendPtr, packet_reader_loop};
use super::{Mailbox, VideoMeta, shared_media_cache};

const GOP_CACHE_CAPACITY: usize = 3;
const FORWARD_DECODE_THRESHOLD: i64 = 120;
const RAM_FRAME_CACHE_MARGIN: usize = 2;

fn rgba16f_frame_bytes(width: u32, height: u32) -> u64 {
    width as u64 * height as u64 * 8
}

fn ram_frame_cache_capacity(width: u32, height: u32) -> usize {
    shared_media_cache()
        .map(|cache| cache.effective_ram_capacity(rgba16f_frame_bytes(width, height)))
        .unwrap_or(neo_media_cache::RAM_MIN_CAPACITY)
        .saturating_sub(RAM_FRAME_CACHE_MARGIN)
        .max(1)
}

pub(crate) struct WorkerSpawnRequest<F: FnOnce(VideoMeta) + Send + 'static> {
    pub(crate) path: std::path::PathBuf,
    pub(crate) clip_key: String,
    pub(crate) store: Arc<VideoFrameStore>,
    pub(crate) gpu_device: Option<Arc<wgpu::Device>>,
    pub(crate) gpu_queue: Option<Arc<wgpu::Queue>>,
    pub(crate) shared: Arc<(Mutex<Mailbox>, Condvar)>,
    pub(crate) is_ready: Arc<AtomicBool>,
    pub(crate) last_requested_frame: Arc<AtomicI64>,
    pub(crate) on_ready: F,
}

pub(crate) fn run_worker<F: FnOnce(VideoMeta) + Send + 'static>(req: WorkerSpawnRequest<F>) {
    let WorkerSpawnRequest {
        path,
        clip_key,
        store,
        gpu_device,
        gpu_queue: _gpu_queue,
        shared,
        is_ready,
        last_requested_frame,
        on_ready,
    } = req;
    let mut ctx = match open_input(&path, &gpu_device) {
        Ok(ctx) => ctx,
        Err(e) => {
            match neo_media_support::probe(&path) {
                Ok(_) => {
                    eprintln!("[neoutl-video-decoder] open失敗: {e}");
                }
                Err(failure) => {
                    eprintln!(
                        "[neoutl-video-decoder][非対応] probe判定: {} ({failure:?}) open失敗: {e}",
                        failure.message()
                    );
                }
            }
            return;
        }
    };

    {
        let fmt_ctx = SendPtr(ctx.fmt_ctx);
        let stream_index = ctx.stream_index;
        let queue = ctx.packet_queue.clone();
        let seek_lock = ctx.seek_lock.clone();
        let stop = ctx.reader_stop.clone();
        let join = std::thread::Builder::new()
            .name("neoutl-packet-reader".into())
            .spawn(move || {
                let fmt_ctx = fmt_ctx;
                packet_reader_loop(fmt_ctx.0, stream_index, queue, seek_lock, stop)
            })
            .expect("packet reader thread spawn failed");
        ctx.reader_join = Some(join);
    }

    is_ready.store(true, Ordering::Release);
    on_ready(VideoMeta {
        total_frames: ctx.index.len(),
        fps: ctx.fps,
        width: ctx.width,
        height: ctx.height,
    });

    let mut caches = DecodeCaches {
        ram_cache: RamFrameCache::new(ram_frame_cache_capacity(ctx.width, ctx.height)),
        p0xx_resources: None,
        gop_cache: GopCache::new(GOP_CACHE_CAPACITY),
        last_decoded_frame: -1,
    };

    let (lock, cvar) = &*shared;
    loop {
        let target = {
            let mut guard = lock.lock().expect("mailbox mutex poisoned");
            loop {
                if guard.target_frame.is_some() {
                    break;
                }
                if guard.stopped {
                    return;
                }
                guard = cvar.wait(guard).expect("mailbox condvar poisoned");
            }
            guard.target_frame.take().expect("target_frame must be set")
        };

        decode_task(
            &mut ctx,
            target,
            &clip_key,
            &store,
            &mut caches,
            &last_requested_frame,
        );

        let max_index = (ctx.index.len() as i64 - 1).max(0);
        let latest_requested = last_requested_frame.load(Ordering::Acquire);
        let latest_clamped = latest_requested.clamp(0, max_index);
        let dispatched_clamped = target.clamp(0, max_index);
        if latest_requested >= 0 && latest_clamped != dispatched_clamped {
            let mut guard = lock.lock().expect("mailbox mutex poisoned");
            if guard.target_frame.is_none() && !guard.stopped {
                eprintln!(
                    "[neoutl-video-decoder][診断][収束再投入] dispatched={dispatched_clamped} latest_requested={latest_clamped}"
                );
                guard.target_frame = Some(latest_clamped);
                cvar.notify_one();
            }
        }
    }
}

struct DecodeCaches {
    ram_cache: RamFrameCache,
    p0xx_resources: Option<P0xxGpuResources>,
    gop_cache: GopCache,
    last_decoded_frame: i64,
}

/// ram_frame→GPU化→store反映→last_good_frame更新を1箇所に統一する。
/// GPUデバイス未取得時はfalseを返し、呼び出し側がフォールバック分岐を判断する。
fn dispatch_frame(
    ctx: &mut OpenContext,
    store: &Arc<VideoFrameStore>,
    clip_key: &str,
    index: i64,
    ram_frame: &crate::frame::RamFrame,
    p0xx_resources: &mut Option<P0xxGpuResources>,
) -> bool {
    let media_cache = shared_media_cache();
    let (Some(_), Some(queue), Some(cache)) = (
        ctx.gpu_device.as_ref(),
        ctx.gpu_queue.as_ref(),
        media_cache.as_ref(),
    ) else {
        return false;
    };
    let Some(frame) = compose_output_frame(ram_frame, queue, cache, p0xx_resources) else {
        return false;
    };
    store.set_frame(clip_key, index, frame.clone());
    ctx.last_good_frame = Some(frame);
    true
}

fn decode_task(
    ctx: &mut OpenContext,
    requested_target: i64,
    clip_key: &str,
    store: &Arc<VideoFrameStore>,
    caches: &mut DecodeCaches,
    last_requested_frame: &AtomicI64,
) {
    let DecodeCaches {
        ram_cache,
        p0xx_resources,
        gop_cache,
        last_decoded_frame,
    } = caches;
    if ctx.index.is_empty() {
        return;
    }
    let target = requested_target.clamp(0, ctx.index.len() - 1);
    let max_index = (ctx.index.len() as i64 - 1).max(0);

    if let Some(ram_frame) = gop_cache.get(target, ram_cache)
        && dispatch_frame(ctx, store, clip_key, target, &ram_frame, p0xx_resources)
    {
        eprintln!(
            "[neoutl-video-decoder][診断][decode_task終了][gop_cache即応] requested_target={requested_target} target={target}"
        );
        return;
    }
    if let Some(ram_frame) = ram_cache.get(target)
        && dispatch_frame(ctx, store, clip_key, target, &ram_frame, p0xx_resources)
    {
        eprintln!(
            "[neoutl-video-decoder][診断][decode_task終了][ram_cache即応] requested_target={requested_target} target={target}"
        );
        return;
    }

    let key_index = ctx.index.preceding_keyframe(target);
    let gop_end = ctx.index.gop_end_of(target);

    let contiguous_forward = *last_decoded_frame != -1
        && target > *last_decoded_frame
        && target <= *last_decoded_frame + FORWARD_DECODE_THRESHOLD;
    let need_seek = !contiguous_forward;
    let should_fill_gop = need_seek;

    if need_seek {
        seek_to_keyframe(ctx, key_index);
        *last_decoded_frame = key_index - 1;
    }

    let mut new_gop_block = GopCacheBlock {
        keyframe_index: key_index,
        start: key_index,
        end: gop_end,
        frame_indices: Vec::new(),
    };

    let mut target_dispatched = false;
    let mut decode_budget = (gop_end - key_index + 10).max(500);
    let mut eof = false;
    let av_frame = unsafe { sys::av_frame_alloc() };
    let mut frames_since_dispatch: u32 = 0;

    while decode_budget > 0 {
        decode_budget -= 1;

        let mut send_ret = 0;
        if !eof {
            match ctx.packet_queue.pop_blocking(&ctx.reader_stop) {
                Some(mut slot) => {
                    if slot.eof {
                        eof = true;
                    } else {
                        send_ret = unsafe { sys::avcodec_send_packet(ctx.dec_ctx, slot.packet) };
                        unsafe { sys::av_packet_free(&mut slot.packet) };
                    }
                }
                None => {
                    eof = true;
                }
            }
        }
        if eof {
            send_ret = unsafe { sys::avcodec_send_packet(ctx.dec_ctx, ptr::null()) };
        }
        if send_ret < 0 && !ignore_send_packet_result(send_ret) {
            break;
        }

        loop {
            let recv_ret = unsafe { sys::avcodec_receive_frame(ctx.dec_ctx, av_frame) };
            if recv_ret == averror_eagain() {
                break;
            }
            if recv_ret == averror_eof() {
                eof = true;
                break;
            }
            if recv_ret < 0 {
                if !ctx.hw_device_ctx.is_null() {
                    ctx.hw_poisoned = true;
                    mark_hw_device_poisoned(&ctx.source_path, ctx.hw_device_type);
                    eprintln!(
                        "[neoutl-video-decoder][診断][hw_poisoned検出] recv_ret={recv_ret} target={target} hwaccel致命的エラー、以後dec_ctx解放を禁止、backend={}除外登録",
                        ctx.hw_device_type
                    );
                }
                break;
            }

            let pts = unsafe {
                if (*av_frame).pts != sys::AV_NOPTS_VALUE {
                    (*av_frame).pts
                } else {
                    (*av_frame).pkt_dts
                }
            };
            let Some(decoded_index) = ctx.index.index_of_pts(pts) else {
                eprintln!(
                    "[neoutl-video-decoder][診断] index_of_pts不一致 pts={pts} target={target} (このフレームは破棄される)"
                );
                continue;
            };
            *last_decoded_frame = decoded_index;
            frames_since_dispatch += 1;

            if !ram_cache.contains(decoded_index) {
                if let Some(ram_frame) = convert_frame(ctx, av_frame) {
                    ram_cache.insert(decoded_index, ram_frame.clone());
                    new_gop_block.frame_indices.push(decoded_index);

                    if decoded_index == target && !target_dispatched {
                        if dispatch_frame(
                            ctx,
                            store,
                            clip_key,
                            decoded_index,
                            &ram_frame,
                            p0xx_resources,
                        ) {
                            target_dispatched = true;
                        } else if ctx.gpu_device.is_none() || ctx.gpu_queue.is_none() {
                            eprintln!(
                                "[neoutl-video-decoder][非対応] wgpuデバイス未取得、昇格スキップ"
                            );
                        }
                    }
                }
            } else if decoded_index == target && !target_dispatched {
                if let Some(ram_frame) = ram_cache.get(decoded_index) {
                    dispatch_frame(
                        ctx,
                        store,
                        clip_key,
                        decoded_index,
                        &ram_frame,
                        p0xx_resources,
                    );
                }
                target_dispatched = true;
            }

            let latest_clamped = last_requested_frame
                .load(Ordering::Acquire)
                .clamp(0, max_index);
            if frames_since_dispatch > 0 && latest_clamped != target {
                let superseded_by = latest_clamped;
                eprintln!(
                    "[neoutl-video-decoder][診断][decode_task中断] requested_target={requested_target} \
target={target} last_decoded_frame={last_decoded_frame} superseded_by={superseded_by} \
decoded_frame_count={}",
                    new_gop_block.frame_indices.len(),
                    last_decoded_frame = *last_decoded_frame,
                );
                if !new_gop_block.frame_indices.is_empty() {
                    gop_cache.put(new_gop_block);
                }
                unsafe {
                    sys::av_frame_free(&mut { av_frame });
                }
                return;
            }

            if (!should_fill_gop && *last_decoded_frame >= target) || *last_decoded_frame >= gop_end
            {
                break;
            }
        }

        if eof
            || (!should_fill_gop && *last_decoded_frame >= target)
            || *last_decoded_frame >= gop_end
            || ctx.hw_poisoned
        {
            break;
        }
    }

    unsafe {
        sys::av_frame_free(&mut { av_frame });
    }

    let decoded_count = new_gop_block.frame_indices.len();

    if !new_gop_block.frame_indices.is_empty() {
        gop_cache.put(new_gop_block);
    }

    {
        let boundary_lo = (target - 2).max(0);
        let boundary_hi = (target + 2).min(ctx.index.len() - 1);
        let mut boundary_dump = String::new();
        for i in boundary_lo..=boundary_hi {
            boundary_dump.push_str(&format!(
                "[{i}:pts={},key={}] ",
                ctx.index.pts_at(i),
                ctx.index.is_key_at(i)
            ));
        }
        eprintln!(
            "[neoutl-video-decoder][診断][decode_task終了] requested_target={requested_target} \
target={target} key_index={key_index} gop_end={gop_end} \
last_decoded_frame={last_decoded_frame} target_dispatched={target_dispatched} \
decoded_frame_count={decoded_count} boundary={boundary_dump}",
            last_decoded_frame = *last_decoded_frame,
        );
    }

    if !target_dispatched && let Some(frame) = ctx.last_good_frame.clone() {
        eprintln!(
            "[neoutl-video-decoder][診断][近似フレーム配信] requested_target={requested_target} target={target}"
        );
        store.set_frame(clip_key, requested_target, frame);
    }
}
