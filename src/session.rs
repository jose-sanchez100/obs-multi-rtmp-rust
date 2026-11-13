//! One session = a dedicated thread + a bounded queue + a transport. No
//! network operation ever happens on the thread that produces packets.

use crate::queue::{DropStats, Kind, MediaPacket, PacketQueue, QueueLimits};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Network abstraction: RTMP, RTMPS, or a test double.
pub trait Transport: Send {
    /// Connects and negotiates (handshake, connect, publish). Must have timeouts.
    fn connect(&mut self) -> io::Result<()>;
    /// Sends a packet. MUST finish in bounded time (write timeout).
    fn send(&mut self, pkt: &MediaPacket) -> io::Result<()>;
    /// Processes server control messages even when no packets are queued.
    fn poll(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn close(&mut self);
}

pub type TransportFactory = Box<dyn Fn() -> Box<dyn Transport> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionState {
    Idle = 0,
    Connecting = 1,
    Live = 2,
    Backoff = 3,
    Stopped = 4,
    /// Stopped by the user; the thread stays alive and can be resumed.
    Paused = 5,
}

impl SessionState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Connecting,
            2 => Self::Live,
            3 => Self::Backoff,
            4 => Self::Stopped,
            5 => Self::Paused,
            _ => Self::Idle,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub limits: QueueLimits,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// If a single send takes longer than this, it counts as a "stall".
    pub slow_send: Duration,
    /// Max consecutive retries before giving up (0 = unlimited).
    pub max_retries: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            limits: QueueLimits::default(),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            slow_send: Duration::from_millis(500),
            max_retries: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub state: SessionState,
    pub packets_sent: u64,
    pub bytes_sent: u64,
    /// Video frames sent (excluding headers); allows computing the actual FPS
    /// of each destination.
    pub video_frames_sent: u64,
    pub reconnects: u64,
    pub slow_sends: u64,
    pub queued_bytes: usize,
    pub drops: DropStats,
    pub last_error: Option<String>,
}

struct Shared {
    queue: PacketQueue,
    stop: AtomicBool,
    /// Manual per-destination control.
    paused: AtomicBool,
    restart: AtomicBool,
    state: AtomicU8,
    packets: AtomicU64,
    bytes: AtomicU64,
    video_frames: AtomicU64,
    reconnects: AtomicU64,
    slow: AtomicU64,
    last_error: Mutex<Option<String>>,
}

pub struct Session {
    pub id: u64,
    shared: Arc<Shared>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

fn jitter(base: Duration) -> Duration {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    base + Duration::from_millis(n % 250)
}

impl Session {
    pub fn spawn(id: u64, cfg: SessionConfig, factory: TransportFactory) -> Arc<Session> {
        let shared = Arc::new(Shared {
            queue: PacketQueue::new(cfg.limits),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            restart: AtomicBool::new(false),
            state: AtomicU8::new(SessionState::Idle as u8),
            packets: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            video_frames: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
            slow: AtomicU64::new(0),
            last_error: Mutex::new(None),
        });
        let w = Worker { sh: shared.clone(), cfg };
        let h = thread::Builder::new()
            .name(format!("rtmp-session-{id}"))
            .spawn(move || w.run(factory))
            .expect("spawn session thread");
        Arc::new(Session { id, shared, handle: Mutex::new(Some(h)) })
    }

    /// Hot path called from the encoder thread. No network I/O.
    #[inline]
    pub fn push(&self, pkt: MediaPacket) -> bool {
        self.shared.queue.push(pkt)
    }

    pub fn start_at_keyframe(&self) {
        self.shared.queue.wait_for_keyframe();
    }

    /// Does not wait for the thread: a slow network cannot delay the caller.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.queue.close();
    }

    /// Drops the connection and stops sending without destroying the session.
    pub fn pause(&self) {
        self.shared.queue.set_paused(true);
        self.shared.paused.store(true, Ordering::SeqCst);
    }

    /// Reconnects and resumes at the next keyframe, with the same timestamps
    /// as the other sessions (identical redundancy preserved).
    pub fn resume(&self) {
        self.shared.queue.set_paused(false);
        self.shared.paused.store(false, Ordering::SeqCst);
    }

    /// Closes the current connection and reconnects immediately (no backoff).
    /// If the session was paused, it is resumed as well.
    pub fn restart(&self) {
        self.shared.restart.store(true, Ordering::SeqCst);
        if self.shared.paused.load(Ordering::SeqCst) {
            self.resume();
        }
    }

