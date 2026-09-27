use std::cell::Cell;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use neoutl_media_api::{
    ColorMeta, DECODE_WATCHDOG_TIMEOUT, MediaKind, MediaMeta, MediaVTable, VideoSource,
};

use crate::decoder::{VideoDecoder, VideoMeta};
use crate::frame::VideoFrameStore;

const FRAME_RENDER_BUDGET: Duration = Duration::from_millis(10);
/// 実質無期限待ち。実際の上限は呼び出し元(runtime層)のDECODE_WATCHDOG_TIMEOUTが
/// 一箇所で強制するため、ここでは`Instant + Duration`のオーバーフローを避けつつ
/// 十分大きい値を置くのみで判定ロジックは持たない。
const UNBOUNDED_WAIT: Duration = Duration::from_secs(3600);
const CLIP_KEY: &str = "ffmpeg_decoder_source";

pub struct FfmpegVideoSource {
    decoder: VideoDecoder,
    store: Arc<VideoFrameStore>,
    width: u32,
    height: u32,
    fps: f64,
    total_frames: i64,
    last_color_meta: Cell<ColorMeta>,
}

impl FfmpegVideoSource {
    fn open(path: &Path) -> Result<Self, String> {
        if let Err(failure) = neo_media_support::probe(path) {
            return Err(format!(
                "動画を開けません: {} ({failure:?})",
                failure.message()
            ));
        }

        let store = VideoFrameStore::new();
        let (tx, rx) = mpsc::channel::<VideoMeta>();
        let store_thread = store.clone();
        let decoder = VideoDecoder::open(
            path,
            CLIP_KEY.to_owned(),
            store_thread,
            crate::decoder::shared_wgpu_device(),
            crate::decoder::shared_wgpu_queue(),
            move |meta| {
                let _ = tx.send(meta);
            },
        );
        let meta = rx
            .recv_timeout(DECODE_WATCHDOG_TIMEOUT)
            .map_err(|e| format!("動画メタ情報取得タイムアウト: {e}"))?;
        Ok(Self {
            decoder,
            store,
            width: meta.width,
            height: meta.height,
            fps: meta.fps,
            total_frames: meta.total_frames,
            last_color_meta: Cell::new(ColorMeta::default()),
        })
    }
}

impl VideoSource for FfmpegVideoSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn fps(&self) -> f64 {
        self.fps
    }
    fn total_frames(&self) -> i64 {
        self.total_frames
    }

    fn prefetch(&mut self, frame_index: i64) -> Result<(), String> {
        self.decoder.seek_to_frame(frame_index);
        Ok(())
    }

    fn frame_gpu(
        &mut self,
        frame_index: i64,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
    ) -> Result<wgpu::Texture, String> {
        if let Some(frame) = self.store.frame(CLIP_KEY, frame_index) {
            self.last_color_meta.set(frame.0.color_meta);
            return Ok(frame.0.texture.clone());
        }

        self.decoder.seek_to_frame(frame_index);

        if let Some(frame) = self
            .store
            .wait_for_frame(CLIP_KEY, frame_index, FRAME_RENDER_BUDGET)
        {
            self.last_color_meta.set(frame.0.color_meta);
            return Ok(frame.0.texture.clone());
        }

        if let Some(frame) = self.store.any_frame(CLIP_KEY) {
            self.last_color_meta.set(frame.0.color_meta);
            return Ok(frame.0.texture.clone());
        }

        self.store
            .wait_for_frame(CLIP_KEY, frame_index, UNBOUNDED_WAIT)
            .map(|frame| {
                self.last_color_meta.set(frame.0.color_meta);
                frame.0.texture.clone()
            })
            .ok_or_else(|| format!("フレーム取得失敗: frame_index={frame_index}"))
    }

    fn last_color_meta(&self) -> ColorMeta {
        self.last_color_meta.get()
    }
}

static EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "avi", "webm", "m4v"];

static META: MediaMeta = MediaMeta {
    id: "neoutl.media.ffmpeg",
    name: "FFmpeg Video Decoder",
    kind: MediaKind::Video,
    extensions_ptr: EXTENSIONS.as_ptr(),
    extensions_len: EXTENSIONS.len(),
};

fn meta() -> &'static MediaMeta {
    &META
}

fn open_video(path: &Path) -> Result<Box<dyn VideoSource>, String> {
    FfmpegVideoSource::open(path).map(|s| Box::new(s) as Box<dyn VideoSource>)
}

pub fn native_vtable() -> MediaVTable {
    MediaVTable {
        meta,
        open_video: Some(open_video),
        open_image: None,
        decode_audio: None,
    }
}
