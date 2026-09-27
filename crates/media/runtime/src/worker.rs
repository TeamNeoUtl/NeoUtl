use egui_wgpu::wgpu;
use neoutl_media_api::{ColorMeta, DECODE_WATCHDOG_TIMEOUT, VideoSource};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

static NEXT_WORKER_TOKEN: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static ACTIVE_WORKER_TOKEN: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

struct ActiveWorkerTokenGuard;

impl ActiveWorkerTokenGuard {
    fn enter(token: u64) -> Self {
        ACTIVE_WORKER_TOKEN.with(|c| c.set(Some(token)));
        Self
    }
}

impl Drop for ActiveWorkerTokenGuard {
    fn drop(&mut self) {
        ACTIVE_WORKER_TOKEN.with(|c| c.set(None));
    }
}

const PREFETCH_RADIUS: i64 = 8;
pub(crate) const RING_CAPACITY: usize = neoutl_media_api::VIDEO_TEXTURE_POOL_CAPACITY;
const _: () = assert!(RING_CAPACITY as i64 > PREFETCH_RADIUS * 2);
const DECODE_PREFETCH_FAIL_THRESHOLD: i64 = 30;

const SAFE_RING_CAPACITY: usize = {
    let radius_window = (PREFETCH_RADIUS as usize) * 2 + 2;
    if RING_CAPACITY < radius_window {
        RING_CAPACITY
    } else {
        radius_window
    }
};

const STOP_SENTINEL: i64 = i64::MIN + 1;
const NONE_SENTINEL: i64 = i64::MIN;

struct TextureStore {
    map: HashMap<i64, (wgpu::Texture, ColorMeta)>,
    order: VecDeque<i64>,
}

impl TextureStore {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn contains(&self, index: i64) -> bool {
        self.map.contains_key(&index)
    }

    fn touch(&mut self, index: i64) {
        self.order.retain(|&i| i != index);
        self.order.push_back(index);
    }

    fn get(&mut self, index: i64) -> Option<(wgpu::Texture, ColorMeta)> {
        let tex = self.map.get(&index).cloned();
        if tex.is_some() {
            self.touch(index);
        }
        tex
    }

    fn put(&mut self, index: i64, texture: wgpu::Texture, color_meta: ColorMeta) {
        if self.map.contains_key(&index) {
            self.touch(index);
            return;
        }
        self.map.insert(index, (texture, color_meta));
        self.order.push_back(index);
        while self.order.len() > SAFE_RING_CAPACITY {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
            }
        }
    }
}

enum DecodeRequest {
    PrefetchOnly(i64),
    Full(i64),
}

enum DecodeResponse {
    PrefetchDone(i64, Result<(), String>),
    FrameDone(i64, Result<(wgpu::Texture, ColorMeta), String>),
}

struct DecodeThreadHandle {
    req_tx: Option<mpsc::Sender<DecodeRequest>>,
    resp_rx: mpsc::Receiver<DecodeResponse>,
    join: Option<thread::JoinHandle<()>>,
    hung: bool,
}

impl DecodeThreadHandle {
    fn spawn(
        mut decoder: Box<dyn VideoSource>,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
    ) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<DecodeRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<DecodeResponse>();

