//! Reconnect on sustained congestion: a destination that keeps dropping media
//! reconnects by itself, with a longer window each time; a healthy one never does.
use bytes::Bytes;
use obs_rtmp_sessions::session::{SessionConfig, Transport};
use obs_rtmp_sessions::{Kind, MediaPacket, QueueLimits, SessionManager, SessionState};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Sends each packet after `delay`; counts connections.
struct Mock {
    delay: Duration,
    connects: Arc<AtomicU64>,
}

impl Transport for Mock {
    fn connect(&mut self) -> io::Result<()> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn send(&mut self, _p: &MediaPacket) -> io::Result<()> {
        std::thread::sleep(self.delay);
        Ok(())
    }
    fn close(&mut self) {}
}

fn add(mgr: &SessionManager, delay: Duration, window: Duration) -> (u64, Arc<AtomicU64>) {
    let connects = Arc::new(AtomicU64::new(0));
    let c = connects.clone();
    let cfg = SessionConfig {
        limits: QueueLimits { soft_bytes: 100_000, hard_bytes: 300_000, max_latency_ms: 1000 },
        congestion_window: window,
        label: format!("test {delay:?}"),
        ..Default::default()
    };
    let id = mgr.add(cfg, Box::new(move || Box::new(Mock { delay, connects: c.clone() })));
    (id, connects)
}

/// Real-time 30 fps video (keyframe every second) plus audio, for `secs` seconds.
fn produce(mgr: &SessionManager, secs: u32) {
    let start = Instant::now();
    for frame in 0..secs * 30 {
        let dts = frame * 1000 / 30;
        let key = frame % 30 == 0;
        let size = if key { 30_000 } else { 6_000 };
        mgr.push_all(&MediaPacket { kind: Kind::Video, dts_ms: dts, keyframe: key, is_header: false, data: Bytes::from(vec![1u8; size]) });
        mgr.push_all(&MediaPacket { kind: Kind::Audio, dts_ms: dts, keyframe: false, is_header: false, data: Bytes::from(vec![2u8; 300]) });
        let due = start + Duration::from_millis(dts as u64);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
}

#[test]
fn congested_destination_reconnects_and_healthy_one_does_not() {
    let _ = env_logger::builder().is_test(true).try_init();
    let mgr = SessionManager::new();
    let (healthy, healthy_connects) = add(&mgr, Duration::ZERO, Duration::from_secs(1));
    // About 20 packets/s against 60/s produced: most media is dropped.
    let (slow, slow_connects) = add(&mgr, Duration::from_millis(50), Duration::from_secs(1));
    // Same congestion with the reconnect disabled.
    let (off, off_connects) = add(&mgr, Duration::from_millis(50), Duration::ZERO);

    produce(&mgr, 8);

    let h = mgr.snapshot(healthy).unwrap();
    let s = mgr.snapshot(slow).unwrap();
    let o = mgr.snapshot(off).unwrap();
    println!("healthy: {h:?}\nslow: {s:?}\noff: {o:?}");

    assert_eq!(h.congestion_reconnects, 0);
    assert_eq!(h.reconnects, 0);
    assert_eq!(healthy_connects.load(Ordering::SeqCst), 1);
    assert_eq!(h.drops.dropped_bytes, 0);
    assert!(h.recent_drop_pct < 1.0);

    // Windows of 1, 2 and 4 s (doubling after each reconnect), plus the time to the
    // next keyframe after each reconnect: 2 or 3 reconnects in 8 s, never one per second.
    assert!(
        (2..=3).contains(&s.congestion_reconnects),
        "congestion reconnects: {}",
        s.congestion_reconnects
    );
    assert_eq!(s.reconnects, s.congestion_reconnects);
    assert_eq!(slow_connects.load(Ordering::SeqCst), 1 + s.congestion_reconnects);
    assert!(s.last_error.as_deref().unwrap_or("").contains("congestion"), "{:?}", s.last_error);
    assert_eq!(s.state, SessionState::Live);

    assert_eq!(o.congestion_reconnects, 0);
    assert_eq!(off_connects.load(Ordering::SeqCst), 1);
    assert!(o.recent_drop_pct > 25.0, "recent drops: {}", o.recent_drop_pct);
    assert!(o.drops.dropped_bytes > 0 && o.drops.dropped_bytes < o.drops.offered_bytes);

    // Turning the option on while live applies to the current connection, and
    // turning it off stops further reconnects.
    assert!(mgr.set_congestion_window(off, Duration::from_secs(1)));
    assert!(mgr.set_congestion_window(slow, Duration::ZERO));
    let slow_before = mgr.snapshot(slow).unwrap().congestion_reconnects;
    produce(&mgr, 3);
    assert!(mgr.snapshot(off).unwrap().congestion_reconnects >= 1);
    assert_eq!(mgr.snapshot(slow).unwrap().congestion_reconnects, slow_before);
    mgr.shutdown();
}
