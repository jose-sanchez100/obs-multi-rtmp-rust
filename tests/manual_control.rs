//! Manual per-session control (YouTube primary / backup): pausing, resuming and
//! restarting one session does not affect the other, and on return it rejoins at headers + IDR.
use bytes::Bytes;
use obs_rtmp_sessions::session::{SessionConfig, Transport};
use obs_rtmp_sessions::{Kind, MediaPacket, SessionManager, SessionState};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
struct Log {
    connects: Arc<AtomicU64>,
    /// (kind, is_header, keyframe, dts) of each sent packet
    sent: Arc<Mutex<Vec<(Kind, bool, bool, u32)>>>,
}

struct Mock(Log);

impl Transport for Mock {
    fn connect(&mut self) -> io::Result<()> {
        self.0.connects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn send(&mut self, p: &MediaPacket) -> io::Result<()> {
        self.0.sent.lock().unwrap().push((p.kind, p.is_header, p.keyframe, p.dts_ms));
        Ok(())
    }
    fn close(&mut self) {}
}

fn pkt(kind: Kind, dts: u32, key: bool, header: bool) -> MediaPacket {
    MediaPacket { kind, dts_ms: dts, keyframe: key, is_header: header, data: Bytes::from(vec![7u8; 500]) }
}

fn wait_state(mgr: &SessionManager, id: u64, want: SessionState) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if mgr.snapshot(id).unwrap().state == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("session {id} did not reach {want:?}: {:?}", mgr.snapshot(id).unwrap().state);
}

/// Pushes `n` video frames (IDR every 10) + audio starting at `*dts`.
fn feed(mgr: &SessionManager, dts: &mut u32, n: u32) {
    for _ in 0..n {
        let key = (*dts / 33) % 10 == 0;
        mgr.push_all(&pkt(Kind::Video, *dts, key, false));
        mgr.push_all(&pkt(Kind::Audio, *dts, false, false));
        *dts += 33;
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(150));
}

/// After index `from`, the first things sent must be headers, then an IDR, all with
/// the IDR's timestamp; timestamps never go backwards after that (a jump from 0 to
/// the current stream time is rejected by YouTube as "faster than real time").
fn assert_rejoins_at_keyframe(log: &Log, from: usize) {
    let sent = log.sent.lock().unwrap();
    let tail = &sent[from..];
    assert!(tail.len() >= 3, "nothing sent after resuming: {tail:?}");
    assert!(tail[0].1 && tail[1].1, "headers missing on resume: {:?}", &tail[..3]);
    let first_video = tail[2];
    assert!(first_video.0 == Kind::Video && first_video.2 && !first_video.1, "did not resume on an IDR: {:?}", &tail[..3]);
    assert!(first_video.3 > 0, "test must resume mid-stream");
    assert!(tail[0].3 == first_video.3 && tail[1].3 == first_video.3, "headers not stamped with the IDR time: {:?}", &tail[..3]);
    assert!(tail.windows(2).all(|w| w[1].3 >= w[0].3), "timestamps went backwards after resuming");
}

#[test]
fn pause_resume_restart_one_session_leaves_the_other_untouched() {
    let mgr = SessionManager::new();
    let cfg = SessionConfig { backoff_min: Duration::from_millis(50), ..Default::default() };
    let (lp, lb) = (Log::default(), Log::default());
    let (a, b) = (lp.clone(), lb.clone());
    let primary = mgr.add(cfg.clone(), Box::new(move || Box::new(Mock(a.clone()))));
    let backup = mgr.add(cfg, Box::new(move || Box::new(Mock(b.clone()))));

    mgr.set_header(pkt(Kind::Video, 0, true, true));
    mgr.set_header(pkt(Kind::Audio, 0, false, true));
    wait_state(&mgr, primary, SessionState::Live);
    wait_state(&mgr, backup, SessionState::Live);

    let mut dts = 0;
    feed(&mgr, &mut dts, 30);

    // ---- pause only the backup
    assert!(mgr.pause(backup));
    wait_state(&mgr, backup, SessionState::Paused);
    let frozen = lb.sent.lock().unwrap().len();
    let primary_before = lp.sent.lock().unwrap().len();
    feed(&mgr, &mut dts, 30);
    let s = mgr.snapshot(backup).unwrap();
    assert_eq!(lb.sent.lock().unwrap().len(), frozen, "paused backup kept sending");
    assert_eq!(s.queued_bytes, 0, "paused backup is accumulating a queue");
    assert_eq!(s.drops.dropped_video_frames, 0, "pausing must not count as a drop");
    assert_eq!(lp.sent.lock().unwrap().len(), primary_before + 60, "primary was affected");

    // ---- resume the backup
    assert!(mgr.resume(backup));
    wait_state(&mgr, backup, SessionState::Live);
    feed(&mgr, &mut dts, 30);
    assert_rejoins_at_keyframe(&lb, frozen);
    assert_eq!(lb.connects.load(Ordering::SeqCst), 2);

    // ---- restart the primary: reconnects immediately, the backup is unaware
    let primary_mark = lp.sent.lock().unwrap().len();
    let backup_connects = lb.connects.load(Ordering::SeqCst);
    let t = Instant::now();
    assert!(mgr.restart(primary));
    let deadline = Instant::now() + Duration::from_secs(2);
    while lp.connects.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(lp.connects.load(Ordering::SeqCst), 2, "primary did not reconnect");
    assert!(t.elapsed() < Duration::from_millis(500), "restart waited for a backoff: {:?}", t.elapsed());
    wait_state(&mgr, primary, SessionState::Live);
    feed(&mgr, &mut dts, 30);
    assert_rejoins_at_keyframe(&lp, primary_mark);
    assert_eq!(lb.connects.load(Ordering::SeqCst), backup_connects, "backup reconnected without being asked");

    // manual actions do not count as failure reconnects
    assert_eq!(mgr.snapshot(primary).unwrap().reconnects, 0);
    assert_eq!(mgr.snapshot(backup).unwrap().reconnects, 0);

    // ---- restarting a paused session resumes it
    assert!(mgr.pause(primary));
    wait_state(&mgr, primary, SessionState::Paused);
    assert!(mgr.restart(primary));
    wait_state(&mgr, primary, SessionState::Live);
    feed(&mgr, &mut dts, 30);

    // shared timestamps: both receive the same dts (identical redundancy)
    let last_p = lp.sent.lock().unwrap().iter().rev().find(|p| p.0 == Kind::Video && !p.1).unwrap().3;
    let last_b = lb.sent.lock().unwrap().iter().rev().find(|p| p.0 == Kind::Video && !p.1).unwrap().3;
    assert_eq!(last_p, last_b);

    assert!(!mgr.pause(9999), "nonexistent id");
    mgr.shutdown();
}