        let join = thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                match req {
                    DecodeRequest::PrefetchOnly(index) => {
                        let result = decoder.prefetch(index);
                        if resp_tx
                            .send(DecodeResponse::PrefetchDone(index, result))
                            .is_err()
                        {
                            break;
                        }
                    }
                    DecodeRequest::Full(index) => {
                        let result = decoder
                            .prefetch(index)
                            .and_then(|()| decoder.frame_gpu(index, &device, &queue))
                            .map(|texture| (texture, decoder.last_color_meta()));
                        if resp_tx
                            .send(DecodeResponse::FrameDone(index, result))
                            .is_err()
                        {
                            eprintln!(
                                "[decode-worker] frame={} decode完了もwatchdogは既に諦めていた(遅延完了) thread={:?}",
                                index,
                                thread::current().id()
                            );
                        }
                    }
                }
            }
            eprintln!(
                "[decode-worker] decode thread終了 thread={:?}",
                thread::current().id()
            );
        });

        Self {
            req_tx: Some(req_tx),
            resp_rx,
            join: Some(join),
            hung: false,
        }
    }

    fn prefetch_only(&mut self, frame_index: i64) -> Result<(), String> {
        if self.hung {
            return Err(format!(
                "decoderはhung状態のためprefetch不可 (frame={frame_index})"
            ));
        }
        let Some(req_tx) = self.req_tx.as_ref() else {
            self.hung = true;
            return Err(format!("decode thread終了済み (frame={frame_index})"));
        };
        if req_tx
            .send(DecodeRequest::PrefetchOnly(frame_index))
            .is_err()
        {
            self.hung = true;
            return Err(format!("decode thread消失 (frame={frame_index})"));
        }
        match self.resp_rx.recv_timeout(DECODE_WATCHDOG_TIMEOUT) {
            Ok(DecodeResponse::PrefetchDone(got, result)) if got == frame_index => result,
            Ok(_) => {
                self.hung = true;
                Err(format!("decode thread応答不一致 (frame={frame_index})"))
            }
            Err(RecvTimeoutError::Timeout) => {
                self.hung = true;
                Err(format!(
                    "prefetch watchdog timeout (frame={frame_index}, timeout={DECODE_WATCHDOG_TIMEOUT:?})"
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.hung = true;
                Err(format!("decode threadとの接続が切断 (frame={frame_index})"))
            }
        }
    }

    fn frame_gpu_watched(
        &mut self,
        frame_index: i64,
    ) -> Result<(wgpu::Texture, ColorMeta), String> {
        if self.hung {
            return Err(format!(
                "decoderは既にwatchdogタイムアウトでhung状態 (frame={frame_index})"
            ));
        }
        let Some(req_tx) = self.req_tx.as_ref() else {
            self.hung = true;
            return Err(format!("decode thread終了済み (frame={frame_index})"));
        };
        if req_tx.send(DecodeRequest::Full(frame_index)).is_err() {
            self.hung = true;
            return Err(format!("decode thread消失 (frame={frame_index})"));
        }
        match self.resp_rx.recv_timeout(DECODE_WATCHDOG_TIMEOUT) {
            Ok(DecodeResponse::FrameDone(got, result)) if got == frame_index => result,
            Ok(_) => {
                self.hung = true;
                Err(format!(
                    "decode thread応答不一致 expected={frame_index} (プロトコル破壊、以後使用不可)"
                ))
            }
            Err(RecvTimeoutError::Timeout) => {
                self.hung = true;
                Err(format!(
                    "decode watchdog timeout (frame={frame_index}, timeout={DECODE_WATCHDOG_TIMEOUT:?})"
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.hung = true;
                Err(format!(
                    "decode watchdogスレッドとの接続が切断されました (frame={frame_index})"
                ))
            }
        }
    }
}

const HUNG_REAP_POLL_INTERVAL: Duration = Duration::from_millis(10);
const HUNG_REAP_DEADLINE: Duration = Duration::from_millis(100);

impl Drop for DecodeThreadHandle {
    /// hung=false: req_rx側ループはreq_tx破棄で即座にreturnするため無条件joinで安全。
    /// hung=true: OSスレッドはFFI呼び出し中で協調的に終了しない可能性がある。
    /// 無条件joinはDrop呼び出し元(tokioワーカー)を無期限にブロックしうるため、
    /// 有限時間ポーリングで完了を待ち、間に合わなければ回収を諦めJoinHandleを破棄する
    /// (OSスレッドはdetachされプロセス終了までリークするが、呼び出し元は停止しない)。
    fn drop(&mut self) {
        self.req_tx = None;
        let Some(join) = self.join.take() else {
            return;
        };
        if !self.hung {
            let _ = join.join();
            return;
        }
        let mut waited = Duration::ZERO;
        while !join.is_finished() && waited < HUNG_REAP_DEADLINE {
            thread::sleep(HUNG_REAP_POLL_INTERVAL);
            waited += HUNG_REAP_POLL_INTERVAL;
        }
        if join.is_finished() {
            let _ = join.join();
        } else {
            eprintln!(
                "[decode-worker] hungスレッド回収断念、リーク許容 thread={:?}",
                join.thread().id()
            );
        }
    }
}

pub struct DecodeWorker {
    generation: u64,
    requested: Arc<AtomicI64>,
    signal: Arc<(Mutex<bool>, Condvar)>,
    store: Arc<Mutex<TextureStore>>,
    last_error: Arc<Mutex<Option<String>>>,

    task: Option<tokio::task::JoinHandle<()>>,
    worker_token: u64,
}

impl DecodeWorker {
    pub fn spawn(
        generation: u64,
        decoder: Box<dyn VideoSource>,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        on_fail: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Self {
        let requested = Arc::new(AtomicI64::new(NONE_SENTINEL));
        let signal = Arc::new((Mutex::new(false), Condvar::new()));
        let store = Arc::new(Mutex::new(TextureStore::new()));
        let last_error = Arc::new(Mutex::new(None));
        let worker_token = NEXT_WORKER_TOKEN.fetch_add(1, Ordering::Relaxed);
        let exact_queue = Arc::new(Mutex::new(VecDeque::new()));

        let requested_t = requested.clone();
        let signal_t = signal.clone();
        let store_t = store.clone();
        let last_error_t = last_error.clone();
        let on_fail_t = on_fail.clone();
        let exact_queue_t = exact_queue.clone();

        let task = super::runtime::handle().spawn_blocking(move || {
            let _token_guard = ActiveWorkerTokenGuard::enter(worker_token);

            let total_frames_t = decoder.total_frames();
            let mut decode_thread = DecodeThreadHandle::spawn(decoder, device, queue);

            let mut served = NONE_SENTINEL;
            let mut direction: i64 = 1;
            let mut consecutive_target_fails: i64 = 0;

            macro_rules! produce {
                ($index:expr, $critical:expr) => {{
                    let index = $index;
                    let critical: bool = $critical;
                    let result = decode_thread.frame_gpu_watched(index);
                    let mut ok = true;
                    match result {
                        Ok((tex, color_meta)) => {
                            store_t.lock().unwrap().put(index, tex, color_meta);
                            *last_error_t.lock().unwrap() = None;
                            if critical {
                                consecutive_target_fails = 0;
                            }
                        }
                        Err(e) => {
                            let msg = format!("decode(frame={index}) failed: {e}");
                            eprintln!("[decode-worker] {msg}");
                            *last_error_t.lock().unwrap() = Some(msg.clone());
                            if decode_thread.hung {
                                on_fail_t(msg);
                                return;
                            }
                            if critical {
                                consecutive_target_fails += 1;
                                if consecutive_target_fails > DECODE_PREFETCH_FAIL_THRESHOLD {
                                    on_fail_t(msg);
                                    return;
                                }
                            }
                            ok = false;
                        }
                    }
                    ok
                }};
            }

            loop {
                let target = {
                    let (lock, cvar) = &*signal_t;
                    let mut pending = lock.lock().unwrap();
                    while !*pending {
                        pending = cvar.wait(pending).unwrap();
                    }
                    *pending = false;
                    requested_t.load(Ordering::Acquire)
                };

                loop {
                    let next_exact = exact_queue_t.lock().unwrap().pop_front();
                    let Some(index) = next_exact else {
                        break;
                    };
                    if store_t.lock().unwrap().contains(index) {
                        on_ready();
                        continue;
                    }
                    if produce!(index, false) {
                        on_ready();
                    }
                }

                if target == STOP_SENTINEL {
                    return;
                }
                if target == served {
                    continue;
                }

                if served != NONE_SENTINEL {
                    direction = if target > served { 1 } else { -1 };
                }

                let already_ready = store_t.lock().unwrap().contains(target);
                if already_ready {
                    served = target;
                    consecutive_target_fails = 0;
                } else if produce!(target, true) {
                    served = target;
                    on_ready();
                }

                let mut farthest_ahead: Option<i64> = None;
                for offset in 1..=PREFETCH_RADIUS {
                    let ahead = target + offset * direction;
                    if ahead < 0 || ahead >= total_frames_t {
                        break;
                    }
                    if !store_t.lock().unwrap().contains(ahead) {
                        farthest_ahead = Some(ahead);
                    }
                }
                if let Some(far) = farthest_ahead
                    && requested_t.load(Ordering::Acquire) == target
                    && let Err(e) = decode_thread.prefetch_only(far)
                {
                    eprintln!(
                        "{}",
                        t!(
                            "[decode-worker] speculative prefetch(frame=%{arg0}) failed: %{arg1}",
                            arg0 = format!("{}", far),
                            arg1 = e.to_string()
                        )
                    );
                    if decode_thread.hung {
                        on_fail_t(e);
                        return;
                    }
                }

                for offset in 1..=PREFETCH_RADIUS {
                    if requested_t.load(Ordering::Acquire) != target {
                        break;
                    }
                    let ahead = target + offset * direction;
                    if ahead < 0 || ahead >= total_frames_t {
                        break;
                    }
                    if store_t.lock().unwrap().contains(ahead) {
                        continue;
                    }
                    if produce!(ahead, false) {
                        on_ready();
                    }
                }
            }
        });

        Self {
            generation,
            requested,
            signal,
            store,
            last_error,
            task: Some(task),
            worker_token,
        }
    }

    pub fn request(&self, frame_index: i64) {
        self.requested.store(frame_index, Ordering::Release);
        let (lock, cvar) = &*self.signal;
        *lock.lock().unwrap() = true;
        cvar.notify_one();
    }

    pub fn poll_texture(&self, frame_index: i64) -> Option<wgpu::Texture> {
        self.store.lock().unwrap().get(frame_index).map(|(t, _)| t)
    }

    pub fn poll_color_meta(&self, frame_index: i64) -> Option<ColorMeta> {
        self.store.lock().unwrap().get(frame_index).map(|(_, m)| m)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn take_last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().take()
    }

    pub fn stop_and_join(mut self) {
        self.requested.store(STOP_SENTINEL, Ordering::Release);
        let (lock, cvar) = &*self.signal;
        *lock.lock().unwrap() = true;
        cvar.notify_one();

        if let Some(task) = self.task.take() {
            let is_self = ACTIVE_WORKER_TOKEN.with(|c| c.get()) == Some(self.worker_token);
            if is_self {
                eprintln!(
                    "[decode-worker] stop_and_join自スレッドから呼び出し、同期待機不可のためabortへ縮退"
                );
                task.abort();
                return;
            }
            super::runtime::handle().block_on(async move {
                let _ = task.await;
            });
        }
    }
}

impl Drop for DecodeWorker {
    fn drop(&mut self) {
        self.requested.store(STOP_SENTINEL, Ordering::Release);
        let (lock, cvar) = &*self.signal;
        *lock.lock().unwrap() = true;
        cvar.notify_one();

        if let Some(task) = self.task.take() {
            let is_self = ACTIVE_WORKER_TOKEN.with(|c| c.get()) == Some(self.worker_token);

            if is_self {
                task.abort();
                return;
            }

            super::runtime::handle().spawn(async move {
                let _ = task.await;
            });
        }
    }
}
