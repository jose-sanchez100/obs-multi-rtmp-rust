//! One session = a dedicated thread + a bounded queue + a transport. No
//! network operation ever happens on the thread that produces packets.

use crate::queue::{DropStats, Kind, MediaPacket, PacketQueue, QueueLimits};
use crate::tcpinfo::TcpStats;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
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
    /// Drops the connection without ending the broadcast, so the platform waits
    /// for the stream to come back (reconnects, pause, restart, errors).
    fn close(&mut self);
    /// Ends the broadcast and then closes; used only when the session is stopped,
    /// so the platform finishes the live event instead of waiting for a reconnect.
    fn finish(&mut self) {
        self.close()
    }
    /// Addresses of the current connection, once connected.
    fn connection_info(&self) -> Option<ConnectionInfo> {
        None
    }
    /// Kernel TCP statistics of the current connection.
    fn tcp_stats(&self) -> Option<TcpStats> {
        None
    }
}

/// Where a connection went: the ingest host name, the address it resolved to,
/// and the local address the OS picked (which shows the interface or uplink).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub host: String,
    pub remote: SocketAddr,
    pub local: SocketAddr,
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
    /// Name used in log messages (e.g. "YouTube / Live"); empty = "session <id>".
    pub label: String,
    /// Reconnect when the session has dropped at least `congestion_drop_ratio` of
    /// its media (by bytes) over this long, measured from the start of the
    /// connection. Each such reconnect within 10 minutes of the previous one
    /// doubles the window, up to 8 times. Zero disables it. Whole seconds;
    /// can be changed while running with `Session::set_congestion_window`.
    pub congestion_window: Duration,
    /// Share of media dropped (0..1) that counts as congestion, for the reconnect
    /// and for the "congested" / "cleared" log messages.
    pub congestion_drop_ratio: f64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            limits: QueueLimits::default(),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            slow_send: Duration::from_millis(500),
            max_retries: 0,
            label: String::new(),
            congestion_window: Duration::ZERO,
            congestion_drop_ratio: 0.25,
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
    /// Reconnects triggered by sustained congestion (also counted in `reconnects`).
    pub congestion_reconnects: u64,
    /// The current connection, or the last one.
    pub connection: Option<ConnectionInfo>,
    /// Smoothed TCP round-trip time of the current connection.
    pub rtt_ms: Option<f64>,
    /// Share of TCP data retransmitted over the last 10 s, in percent.
    pub retrans_pct: Option<f64>,
    /// Share of media dropped over the last 10 s, in percent.
    pub recent_drop_pct: f64,
}

/// Connection details refreshed once per second while live.
#[derive(Clone, Debug, Default)]
struct LiveInfo {
    connection: Option<ConnectionInfo>,
    rtt_ms: Option<f64>,
    retrans_pct: Option<f64>,
    recent_drop_pct: f64,
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
    congestion_reconnects: AtomicU64,
    /// Current `congestion_window`, in seconds (0 = off).
    congestion_window_s: AtomicU64,
    last_error: Mutex<Option<String>>,
    live: Mutex<LiveInfo>,
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
            congestion_reconnects: AtomicU64::new(0),
            congestion_window_s: AtomicU64::new(cfg.congestion_window.as_secs()),
            last_error: Mutex::new(None),
            live: Mutex::new(LiveInfo::default()),
        });
        let label = if cfg.label.is_empty() { format!("session {id}") } else { cfg.label.clone() };
        let w = Worker { sh: shared.clone(), cfg, label };
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

    /// Changes (or turns off, with zero) the reconnect on congestion; takes
    /// effect within a second, also on the current connection.
    pub fn set_congestion_window(&self, window: Duration) {
        self.shared.congestion_window_s.store(window.as_secs(), Ordering::Relaxed);
    }

    pub fn join(&self) {
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        let sh = &self.shared;
        let (drops, queued) = sh.queue.stats();
        let live = sh.live.lock().unwrap().clone();
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
            congestion_reconnects: sh.congestion_reconnects.load(Ordering::Relaxed),
            connection: live.connection,
            rtt_ms: live.rtt_ms,
            retrans_pct: live.retrans_pct,
            recent_drop_pct: live.recent_drop_pct,
        }
    }
}

/// Seconds of history behind the "recent" figures (drops, retransmissions).
const RECENT_S: usize = 10;
/// How often a live connection writes a summary line to the log.
const LOG_PERIOD: Duration = Duration::from_secs(300);
/// A congestion reconnect this soon after the previous one doubles the window.
const CONGESTION_MEMORY: Duration = Duration::from_secs(600);
const MAX_CONGESTION_LEVEL: u32 = 3;

