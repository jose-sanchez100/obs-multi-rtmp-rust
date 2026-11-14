//! Tests the real RTMP transport against a local rml_rtmp server (no TLS).
use bytes::Bytes;
use obs_rtmp_sessions::rtmp::{RtmpTransport, RtmpUrl};
use obs_rtmp_sessions::session::Transport;
use obs_rtmp_sessions::{IpFamily, Kind, MediaPacket, NetOptions};
use rml_rtmp::chunk_io::ChunkSerializer;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::messages::RtmpMessage;
use rml_rtmp::rml_amf0::Amf0Value;
use rml_rtmp::sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult};
use rml_rtmp::time::RtmpTimestamp;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

fn serve(
    listener: TcpListener,
    tx: mpsc::Sender<(String, String, usize)>,
    end_publish: Option<mpsc::Receiver<()>>,
) {
    let (mut s, _) = listener.accept().unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut hs = Handshake::new(PeerType::Server);
    let mut buf = [0u8; 8192];
    let mut leftover;
    loop {
        let n = s.read(&mut buf).unwrap();
        match hs.process_bytes(&buf[..n]).unwrap() {
            HandshakeProcessResult::InProgress { response_bytes } => s.write_all(&response_bytes).unwrap(),
            HandshakeProcessResult::Completed { response_bytes, remaining_bytes } => {
                s.write_all(&response_bytes).unwrap();
                leftover = remaining_bytes;
                break;
            }
        }
    }
    let (mut sess, init) = ServerSession::new(ServerSessionConfig::new()).unwrap();
    let handle = |sess: &mut ServerSession, s: &mut std::net::TcpStream, res: Vec<ServerSessionResult>| {
        let mut queue: Vec<ServerSessionResult> = res;
        while !queue.is_empty() {
            let r = queue.remove(0);
            match r {
                ServerSessionResult::OutboundResponse(p) => s.write_all(&p.bytes).unwrap(),
                ServerSessionResult::RaisedEvent(ev) => match ev {
                    ServerSessionEvent::ConnectionRequested { request_id, .. } => {
                        queue.extend(sess.accept_request(request_id).unwrap());
                    }
                    ServerSessionEvent::PublishStreamRequested { request_id, app_name, stream_key, .. } => {
                        queue.extend(sess.accept_request(request_id).unwrap());
                        tx.send((app_name, stream_key, 0)).unwrap();
                        if let Some(end_publish) = &end_publish {
                            let status = RtmpMessage::Amf0Command {
                                command_name: "onStatus".into(),
                                transaction_id: 0.0,
                                command_object: Amf0Value::Null,
                                additional_arguments: vec![Amf0Value::Object(
                                    [("code".into(), Amf0Value::Utf8String("NetStream.Unpublish.Success".into()))].into(),
                                )],
                            };
                            // The test server assigns ID 1 to its first stream.
                            let payload = status.into_message_payload(RtmpTimestamp::new(0), 1).unwrap();
                            let packet = ChunkSerializer::new().serialize(&payload, true, false).unwrap();
                            // Let the client enter LIVE before ending the publish.
                            for reply in queue.drain(..) {
                                if let ServerSessionResult::OutboundResponse(reply) = reply {
                                    s.write_all(&reply.bytes).unwrap();
                                }
                            }
                            end_publish.recv_timeout(Duration::from_secs(5)).unwrap();
                            s.write_all(&packet.bytes).unwrap();
                        }
                    }
                    ServerSessionEvent::VideoDataReceived { data, .. } => {
                        tx.send(("video".into(), String::new(), data.len())).unwrap();
                    }
                    ServerSessionEvent::AudioDataReceived { data, .. } => {
                        tx.send(("audio".into(), String::new(), data.len())).unwrap();
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    };
    handle(&mut sess, &mut s, init);
    if !leftover.is_empty() {
        let r = sess.handle_input(&leftover).unwrap();
        handle(&mut sess, &mut s, r);
        leftover.clear();
    }
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let r = sess.handle_input(&buf[..n]).unwrap();
                handle(&mut sess, &mut s, r);
            }
        }
    }
}

#[test]
fn publishes_to_local_rtmp_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || serve(listener, tx, None));

    let url = RtmpUrl::parse(&format!("rtmp://127.0.0.1:{port}/live"), Some("key123")).unwrap();
    let mut t = RtmpTransport::new(url);
    t.connect().expect("connect+publish");

    let (app, key, _) = rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!((app.as_str(), key.as_str()), ("live", "key123"));

    let mk = |kind, dts, key, n| MediaPacket { kind, dts_ms: dts, keyframe: key, is_header: false, data: Bytes::from(vec![9u8; n]) };
    t.send(&mk(Kind::Video, 0, true, 5000)).unwrap();
    t.send(&mk(Kind::Audio, 10, false, 300)).unwrap();
    t.send(&mk(Kind::Video, 33, false, 1200)).unwrap();

    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(rx.recv_timeout(Duration::from_secs(3)).unwrap());
    }
    assert_eq!(got.iter().map(|g| (g.0.as_str(), g.2)).collect::<Vec<_>>(),
        vec![("video", 5000), ("audio", 300), ("video", 1200)]);

    // Where the connection went, and the kernel's TCP statistics for it.
    let info = t.connection_info().expect("connection info");
    assert_eq!(info.host, "127.0.0.1");
    assert_eq!(info.remote, format!("127.0.0.1:{port}").parse().unwrap());
    assert!(info.local.ip().is_loopback() && info.local.port() != 0);
    if cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        let tcp = t.tcp_stats().expect("TCP statistics");
        assert!(tcp.sent > 0 && tcp.retransmitted <= tcp.sent, "{tcp:?}");
    }
    t.close();
    assert!(t.tcp_stats().is_none());
}

