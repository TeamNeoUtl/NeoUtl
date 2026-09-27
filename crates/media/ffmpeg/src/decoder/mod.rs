use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

use neo_media_cache::NeoMediaCache;

use crate::frame::VideoFrameStore;
use crate::index::FrameIndex;

mod av_errors;
mod codec_registry;
mod frame_convert;
mod hw_device;
mod open;
mod packet_queue;
mod pixfmt;
pub(crate) mod staging_pool;
mod worker;

pub use codec_registry::CODEC_KIND_LIST;
pub use hw_device::{
    clear_hw_device_type_priority_for_codec, default_hw_device_type_priority, is_force_sw_decode,
    set_force_sw_decode, set_hw_device_type_priority, set_hw_device_type_priority_for_codec,
};

use staging_pool::StagingPool;

static SHARED_WGPU: OnceLock<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> = OnceLock::new();
static SHARED_CACHE: OnceLock<Arc<NeoMediaCache>> = OnceLock::new();
static SHARED_STAGING: OnceLock<Arc<StagingPool>> = OnceLock::new();
static SHARED_QUEUE_SUBMIT_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
static HW_DECODE_EXTRA_FRAMES: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(16);

pub fn set_hw_decode_extra_frames(count: i32) {
    HW_DECODE_EXTRA_FRAMES.store(count, std::sync::atomic::Ordering::Release);
}

pub(crate) fn hw_decode_extra_frames() -> i32 {
    HW_DECODE_EXTRA_FRAMES.load(std::sync::atomic::Ordering::Acquire)
}

static DECODE_THREAD_CAP: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// 0はsystem設定「自動」を意味し、effective_decode_thread_cap()で
/// available_parallelism()に解決される。
pub fn set_decode_thread_cap(count: i32) {
    DECODE_THREAD_CAP.store(count, std::sync::atomic::Ordering::Release);
}

pub(crate) fn effective_decode_thread_cap() -> i32 {
    let cap = DECODE_THREAD_CAP.load(std::sync::atomic::Ordering::Acquire);
    if cap > 0 {
        cap
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(1)
    }
}

pub fn shared_wgpu_submit_lock() -> Arc<Mutex<()>> {
    SHARED_QUEUE_SUBMIT_LOCK
        .get_or_init(|| Arc::new(Mutex::new(())))
        .clone()
}

pub fn set_shared_wgpu_device(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) {
    let budget_provider: Option<Arc<neo_media_cache::VramBudgetProvider>> = None;
    let ram_budget_provider: Option<Arc<neo_media_cache::RamBudgetProvider>> = None;
    let _ = SHARED_CACHE.set(Arc::new(NeoMediaCache::new(
        (*device).clone(),
        budget_provider,
        ram_budget_provider,
    )));
    if let Some(cache) = SHARED_CACHE.get() {
        cache.register_consumer(neo_media_cache::KIND_PLAYBACK, 3);
        cache.register_consumer(neo_media_cache::KIND_THUMBNAIL, 1);
        cache.register_consumer(neo_media_cache::KIND_LUA_SAMPLE, 1);
    }
    let _ = SHARED_STAGING.set(StagingPool::new(device.clone()));
    let _ = SHARED_WGPU.set((device, queue));
}

pub fn shared_wgpu_device() -> Option<Arc<wgpu::Device>> {
    SHARED_WGPU.get().map(|(device, _)| device.clone())
}

pub fn shared_wgpu_queue() -> Option<Arc<wgpu::Queue>> {
    SHARED_WGPU.get().map(|(_, queue)| queue.clone())
}

pub(crate) fn shared_media_cache() -> Option<Arc<NeoMediaCache>> {
    SHARED_CACHE.get().cloned()
}

pub(crate) fn shared_staging_pool() -> Option<Arc<StagingPool>> {
    SHARED_STAGING.get().cloned()
}

pub struct VideoMeta {
    pub total_frames: i64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct Mailbox {
    pub(crate) target_frame: Option<i64>,
    pub(crate) stopped: bool,
}

pub struct VideoDecoder {
    shared: Arc<(Mutex<Mailbox>, Condvar)>,
    join: Option<JoinHandle<()>>,
    pub is_ready: Arc<AtomicBool>,
    pub last_requested_frame: Arc<AtomicI64>,
}

impl VideoDecoder {
    pub fn open(
        path: impl AsRef<Path>,
        clip_key: String,
        store: Arc<VideoFrameStore>,
        gpu_device: Option<Arc<wgpu::Device>>,
        gpu_queue: Option<Arc<wgpu::Queue>>,
        on_ready: impl FnOnce(VideoMeta) + Send + 'static,
    ) -> Self {
        let shared = Arc::new((
            Mutex::new(Mailbox {
                target_frame: None,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let is_ready = Arc::new(AtomicBool::new(false));
        let last_requested_frame = Arc::new(AtomicI64::new(-1));

        let shared_thread = shared.clone();
        let is_ready_thread = is_ready.clone();
        let last_requested_thread = last_requested_frame.clone();
        let path = path.as_ref().to_owned();

        let join = std::thread::Builder::new()
            .name("neoutl-video-decoder".into())
            .spawn(move || {
                worker::run_worker(worker::WorkerSpawnRequest {
                    path,
                    clip_key,
                    store,
                    gpu_device,
                    gpu_queue,
                    shared: shared_thread,
                    is_ready: is_ready_thread,
                    last_requested_frame: last_requested_thread,
                    on_ready,
                });
            })
            .expect("video decoder thread spawn failed");

        Self {
            shared,
            join: Some(join),
            is_ready,
            last_requested_frame,
        }
    }

    pub fn seek_to_frame(&self, frame: i64) {
        if frame < 0 {
            return;
        }
        self.last_requested_frame.store(frame, Ordering::Release);
        let (lock, cvar) = &*self.shared;
        let mut mailbox = lock.lock().expect("mailbox mutex poisoned");
        if let Some(overwritten) = mailbox.target_frame.replace(frame)
            && overwritten != frame
        {
            eprintln!(
                "[neoutl-video-decoder][診断][seek上書き] overwritten={overwritten} new={frame}"
            );
        }
        cvar.notify_one();
    }

    pub fn seek_to_time(&self, seconds: f64, index: &FrameIndex, time_base: (i32, i32)) {
        let frame = index.index_from_seconds(seconds.max(0.0), time_base);
        self.seek_to_frame(frame);
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.shared;
            let mut mailbox = lock.lock().expect("mailbox mutex poisoned");
            mailbox.stopped = true;
            cvar.notify_one();
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
