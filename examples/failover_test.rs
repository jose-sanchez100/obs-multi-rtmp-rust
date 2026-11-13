//! Cuts the primary (simulated transport failure) mid-stream and lets it
//! reconnect through the real session path; the backup continues uninterrupted.
//! OMR_URL, OMR_BACKUP_URL, OMR_KEY (environment); args: video audio cut_at_s cut_len_s
use obs_rtmp_sessions::flv;
use obs_rtmp_sessions::rtmp::{RtmpTransport, RtmpUrl};
use obs_rtmp_sessions::session::{SessionConfig, Transport};
use obs_rtmp_sessions::{MediaPacket, SessionManager};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Cuttable {
    inner: RtmpTransport,
    cut: Arc<AtomicBool>,
}
impl Transport for Cuttable {
    fn connect(&mut self) -> io::Result<()> {
        if self.cut.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "simulated cut"));
        }
        self.inner.connect()
    }
    fn send(&mut self, p: &MediaPacket) -> io::Result<()> {
        if self.cut.load(Ordering::SeqCst) {
            self.inner.close();
            return Err(io::Error::new(io::ErrorKind::ConnectionReset, "simulated cut"));
        }
        self.inner.send(p)
    }
    fn close(&mut self) {
        self.inner.close()
    }
}

fn split_aus(d: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 4 < d.len() {
        let sc = if d[i..].starts_with(&[0, 0, 0, 1]) { 4 } else if d[i..].starts_with(&[0, 0, 1]) { 3 } else { 0 };
        if sc > 0 && d[i + sc] & 0x1f == 9 {
            starts.push(i);
        }
        i += if sc > 0 { sc } else { 1 };
    }
    starts.iter().enumerate().map(|(k, &s)| &d[s..*starts.get(k + 1).unwrap_or(&d.len())]).collect()
}

fn adts(d: &[u8]) -> (Vec<u8>, Vec<&[u8]>) {
    let aot = ((d[2] >> 6) & 3) + 1;
    let fi = (d[2] >> 2) & 0xf;
    let ch = ((d[2] & 1) << 2) | (d[3] >> 6);
    let asc = (((aot as u16) << 11) | ((fi as u16) << 7) | ((ch as u16) << 3)).to_be_bytes().to_vec();
    let (mut v, mut i) = (Vec::new(), 0);
    while i + 7 <= d.len() {
        let len = (((d[i + 3] & 3) as usize) << 11) | ((d[i + 4] as usize) << 3) | ((d[i + 5] as usize) >> 5);
        let hdr = if d[i + 1] & 1 == 0 { 9 } else { 7 };
        if len < hdr || i + len > d.len() {
            break;
        }
        v.push(&d[i + hdr..i + len]);
        i += len;
    }
    (asc, v)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (vid, aud) = (std::fs::read(&a[1]).unwrap(), std::fs::read(&a[2]).unwrap());
    let cut_at: f64 = a[3].parse().unwrap();
    let cut_len: f64 = a[4].parse().unwrap();
    let key = std::env::var("OMR_KEY").unwrap();
    let mk = |u: String| RtmpUrl::parse(&u, Some(&key)).unwrap();
    let (u1, u2) = (mk(std::env::var("OMR_URL").unwrap()), mk(std::env::var("OMR_BACKUP_URL").unwrap()));
    let cut = Arc::new(AtomicBool::new(false));

    let cfg = SessionConfig {
        backoff_min: Duration::from_millis(500),
        backoff_max: Duration::from_secs(2),
        ..Default::default()
    };
    let mgr = SessionManager::new();
    let c2 = cut.clone();
    let p = mgr.add(cfg.clone(), Box::new(move || Box::new(Cuttable { inner: RtmpTransport::new(u1.clone()), cut: c2.clone() })));
    let b = mgr.add(cfg, Box::new(move || Box::new(RtmpTransport::new(u2.clone()))));

    let aus = split_aus(&vid);
    let (asc, afr) = adts(&aud);
    mgr.set_header(flv::video_header(aus[0]).unwrap());
    mgr.set_header(flv::audio_header(&asc));

    let t0 = Instant::now();
    let (mut vi, mut ai) = (0usize, 0usize);
    let mut last = Instant::now();
    let mut cut_state = 0; // 0 before, 1 cut, 2 restored
    while vi < aus.len() || ai < afr.len() {
        let el = t0.elapsed().as_secs_f64();
        if cut_state == 0 && el >= cut_at {
            cut.store(true, Ordering::SeqCst);
            cut_state = 1;
            println!(">>> t={el:.1}s primary CUT");
        } else if cut_state == 1 && el >= cut_at + cut_len {
            cut.store(false, Ordering::SeqCst);
            cut_state = 2;
            println!(">>> t={el:.1}s primary RESTORED (reconnects on its own)");
        }
        let vt = vi as f64 * 1000.0 / 30.0;
        let at = ai as f64 * 1024.0 * 1000.0 / 48000.0;
        let is_video = vi < aus.len() && (ai >= afr.len() || vt <= at);
        let due = if is_video { vt } else { at };
        std::thread::sleep(Duration::from_millis(due as u64).saturating_sub(t0.elapsed()));
        if is_video {
            if let Some(pk) = flv::video_frame(vt as i64, vt as i64, aus[vi]) {
                mgr.push_all(&pk);
            }
            vi += 1;
        } else {
            mgr.push_all(&flv::audio_frame(at as i64, afr[ai]));
            ai += 1;
        }
        if last.elapsed() >= Duration::from_secs(2) {
            last = Instant::now();
            let (sp, sb) = (mgr.snapshot(p).unwrap(), mgr.snapshot(b).unwrap());
            println!("t={:>4.1}s  primary: {:?} sent={} recon={} drop_v={}  |  backup: {:?} sent={} recon={} drop_v={}",
                t0.elapsed().as_secs_f32(), sp.state, sp.packets_sent, sp.reconnects, sp.drops.dropped_video_frames,
                sb.state, sb.packets_sent, sb.reconnects, sb.drops.dropped_video_frames);
        }
    }
    std::thread::sleep(Duration::from_millis(2000));
    let (sp, sb) = (mgr.snapshot(p).unwrap(), mgr.snapshot(b).unwrap());
    println!("END primary: {:?} sent={} recon={} drop_v={} drop_a={} gops={} err={:?}", sp.state, sp.packets_sent, sp.reconnects, sp.drops.dropped_video_frames, sp.drops.dropped_audio_frames, sp.drops.dropped_gops, sp.last_error);
    println!("END backup:  {:?} sent={} recon={} drop_v={} drop_a={}", sb.state, sb.packets_sent, sb.reconnects, sb.drops.dropped_video_frames, sb.drops.dropped_audio_frames);
    mgr.shutdown();
}
