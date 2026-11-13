//! Probe: connects over RTMP/RTMPS until "publish accepted" and closes without sending media.
//! OMR_URL and OMR_KEY are read from the environment (the key is never stored on disk).
use obs_rtmp_sessions::rtmp::{RtmpTransport, RtmpUrl};
use obs_rtmp_sessions::session::Transport;
use std::time::Instant;

fn main() {
    let url = std::env::var("OMR_URL").expect("OMR_URL");
    let key = std::env::var("OMR_KEY").ok();
    let parsed = RtmpUrl::parse(&url, key.as_deref()).expect("url");
    println!("tls={} host={} port={} app={}", parsed.tls, parsed.host, parsed.port, parsed.app);
    let mut t = RtmpTransport::new(parsed);
    let t0 = Instant::now();
    match t.connect() {
        Ok(()) => println!("OK: publish accepted in {:?}", t0.elapsed()),
        Err(e) => println!("ERROR after {:?}: {e}", t0.elapsed()),
    }
    t.close();
}
