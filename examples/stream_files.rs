//! Streams a .h264 (Annex-B with AUD) and an .aac (ADTS) in real time through the C API,
//! just as the OBS output will. OMR_URL, OMR_KEY (environment), args: video audio.
use obs_rtmp_sessions::ffi::*;
use std::ffi::CString;
use std::time::{Duration, Instant};

fn split_aus(d: &[u8]) -> Vec<&[u8]> {
    // Access units delimited by NAL type 9 (AUD)
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 4 < d.len() {
        let (sc, ok) = if d[i..].starts_with(&[0, 0, 0, 1]) { (4, true) } else if d[i..].starts_with(&[0, 0, 1]) { (3, true) } else { (0, false) };
        if ok && d[i + sc] & 0x1f == 9 {
            starts.push(i);
        }
        i += if ok { sc } else { 1 };
    }
    starts.iter().enumerate().map(|(k, &s)| &d[s..*starts.get(k + 1).unwrap_or(&d.len())]).collect()
}

fn adts_frames(d: &[u8]) -> (Vec<u8>, Vec<&[u8]>) {
    let aot = ((d[2] >> 6) & 3) + 1;
    let fi = (d[2] >> 2) & 0xf;
    let ch = ((d[2] & 1) << 2) | (d[3] >> 6);
    let asc = (((aot as u16) << 11) | ((fi as u16) << 7) | ((ch as u16) << 3)).to_be_bytes().to_vec();
    let mut v = Vec::new();
    let mut i = 0;
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
    let args: Vec<String> = std::env::args().collect();
    let vid = std::fs::read(&args[1]).unwrap();
    let aud = std::fs::read(&args[2]).unwrap();
    let url = CString::new(std::env::var("OMR_URL").unwrap()).unwrap();
    let key = CString::new(std::env::var("OMR_KEY").unwrap()).unwrap();
    let aus = split_aus(&vid);
    let (asc, afr) = adts_frames(&aud);
    println!("{} video access units, {} AAC frames, ASC={:02x?}", aus.len(), afr.len(), asc);

    unsafe {
        let m = omr_manager_new();
        let id = omr_session_add(m, url.as_ptr(), key.as_ptr(), 0, 0);
        assert!(id > 0);
        // Optional backup (OMR_BACKUP_URL), same key: same packets for both sessions.
        let backup_id = std::env::var("OMR_BACKUP_URL").ok().map(|u| {
            let u = CString::new(u).unwrap();
            let b = omr_session_add(m, u.as_ptr(), key.as_ptr(), 0, 0);
            assert!(b > 0);
            b
        });
        assert_eq!(omr_set_video_extradata(m, aus[0].as_ptr(), aus[0].len()), 1);
        assert_eq!(omr_set_audio_extradata(m, asc.as_ptr(), asc.len()), 1);

        let t0 = Instant::now();
        let (mut vi, mut ai) = (0usize, 0usize);
        let mut last_print = Instant::now();
        while vi < aus.len() || ai < afr.len() {
            let vt = vi as f64 * 1000.0 / 30.0;
            let at = ai as f64 * 1024.0 * 1000.0 / 48000.0;
            let (is_video, due) = if vi < aus.len() && (ai >= afr.len() || vt <= at) { (true, vt) } else { (false, at) };
            let wait = Duration::from_millis(due as u64).saturating_sub(t0.elapsed());
            std::thread::sleep(wait);
            if is_video {
                omr_push_video(m, vt as i64, vt as i64, aus[vi].as_ptr(), aus[vi].len());
                vi += 1;
            } else {
                omr_push_audio(m, at as i64, afr[ai].as_ptr(), afr[ai].len());
                ai += 1;
            }
            if last_print.elapsed() > Duration::from_secs(5) {
                last_print = Instant::now();
                let mut st = std::mem::zeroed::<OmrStats>();
                omr_session_stats(m, id, &mut st);
                if let Some(b) = backup_id {
                    let mut sb = std::mem::zeroed::<OmrStats>();
                    omr_session_stats(m, b, &mut sb);
                    println!("      backup state={} sent={} bytes={} queued={} dropped_v={} reconnects={}",
                        sb.state, sb.packets_sent, sb.bytes_sent, sb.queued_bytes, sb.dropped_video_frames, sb.reconnects);
                }
                println!("t={:>4.1}s state={} sent={} bytes={} queued={} dropped_v={} reconnects={}",
                    t0.elapsed().as_secs_f32(), st.state, st.packets_sent, st.bytes_sent, st.queued_bytes, st.dropped_video_frames, st.reconnects);
            }
        }
        std::thread::sleep(Duration::from_millis(1500)); // drain the queue
        let mut st = std::mem::zeroed::<OmrStats>();
        omr_session_stats(m, id, &mut st);
        println!("END state={} sent={} bytes={} queued={} dropped_v={} dropped_gops={} reconnects={}",
            st.state, st.packets_sent, st.bytes_sent, st.queued_bytes, st.dropped_video_frames, st.dropped_gops, st.reconnects);
        if let Some(b) = backup_id {
            let mut sb = std::mem::zeroed::<OmrStats>();
            omr_session_stats(m, b, &mut sb);
            println!("END backup state={} sent={} bytes={} queued={} dropped_v={} reconnects={}",
                sb.state, sb.packets_sent, sb.bytes_sent, sb.queued_bytes, sb.dropped_video_frames, sb.reconnects);
            let mut e = [0 as std::os::raw::c_char; 256];
            if omr_session_last_error(m, b, e.as_mut_ptr(), 256) > 0 {
                println!("backup last error: {}", std::ffi::CStr::from_ptr(e.as_ptr()).to_string_lossy());
            }
        }
        omr_manager_free(m);
    }
}
