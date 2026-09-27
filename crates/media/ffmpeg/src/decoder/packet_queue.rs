use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use ffmpeg_sys_next as sys;

pub(crate) struct PacketSlot {
    pub(crate) packet: *mut sys::AVPacket,
    pub(crate) eof: bool,
}

unsafe impl Send for PacketSlot {}

impl Drop for PacketSlot {
    fn drop(&mut self) {
        unsafe {
            if !self.packet.is_null() {
                sys::av_packet_free(&mut self.packet);
            }
        }
    }
}

/// 容量1のリングバッファ。単一生産者(packet_reader_loop)・単一消費者
/// (decode_task)が前提。`sync_channel(1)`が停止/満杯待ちを提供するため、
/// 独自のCondvarポーリングは持たない。
pub(crate) struct PacketQueue {
    sender: Mutex<mpsc::SyncSender<PacketSlot>>,
    receiver: Mutex<mpsc::Receiver<PacketSlot>>,
}

impl PacketQueue {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        Self {
            sender: Mutex::new(sender),
            receiver: Mutex::new(receiver),
        }
    }

    pub(crate) fn push_blocking(&self, item: PacketSlot, stop: &AtomicBool) -> bool {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let sender = self.sender.lock().expect("packet queue sender poisoned");
        sender.send(item).is_ok()
    }

    pub(crate) fn pop_blocking(&self, stop: &AtomicBool) -> Option<PacketSlot> {
        if stop.load(Ordering::Acquire) {
            return None;
        }
        let receiver = self
            .receiver
            .lock()
            .expect("packet queue receiver poisoned");
        receiver.recv().ok()
    }

    pub(crate) fn flush(&self) {
        let receiver = self
            .receiver
            .lock()
            .expect("packet queue receiver poisoned");
        while receiver.try_recv().is_ok() {}
    }
}

pub(crate) struct SeekLock(pub(crate) Mutex<()>);

impl SeekLock {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(()))
    }
}

pub(crate) struct SendPtr(pub(crate) *mut sys::AVFormatContext);
unsafe impl Send for SendPtr {}

pub(crate) fn packet_reader_loop(
    fmt_ctx: *mut sys::AVFormatContext,
    stream_index: i32,
    queue: Arc<PacketQueue>,
    seek_lock: Arc<SeekLock>,
    stop: Arc<AtomicBool>,
) {
    let fmt_ctx = SendPtr(fmt_ctx);
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let pkt = unsafe { sys::av_packet_alloc() };
        let read_ret = {
            let _guard = seek_lock.0.lock().expect("seek lock poisoned");
            unsafe { sys::av_read_frame(fmt_ctx.0, pkt) }
        };
        if read_ret < 0 {
            unsafe { sys::av_packet_free(&mut { pkt }) };
            let pushed = queue.push_blocking(
                PacketSlot {
                    packet: ptr::null_mut(),
                    eof: true,
                },
                &stop,
            );
            if !pushed {
                return;
            }
            continue;
        }
        if unsafe { (*pkt).stream_index } != stream_index {
            unsafe { sys::av_packet_free(&mut { pkt }) };
            continue;
        }
        let pushed = queue.push_blocking(
            PacketSlot {
                packet: pkt,
                eof: false,
            },
            &stop,
        );
        if !pushed {
            unsafe { sys::av_packet_free(&mut { pkt }) };
            return;
        }
    }
}