/// Cumulative counters of a session at one instant.
#[derive(Clone, Copy, Debug)]
struct Sample {
    offered: u64,
    dropped: u64,
    sent: u64,
    tcp: Option<TcpStats>,
}

/// What happened between two samples.
struct Delta {
    secs: f64,
    drop_ratio: f64,
    mbps: f64,
    retrans_pct: Option<f64>,
}

impl Delta {
    fn between(a: &Sample, b: &Sample, secs: f64) -> Self {
        let offered = b.offered.saturating_sub(a.offered);
        let dropped = b.dropped.saturating_sub(a.dropped);
        let retrans_pct = match (a.tcp, b.tcp) {
            (Some(x), Some(y)) if y.sent > x.sent => {
                Some(y.retransmitted.saturating_sub(x.retransmitted) as f64 * 100.0 / (y.sent - x.sent) as f64)
            }
            (Some(_), Some(_)) => Some(0.0),
            _ => None,
        };
        Self {
            secs,
            drop_ratio: if offered == 0 { 0.0 } else { dropped as f64 / offered as f64 },
            mbps: if secs > 0.0 { b.sent.saturating_sub(a.sent) as f64 * 8.0 / secs / 1e6 } else { 0.0 },
            retrans_pct,
        }
    }

    fn describe(&self, rtt_ms: Option<f64>) -> String {
        let mut s = format!("sending {:.2} Mbps, dropped {:.0}% of media", self.mbps, self.drop_ratio * 100.0);
        if let Some(rtt) = rtt_ms {
            s += &format!(", rtt {rtt:.0} ms");
        }
        if let Some(r) = self.retrans_pct {
            s += &format!(", retransmitted {r:.1}%");
        }
        s
    }
}

fn hms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Watches one live connection: per-second samples for the congestion check,
/// the dock's recent figures, and the log messages.
struct Monitor {
    /// Last samples, one per second, oldest first.
    samples: VecDeque<Sample>,
    /// Doublings of the reconnect window after repeated congestion reconnects.
    level: u32,
    started: Instant,
    first: Sample,
    period_at: Instant,
    period: Sample,
    congested: bool,
}

impl Monitor {
    fn new(level: u32, first: Sample) -> Self {
        let mut samples = VecDeque::with_capacity(RECENT_S + 1);
        samples.push_back(first);
        let now = Instant::now();
        Self { samples, level, started: now, first, period_at: now, period: first, congested: false }
    }

    /// Change over the last `secs` samples (fewer if the connection is younger).
    fn last(&self, secs: usize) -> Delta {
        let n = secs.min(self.samples.len() - 1);
        let b = self.samples.back().unwrap();
        Delta::between(&self.samples[self.samples.len() - 1 - n], b, n as f64)
    }
}

/// Why `pump` returned.
enum PumpEnd {
    /// Stop, pause or restart requested.
    Interrupted,
    /// Network or protocol error (already recorded).
    Failed,
    /// Sustained congestion: reconnect at once.
    Congested(String),
}