#[test]
fn detects_server_ending_an_idle_publish() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, _rx) = mpsc::channel();
    let (end_publish, end_request) = mpsc::channel();
    let server = std::thread::spawn(move || serve(listener, tx, Some(end_request)));
    let url = RtmpUrl::parse(&format!("rtmp://127.0.0.1:{port}/live"), Some("test")).unwrap();
    let mut transport = RtmpTransport::new(url);
    transport.connect().unwrap();
    end_publish.send(()).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let error = loop {
        if let Err(error) = transport.poll() {
            break error;
        }
        assert!(std::time::Instant::now() < deadline, "onStatus was not detected");
        std::thread::sleep(Duration::from_millis(30));
    };
    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
    assert!(error.to_string().contains("NetStream.Unpublish.Success"));
    transport.close();
    server.join().unwrap();
}

#[test]
fn binds_local_ip_and_filters_family() {
    // Bind IP: the connection goes out through the chosen local IP and publishes normally.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || serve(listener, tx, None));
    let url = RtmpUrl::parse(&format!("rtmp://127.0.0.1:{port}/live"), Some("k")).unwrap();
    let net = NetOptions { family: IpFamily::Any, bind_ip: Some("127.0.0.1".parse().unwrap()), ..Default::default() };
    let mut t = RtmpTransport::new(url.clone()).with_net(net);
    t.connect().expect("connect with bind IP");
    assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap().1, "k");
    t.close();

    // IPv6 only against a server that only has IPv4: clear error, no connection.
    let net = NetOptions { family: IpFamily::V6, bind_ip: None, ..Default::default() };
    let err = RtmpTransport::new(url.clone()).with_net(net).connect().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");

    // An IPv6 local IP forces IPv6 even if the family says "both" (like OBS).
    let net = NetOptions { family: IpFamily::Any, bind_ip: Some("::1".parse().unwrap()), ..Default::default() };
    let err = RtmpTransport::new(url).with_net(net).connect().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
}