    pub fn join(&self) {
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        let sh = &self.shared;
        let (drops, queued) = sh.queue.stats();
        SessionSnapshot {
            state: SessionState::from_u8(sh.state.load(Ordering::Relaxed)),
            packets_sent: sh.packets.load(Ordering::Relaxed),
            bytes_sent: sh.bytes.load(Ordering::Relaxed),
            video_frames_sent: sh.video_frames.load(Ordering::Relaxed),
            reconnects: sh.reconnects.load(Ordering::Relaxed),
            slow_sends: sh.slow.load(Ordering::Relaxed),
            queued_bytes: queued,
            drops,
            last_error: sh.last_error.lock().unwrap().clone(),
        }
    }
}

struct Worker {
    sh: Arc<Shared>,
    cfg: SessionConfig,
}

impl Worker {
    fn stopping(&self) -> bool {
        self.sh.stop.load(Ordering::Relaxed)
    }

    fn paused(&self) -> bool {
        self.sh.paused.load(Ordering::SeqCst)
    }

    /// The current connection must be abandoned (stop, pause, or restart requested).
    fn interrupted(&self) -> bool {
        self.stopping() || self.paused() || self.sh.restart.load(Ordering::SeqCst)
    }

    fn set(&self, s: SessionState) {
        self.sh.state.store(s as u8, Ordering::Relaxed);
    }

    fn fail(&self, e: &io::Error) {
        log::warn!("session: {e}");
        *self.sh.last_error.lock().unwrap() = Some(e.to_string());
    }

    fn sleep_interruptible(&self, d: Duration) {
        let end = Instant::now() + d;
        while !self.interrupted() && Instant::now() < end {
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn run(self, factory: TransportFactory) {
        let mut backoff = self.cfg.backoff_min;
        let mut retries = 0u32;
        let mut first = true;
        // Reconnect requested by the user (resume/restart): does not count as a failure.
        let mut manual = false;

        while !self.stopping() {
            if self.paused() {
                self.set(SessionState::Paused);
                while self.paused() && !self.stopping() {
                    thread::sleep(Duration::from_millis(50));
                }
                if self.stopping() {
                    break;
                }
                manual = true;
            }
            if self.sh.restart.swap(false, Ordering::SeqCst) {
                manual = true;
            }
            if manual {
                backoff = self.cfg.backoff_min;
                retries = 0;
            }

            self.set(SessionState::Connecting);
            let mut t = factory();
            match t.connect() {
                Ok(()) if self.interrupted() => {} // pause or restart during connect
                Ok(()) => {
                    // First connection: the queue already starts with headers + IDR, keep it.
                    // Reconnect: old data is useless; restart at the next IDR, which the
                    // queue precedes with the headers stamped with its timestamp.
                    if !first || manual {
                        if !manual {
                            self.sh.reconnects.fetch_add(1, Ordering::Relaxed);
                        }
                        self.sh.queue.reset_for_reconnect();
                    }
                    first = false;
                    manual = false;
                    self.set(SessionState::Live);
                    let started = Instant::now();
                    self.pump(&mut *t);
                    // A stable session resets the backoff.
                    if started.elapsed() > Duration::from_secs(20) {
                        backoff = self.cfg.backoff_min;
                        retries = 0;
                    }
                }
                Err(e) => self.fail(&e),
            }
            t.close();
            if self.stopping() {
                break;
            }
            if self.paused() || self.sh.restart.load(Ordering::SeqCst) {
                continue; // manual action: no backoff
            }
            retries += 1;
            if self.cfg.max_retries != 0 && retries > self.cfg.max_retries {
                *self.sh.last_error.lock().unwrap() = Some("max retries reached".into());
                break;
            }
            self.set(SessionState::Backoff);
            self.sleep_interruptible(jitter(backoff));
            backoff = (backoff * 2).min(self.cfg.backoff_max);
        }
        self.set(SessionState::Stopped);
    }

    fn pump(&self, t: &mut dyn Transport) {
        while !self.interrupted() {
            let Some(pkt) = self.sh.queue.pop_timeout(Duration::from_millis(100)) else {
                if !self.interrupted() {
                    if let Err(e) = t.poll() {
                        self.fail(&e);
                        return;
                    }
                }
                continue;
            };
            let t0 = Instant::now();
            match t.send(&pkt) {
                Ok(()) => {
                    self.sh.packets.fetch_add(1, Ordering::Relaxed);
                    self.sh.bytes.fetch_add(pkt.data.len() as u64, Ordering::Relaxed);
                    if pkt.kind == Kind::Video && !pkt.is_header {
                        self.sh.video_frames.fetch_add(1, Ordering::Relaxed);
                    }
                    if t0.elapsed() > self.cfg.slow_send {
                        self.sh.slow.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    self.fail(&e);
                    return;
                }
            }
        }
    }
}
