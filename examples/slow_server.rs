//! Local RTMP server that reads at a limited rate to simulate congestion.
//! args: port  limited_kbps  limited_seconds
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult};
use std::cell::Cell;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let port = &a[1];
    let kbps: u64 = a[2].parse().unwrap();
    let limit_secs: u64 = a[3].parse().unwrap();
    let l = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
    println!("listening on {port}, limit {kbps} kbps for {limit_secs}s");
    let (mut s, _) = l.accept().unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

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
    let (vframes, keyframes, aframes) = (Cell::new(0u64), Cell::new(0u64), Cell::new(0u64));
    let last_video_ts: Cell<Option<u32>> = Cell::new(None);
    let max_gap_ms = Cell::new(0u32);

    let process = |sess: &mut ServerSession, s: &mut std::net::TcpStream, res: Vec<ServerSessionResult>| {
        let mut q = res;
        while !q.is_empty() {
            match q.remove(0) {
                ServerSessionResult::OutboundResponse(p) => s.write_all(&p.bytes).unwrap(),
                ServerSessionResult::RaisedEvent(ev) => match ev {
                    ServerSessionEvent::ConnectionRequested { request_id, .. } => q.extend(sess.accept_request(request_id).unwrap()),
                    ServerSessionEvent::PublishStreamRequested { request_id, .. } => q.extend(sess.accept_request(request_id).unwrap()),
                    ServerSessionEvent::VideoDataReceived { data, timestamp, .. } => {
                        vframes.set(vframes.get() + 1);
                        if data.first() == Some(&0x17) && data.get(1) == Some(&1) {
                            keyframes.set(keyframes.get() + 1);
                        }
                        if let Some(l) = last_video_ts.get() {
                            max_gap_ms.set(max_gap_ms.get().max(timestamp.value.wrapping_sub(l)));
                        }
                        last_video_ts.set(Some(timestamp.value));
                    }
                    ServerSessionEvent::AudioDataReceived { .. } => aframes.set(aframes.get() + 1),
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

    let t0 = Instant::now();
    let mut last_print = Instant::now();
    let chunk = (kbps * 1000 / 8 / 20) as usize; // 20 reads/s
    loop {
        let limited = t0.elapsed() < Duration::from_secs(limit_secs);
        let want = if limited { chunk.min(buf.len()) } else { buf.len() };
        match s.read(&mut buf[..want]) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let r = sess.handle_input(&buf[..n]).unwrap();
                process(&mut sess, &mut s, r);
            }
        }
        if limited {
            std::thread::sleep(Duration::from_millis(50));
        }
        if last_print.elapsed() > Duration::from_secs(5) {
            last_print = Instant::now();
            println!("[srv t={:>4.1}s {}] video={} (keyframes={}) audio={} max_video_gap={}ms",
                t0.elapsed().as_secs_f32(), if limited { "SLOW" } else { "free" }, vframes.get(), keyframes.get(), aframes.get(), max_gap_ms.get());
        }
    }
    println!("[srv END] video={} keyframes={} audio={} max_video_gap={}ms", vframes.get(), keyframes.get(), aframes.get(), max_gap_ms.get());
}
