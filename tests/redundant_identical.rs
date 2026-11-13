//! Primary/backup redundancy (YouTube): both streams must carry exactly the
//! same bytes with the same timestamps; under asymmetric congestion the slow stream
//! may only LOSE packets (and resume at a keyframe), never diverge.
use bytes::Bytes;
use obs_rtmp_sessions::rtmp::{NetOptions, RtmpTransport, RtmpUrl};
use obs_rtmp_sessions::session::SessionConfig;
use obs_rtmp_sessions::{Kind, MediaPacket, QueueLimits, SessionManager};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Capture = Arc<Mutex<Vec<(Kind, u32, Vec<u8>)>>>;

/// Kernel socket buffer size for the throttled link, on both ends. Loopback buffers
/// are autotuned (up to tens of MB on Linux) and would absorb the whole backlog,
/// hiding the congestion from the sender's queue; a congested real link keeps them small.
const THROTTLED_SOCKET_BUFFER: usize = 32 * 1024;

fn listen(throttled: bool) -> TcpListener {
    use socket2::{Domain, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
    if throttled {
        // Before listen(), so accepted sockets inherit it and the window scale matches.
        s.set_recv_buffer_size(THROTTLED_SOCKET_BUFFER).unwrap();
    }
    s.bind(&"127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap().into()).unwrap();
    s.listen(4).unwrap();
    s.into()
}

/// `throttle`: Some(bytes per 10 ms) to simulate a throttled link.
fn spawn_server(throttle: Option<usize>) -> (u16, Capture, std::thread::JoinHandle<()>) {
    let l = listen(throttle.is_some());
    let port = l.local_addr().unwrap().port();
    let cap: Capture = Arc::new(Mutex::new(Vec::new()));
    let c2 = cap.clone();
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        let mut hs = Handshake::new(PeerType::Server);
        let mut buf = [0u8; 16384];
        let leftover = loop {
            let n = s.read(&mut buf).unwrap();
            match hs.process_bytes(&buf[..n]).unwrap() {
                HandshakeProcessResult::InProgress { response_bytes } => s.write_all(&response_bytes).unwrap(),
                HandshakeProcessResult::Completed { response_bytes, remaining_bytes } => {
                    s.write_all(&response_bytes).unwrap();
                    break remaining_bytes;
                }
            }
        };
        let (mut sess, init) = ServerSession::new(ServerSessionConfig::new()).unwrap();
        let process = |sess: &mut ServerSession, s: &mut std::net::TcpStream, res: Vec<ServerSessionResult>| {
            let mut q = res;
            while !q.is_empty() {
                match q.remove(0) {
                    ServerSessionResult::OutboundResponse(p) => {
                        let _ = s.write_all(&p.bytes);
                    }
                    ServerSessionResult::RaisedEvent(ev) => match ev {
                        ServerSessionEvent::ConnectionRequested { request_id, .. }
                        | ServerSessionEvent::PublishStreamRequested { request_id, .. } => {
                            q.extend(sess.accept_request(request_id).unwrap())
                        }
                        ServerSessionEvent::VideoDataReceived { data, timestamp, .. } => {
                            c2.lock().unwrap().push((Kind::Video, timestamp.value, data.to_vec()))
                        }
                        ServerSessionEvent::AudioDataReceived { data, timestamp, .. } => {
                            c2.lock().unwrap().push((Kind::Audio, timestamp.value, data.to_vec()))
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        };
        process(&mut sess, &mut s, init);
        if !leftover.is_empty() {
            let r = sess.handle_input(&leftover).unwrap();
            process(&mut sess, &mut s, r);
        }
        loop {
            let want = throttle.map(|t| t.min(buf.len())).unwrap_or(buf.len());
            match s.read(&mut buf[..want]) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let r = sess.handle_input(&buf[..n]).unwrap();
                    process(&mut sess, &mut s, r);
                }
            }
            if throttle.is_some() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    });
    (port, cap, h)
}

fn video(dts: u32, key: bool) -> MediaPacket {
    // deterministic, per-frame distinct content to detect any alteration
    let n = if key { 9000 } else { 2500 };
    let mut data: Vec<u8> = (0..n).map(|i| (i as u32 ^ dts) as u8).collect();
    data[0] = if key { 0x17 } else { 0x27 };
    MediaPacket { kind: Kind::Video, dts_ms: dts, keyframe: key, is_header: false, data: Bytes::from(data) }
}
fn audio(dts: u32) -> MediaPacket {
    MediaPacket { kind: Kind::Audio, dts_ms: dts, keyframe: false, is_header: false, data: Bytes::from(vec![(dts % 251) as u8; 300]) }
}

fn run(throttle_b: Option<usize>) -> (Vec<(Kind, u32, Vec<u8>)>, Vec<(Kind, u32, Vec<u8>)>, u64) {
    let (pa, ca, ha) = spawn_server(None);
    let (pb, cb, hb) = spawn_server(throttle_b);
    let mgr = SessionManager::new();
    let cfg = SessionConfig {
        limits: QueueLimits { soft_bytes: 1 << 30, hard_bytes: 1 << 30, max_latency_ms: 1500 },
        ..Default::default()
    };
    let mut ids = Vec::new();
    for (port, throttled) in [(pa, false), (pb, throttle_b.is_some())] {
        let url = RtmpUrl::parse(&format!("rtmp://127.0.0.1:{port}/live2"), Some("key")).unwrap();
        let net = NetOptions { send_buffer: throttled.then_some(THROTTLED_SOCKET_BUFFER), ..Default::default() };
        ids.push(mgr.add(
            cfg.clone(),
            Box::new(move || Box::new(RtmpTransport::new(url.clone()).with_net(net.clone()))),
        ));
    }
    // identical headers for both
    let mut hv = video(0, true);
    hv.is_header = true;
    mgr.set_header(hv);
    let mut ha_pkt = audio(0);
    ha_pkt.is_header = true;
    mgr.set_header(ha_pkt);
    std::thread::sleep(Duration::from_millis(300));

    // 8 s of media in real time: 30 fps, IDR every 1 s, audio every ~21 ms
    let t0 = Instant::now();
    let mut next_audio = 0u32;
    for f in 0..240u32 {
        let dts = f * 33;
        while next_audio <= dts {
            mgr.push_all(&audio(next_audio));
            next_audio += 21;
        }
        mgr.push_all(&video(dts, f % 30 == 0));
        std::thread::sleep(Duration::from_millis(dts as u64 + 33).saturating_sub(t0.elapsed()));
    }
    // drain queues
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if ids.iter().all(|&i| mgr.snapshot(i).unwrap().queued_bytes == 0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(500));
    let dropped_b = mgr.snapshot(ids[1]).unwrap().drops.dropped_video_frames;
    mgr.shutdown();
    ha.join().unwrap();
    hb.join().unwrap();
    let a = ca.lock().unwrap().clone();
    let b = cb.lock().unwrap().clone();
    (a, b, dropped_b)
}

#[test]
fn both_streams_identical_without_congestion() {
    let (a, b, dropped) = run(None);
    assert_eq!(dropped, 0);
    assert!(a.len() > 400, "len={}", a.len());
    assert_eq!(a, b, "primary and backup differ");
}

#[test]
fn congested_backup_is_a_strict_subset_of_primary() {
    // ~30 KB/s versus ~90 KB/s of media: sustained congestion
    let (a, b, dropped_b) = run(Some(300));
    println!("primary={} packets, backup={} packets, video frames dropped on backup={dropped_b}", a.len(), b.len());

    // the primary receives everything
    let produced_video = 240;
    assert_eq!(a.iter().filter(|p| p.0 == Kind::Video).count(), produced_video + 1 /*header*/);

    // the backup dropped something...
    assert!(dropped_b > 0 && b.len() < a.len(), "backup should have lost packets");

    // ...but everything that arrived is BYTE-IDENTICAL to the primary's at the same timestamp
    for (k, ts, data) in &b {
        // headers have ts 0 and share a key with the first frame: compare by content
        let same = a.iter().any(|p| p.0 == *k && p.1 == *ts && &p.2 == data);
        assert!(same, "backup packet with no identical counterpart in the primary: {ts}");
    }

    // after each gap the backup resumes at a keyframe (0x17), never at a P-frame
    let mut last_ts: Option<u32> = None;
    for (k, ts, data) in b.iter().filter(|p| p.0 == Kind::Video) {
        if let Some(l) = last_ts {
            if *ts - l > 40 {
                assert_eq!(data[0], 0x17, "resumed at a non-key frame at ts={ts}");
            }
        }
        let _ = k;
        last_ts = Some(*ts);
    }
}
