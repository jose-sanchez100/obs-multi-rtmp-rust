use bytes::Bytes;
use obs_rtmp_sessions::session::{SessionConfig, Transport};
use obs_rtmp_sessions::{Kind, MediaPacket, QueueLimits, SessionManager, SessionState};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Mock {
    delay: Duration,
    sent: Arc<AtomicU64>,
    fail_connect_times: Arc<AtomicU64>,
}

impl Transport for Mock {
    fn connect(&mut self) -> io::Result<()> {
        if self.fail_connect_times.load(Ordering::SeqCst) > 0 {
            self.fail_connect_times.fetch_sub(1, Ordering::SeqCst);
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "mock refused"));
        }
        Ok(())
    }
    fn send(&mut self, _p: &MediaPacket) -> io::Result<()> {
        std::thread::sleep(self.delay);
        self.sent.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn close(&mut self) {}
}

fn video(dts: u32, key: bool) -> MediaPacket {
    MediaPacket { kind: Kind::Video, dts_ms: dts, keyframe: key, is_header: false, data: Bytes::from(vec![1u8; if key { 40_000 } else { 8_000 }]) }
}
fn audio(dts: u32) -> MediaPacket {
    MediaPacket { kind: Kind::Audio, dts_ms: dts, keyframe: false, is_header: false, data: Bytes::from(vec![2u8; 400]) }
}

fn cfg() -> SessionConfig {
    SessionConfig {
        limits: QueueLimits { soft_bytes: 300_000, hard_bytes: 800_000, ..Default::default() },
        backoff_min: Duration::from_millis(100),
        backoff_max: Duration::from_millis(300),
        ..Default::default()
    }
}

#[test]
fn slow_destination_does_not_affect_producer_or_healthy_destination() {
    let mgr = SessionManager::new();
    let fast_sent = Arc::new(AtomicU64::new(0));
    let slow_sent = Arc::new(AtomicU64::new(0));
    let (fs, ss) = (fast_sent.clone(), slow_sent.clone());

    let fast = mgr.add(cfg(), Box::new(move || {
        Box::new(Mock { delay: Duration::ZERO, sent: fs.clone(), fail_connect_times: Arc::new(AtomicU64::new(0)) })
    }));
    // ~25 packets/s max versus ~90/s produced: sustained congestion
    let slow = mgr.add(cfg(), Box::new(move || {
        Box::new(Mock { delay: Duration::from_millis(40), sent: ss.clone(), fail_connect_times: Arc::new(AtomicU64::new(0)) })
    }));
    std::thread::sleep(Duration::from_millis(200));

    // 10 simulated seconds of 30 fps + audio, pumped as fast as possible.
    let t0 = Instant::now();
    let mut produced = 0u64;
    for frame in 0..300u32 {
        let dts = frame * 33;
        mgr.push_all(&video(dts, frame % 30 == 0));
        mgr.push_all(&audio(dts));
        produced += 2;
        if frame % 3 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let push_time = t0.elapsed();
    std::thread::sleep(Duration::from_millis(800));

    let f = mgr.snapshot(fast).unwrap();
    let s = mgr.snapshot(slow).unwrap();
    println!("producer: {produced} pkts in {push_time:?}\nfast: {f:?}\nslow: {s:?}");

    assert!(push_time < Duration::from_millis(900), "producer blocked: {push_time:?}");
    assert_eq!(f.packets_sent, produced, "healthy destination lost packets");
    assert_eq!(f.video_frames_sent, 300, "per-destination video frame count");
    assert!(s.video_frames_sent < 300);
    assert_eq!(f.drops.dropped_video_frames, 0);
    assert!(s.drops.dropped_video_frames > 0, "slow destination should have dropped frames");
    assert!(s.queued_bytes <= 800_000, "slow destination's queue exceeded its limit");
    assert!(s.packets_sent < produced);
    mgr.shutdown();
}

#[test]
fn reconnects_with_backoff_and_remove_does_not_block() {
    let mgr = SessionManager::new();
    let sent = Arc::new(AtomicU64::new(0));
    let fails = Arc::new(AtomicU64::new(2));
    let (s2, f2) = (sent.clone(), fails.clone());
    let id = mgr.add(cfg(), Box::new(move || {
        Box::new(Mock { delay: Duration::ZERO, sent: s2.clone(), fail_connect_times: f2.clone() })
    }));

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && mgr.snapshot(id).unwrap().state != SessionState::Live {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(mgr.snapshot(id).unwrap().state, SessionState::Live);
    assert!(mgr.snapshot(id).unwrap().last_error.is_some());

    mgr.push_to(id, video(0, true));
    std::thread::sleep(Duration::from_millis(200));
    assert!(sent.load(Ordering::SeqCst) >= 1);

    let t = Instant::now();
    assert!(mgr.remove(id));
    assert!(t.elapsed() < Duration::from_millis(50));
    mgr.shutdown();
}
