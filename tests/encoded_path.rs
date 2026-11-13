//! Tests the real RTMP transport against a local rml_rtmp server (no TLS).
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

fn serve(listener: TcpListener, tx: mpsc::Sender<(String, Vec<u8>)>) {
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
                        tx.send(("publish".into(), format!("{app_name}/{stream_key}").into_bytes())).unwrap();
                    }
                    ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                        assert_eq!((metadata.video_width, metadata.video_height), (Some(1920), Some(1080)));
                        assert_eq!(metadata.video_frame_rate, Some(29.97));
                        assert_eq!((metadata.video_codec_id, metadata.audio_codec_id), (Some(7), Some(10)));
                        assert_eq!((metadata.video_bitrate_kbps, metadata.audio_bitrate_kbps), (Some(6000), Some(128)));
                        assert_eq!((metadata.audio_sample_rate, metadata.audio_channels), (Some(48000), Some(2)));
                        assert_eq!(metadata.audio_is_stereo, Some(true));
                        tx.send(("metadata".into(), Vec::new())).unwrap();
                    }
                    ServerSessionEvent::VideoDataReceived { data, .. } => {
                        tx.send(("video".into(), data.to_vec())).unwrap();
                    }
                    ServerSessionEvent::AudioDataReceived { data, .. } => {
                        tx.send(("audio".into(), data.to_vec())).unwrap();
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


use obs_rtmp_sessions::ffi::*;
use std::ffi::CString;

#[test]
fn obs_style_encoded_packets_reach_server_as_valid_flv() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || serve(listener, tx));

    let sps = [0x67u8, 0x64, 0x00, 0x1f, 0xac, 0xd9];
    let pps = [0x68u8, 0xeb, 0xe3, 0xcb];
    let mut extra = vec![0, 0, 0, 1];
    extra.extend_from_slice(&sps);
    extra.extend_from_slice(&[0, 0, 0, 1]);
    extra.extend_from_slice(&pps);
    let idr = [0x65u8, 1, 2, 3, 4];
    let mut au = vec![0, 0, 0, 1];
    au.extend_from_slice(&extra[4..]); // (SPS+PPS repeated in the IDR, as x264 does)
    au.splice(0..0, []);
    let mut frame = extra.clone();
    frame.extend_from_slice(&[0, 0, 0, 1]);
    frame.extend_from_slice(&idr);

    unsafe {
        let m = omr_manager_new();
        let url = CString::new(format!("rtmp://127.0.0.1:{port}/live")).unwrap();
        let key = CString::new("k").unwrap();
        let info = OmrStreamInfo {
            width: 1920,
            height: 1080,
            frame_rate: 29.97,
            video_bitrate_kbps: 6000,
            audio_bitrate_kbps: 128,
            audio_sample_rate: 48000,
            audio_channels: 2,
        };
        // Same as OBS: metadata -> codec headers -> audio and video.
        let id = omr_session_add_with_info(m, url.as_ptr(), key.as_ptr(), 0, 0, &info);
        assert!(id > 0);
        assert_eq!(omr_set_video_extradata(m, extra.as_ptr(), extra.len()), 1);
        assert_eq!(omr_set_audio_extradata(m, [0x12u8, 0x10].as_ptr(), 2), 1);
        omr_push_video(m, 0, 66, frame.as_ptr(), frame.len());
        omr_push_audio(m, 10, [7u8, 7, 7].as_ptr(), 3);

        let mut got = Vec::new();
        while got.len() < 6 {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(x) => got.push(x),
                Err(_) => break,
            }
        }
        let mut st = std::mem::zeroed::<OmrStats>();
        assert_eq!(omr_session_stats(m, id, &mut st), 1);
        omr_manager_free(m);

        assert_eq!(got[0].0, "publish");
        assert_eq!(got[0].1, b"live/k");
        assert_eq!(got[1].0, "metadata");
        let video: Vec<_> = got.iter().filter(|g| g.0 == "video").collect();
        let audio: Vec<_> = got.iter().filter(|g| g.0 == "audio").collect();
        assert_eq!(video.len(), 2, "{got:?}");
        // AVC header: 0x17 0x00 + record (version=1, SPS profile)
        assert_eq!(&video[0].1[..2], &[0x17, 0x00]);
        assert_eq!(&video[0].1[5..9], &[1, 0x64, 0x00, 0x1f]);
        // IDR: 0x17 0x01, cts=66, length 5 + NAL; no repeated SPS/PPS
        assert_eq!(&video[1].1[..5], &[0x17, 0x01, 0, 0, 66]);
        assert_eq!(&video[1].1[5..], &[0, 0, 0, 5, 0x65, 1, 2, 3, 4]);
        assert_eq!(audio[0].1, vec![0xAF, 0, 0x12, 0x10]);
        assert_eq!(audio[1].1, vec![0xAF, 1, 7, 7, 7]);
    }
}