struct Worker {
    sh: Arc<Shared>,
    cfg: SessionConfig,
    label: String,
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
        log::warn!("{}: {e}", self.label);
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
        let mut congestion_level = 0u32;
        let mut last_congestion: Option<Instant> = None;

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
            let connect_started = Instant::now();
            let mut congested = false;
            match t.connect() {
                Ok(()) if self.interrupted() => {} // pause or restart during connect
                Ok(()) => {
                    self.connected(&*t, connect_started.elapsed());
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
                    let (end, summary) = self.pump(&mut *t, congestion_level);
                    if let PumpEnd::Congested(reason) = end {
                        let now = Instant::now();
                        congestion_level = match last_congestion {
                            Some(prev) if now - prev < CONGESTION_MEMORY => {
                                (congestion_level + 1).min(MAX_CONGESTION_LEVEL)
                            }
                            _ => 0,
                        };
                        last_congestion = Some(now);
                        self.sh.congestion_reconnects.fetch_add(1, Ordering::Relaxed);
                        log::warn!(
                            "{}: reconnecting because of congestion: {reason}; next check after {} s",
                            self.label,
                            self.window_s(congestion_level)
                        );
                        *self.sh.last_error.lock().unwrap() =
                            Some(format!("reconnected because of congestion: {reason}"));
                        congested = true;
                    }
                    log::info!("{}: connection closed after {summary}", self.label);
                    // A stable session resets the backoff.
                    if started.elapsed() > Duration::from_secs(20) {
                        backoff = self.cfg.backoff_min;
                        retries = 0;
                    }
                }
                Err(e) => self.fail(&e),
            }
            if self.stopping() {
                t.finish();
                break;
            }
            t.close();
            if congested || self.paused() || self.sh.restart.load(Ordering::SeqCst) {
                continue; // congestion or manual action: reconnect without backoff
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

    /// Records and logs where a new connection went.
    fn connected(&self, t: &dyn Transport, took: Duration) {
        let info = t.connection_info();
        match &info {
            Some(c) => log::info!(
                "{}: connected to {} ({}) from {} in {} ms",
                self.label,
                c.host,
                c.remote,
                c.local,
                took.as_millis()
            ),
            None => log::info!("{}: connected in {} ms", self.label, took.as_millis()),
        }
        *self.sh.live.lock().unwrap() = LiveInfo { connection: info, ..Default::default() };
    }

    /// Seconds of congestion that trigger a reconnect at this level (0 = off).
    fn window_s(&self, level: u32) -> usize {
        (self.sh.congestion_window_s.load(Ordering::Relaxed) as usize) << level
    }

    fn sample(&self, t: &dyn Transport) -> Sample {
        let (drops, _) = self.sh.queue.stats();
        Sample {
            offered: drops.offered_bytes,
            dropped: drops.dropped_bytes,
            sent: self.sh.bytes.load(Ordering::Relaxed),
            tcp: t.tcp_stats(),
        }
    }

    /// Once per second: refreshes the recent figures, logs congestion changes and
    /// periodic summaries, and decides whether to reconnect.
    fn tick(&self, t: &dyn Transport, m: &mut Monitor) -> Option<String> {
        let now_sample = self.sample(t);
        let window_s = self.window_s(m.level);
        while m.samples.len() > window_s.max(RECENT_S) {
            m.samples.pop_front();
        }
        m.samples.push_back(now_sample);
        let rtt_ms = now_sample.tcp.map(|s| s.rtt_us as f64 / 1000.0);
        let recent = m.last(RECENT_S);
        {
            let mut live = self.sh.live.lock().unwrap();
            live.rtt_ms = rtt_ms;
            live.retrans_pct = recent.retrans_pct;
            live.recent_drop_pct = recent.drop_ratio * 100.0;
        }

        let threshold = self.cfg.congestion_drop_ratio;
        if recent.secs as usize >= RECENT_S {
            if !m.congested && recent.drop_ratio >= threshold {
                m.congested = true;
                log::warn!("{}: congested over the last {} s: {}", self.label, RECENT_S, recent.describe(rtt_ms));
            } else if m.congested && recent.drop_ratio < threshold / 2.0 {
                m.congested = false;
                log::info!("{}: congestion cleared: {}", self.label, recent.describe(rtt_ms));
            }
        }
        if m.period_at.elapsed() >= LOG_PERIOD {
            let d = Delta::between(&m.period, &now_sample, m.period_at.elapsed().as_secs_f64());
            log::info!(
                "{}: live {}, last {} min: {}",
                self.label,
                hms(m.started.elapsed()),
                LOG_PERIOD.as_secs() / 60,
                d.describe(rtt_ms)
            );
            m.period = now_sample;
            m.period_at = Instant::now();
        }

        if window_s > 0 && m.samples.len() > window_s {
            let d = m.last(window_s);
            if d.drop_ratio >= threshold {
                return Some(format!("{} over the last {} s", d.describe(rtt_ms), window_s));
            }
        }
        None
    }

    /// Sends until interrupted, failed or congested. Also returns a summary of
    /// the connection for the log.
    fn pump(&self, t: &mut dyn Transport, congestion_level: u32) -> (PumpEnd, String) {
        let mut m = Monitor::new(congestion_level, self.sample(t));
        let mut next_tick = Instant::now() + Duration::from_secs(1);
        let end = loop {
            if self.interrupted() {
                break PumpEnd::Interrupted;
            }
            if Instant::now() >= next_tick {
                next_tick = (next_tick + Duration::from_secs(1)).max(Instant::now());
                if let Some(reason) = self.tick(t, &mut m) {
                    break PumpEnd::Congested(reason);
                }
            }
            let Some(pkt) = self.sh.queue.pop_timeout(Duration::from_millis(100)) else {
                if !self.interrupted() {
                    if let Err(e) = t.poll() {
                        self.fail(&e);
                        break PumpEnd::Failed;
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
                    break PumpEnd::Failed;
                }
            }
        };
        let last = self.sample(t);
        let d = Delta::between(&m.first, &last, m.started.elapsed().as_secs_f64());
        (end, format!("{}: {}", hms(m.started.elapsed()), d.describe(None)))
    }
}
