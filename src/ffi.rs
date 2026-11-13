//! C ABI surface for the existing C++/Qt plugin (or a future pure-Rust plugin).
//! Intended use: a dedicated `obs_output` ("multi_rtmp_session") whose
//! `encoded_packet` callback converts the `encoder_packet` into an FLV tag and
//! calls `omr_push`; this call never touches the network.

use crate::manager::SessionManager;
use crate::queue::{Kind, MediaPacket};
use crate::rtmp::{NetOptions, RtmpTransport, RtmpUrl};
use crate::session::{SessionConfig, SessionState};
use bytes::Bytes;
use rml_rtmp::sessions::StreamMetadata;
use std::ffi::{c_char, CStr};
use std::os::raw::c_int;

#[repr(C)]
pub struct OmrStats {
    pub state: u8, // 0 idle,1 connecting,2 live,3 backoff,4 stopped,5 paused
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub reconnects: u64,
    pub slow_sends: u64,
    pub queued_bytes: u64,
    pub dropped_video_frames: u64,
    pub dropped_audio_frames: u64,
    pub dropped_gops: u64,
}

/// H.264 and AAC encoder metadata. Optional fields may be zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct OmrStreamInfo {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f32,
    pub video_bitrate_kbps: u32,
    pub audio_bitrate_kbps: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
}

impl From<&OmrStreamInfo> for StreamMetadata {
    fn from(info: &OmrStreamInfo) -> Self {
        let nonzero = |value| Some(value).filter(|v| *v != 0);
        Self {
            video_width: nonzero(info.width),
            video_height: nonzero(info.height),
            video_codec_id: Some(7), // AVC
            video_frame_rate: Some(info.frame_rate).filter(|v| v.is_finite() && *v > 0.0),
            video_bitrate_kbps: nonzero(info.video_bitrate_kbps),
            audio_codec_id: Some(10), // AAC
            audio_bitrate_kbps: nonzero(info.audio_bitrate_kbps),
            audio_sample_rate: nonzero(info.audio_sample_rate),
            audio_channels: nonzero(info.audio_channels),
            audio_is_stereo: nonzero(info.audio_channels).map(|channels| channels == 2),
            encoder: Some("obs-multi-rtmp (Rust)".into()),
        }
    }
}

#[no_mangle]
pub extern "C" fn omr_manager_new() -> *mut SessionManager {
    Box::into_raw(Box::new(SessionManager::new()))
}

/// # Safety
/// `m` must come from `omr_manager_new` and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn omr_manager_free(m: *mut SessionManager) {
    if !m.is_null() {
        let m = Box::from_raw(m);
        m.shutdown();
    }
}

/// Returns the session id (>0), or 0 if the URL is invalid.
///
/// # Safety
/// `url` and `key` must be valid C strings (`key` may be NULL).
#[no_mangle]
pub unsafe extern "C" fn omr_session_add(
    m: *mut SessionManager,
    url: *const c_char,
    key: *const c_char,
    soft_bytes: usize,
    hard_bytes: usize,
) -> u64 {
    omr_session_add_with_info(m, url, key, soft_bytes, hard_bytes, std::ptr::null())
}

/// Same as `omr_session_add`, with metadata announced on every connection.
///
/// # Safety
/// `m`, `url` and `key` follow the contract of `omr_session_add`.
/// `info` may be NULL or point to an `OmrStreamInfo` readable for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn omr_session_add_with_info(
    m: *mut SessionManager,
    url: *const c_char,
    key: *const c_char,
    soft_bytes: usize,
    hard_bytes: usize,
    info: *const OmrStreamInfo,
) -> u64 {
    if m.is_null() || url.is_null() {
        return 0;
    }
    let url = CStr::from_ptr(url).to_string_lossy();
    let key = (!key.is_null()).then(|| CStr::from_ptr(key).to_string_lossy().into_owned());
    add_rtmp_session(&*m, &url, key.as_deref(), soft_bytes, hard_bytes, info.as_ref()).unwrap_or(0)
}

/// Rust version of `omr_session_add_with_info` (used directly by the Rust plugin).
/// `None` if the URL is invalid. Limits of 0 = defaults.
pub fn add_rtmp_session(
    m: &SessionManager,
    url: &str,
    key: Option<&str>,
    soft_bytes: usize,
    hard_bytes: usize,
    info: Option<&OmrStreamInfo>,
) -> Option<u64> {
    let mut cfg = SessionConfig::default();
    if soft_bytes > 0 && hard_bytes >= soft_bytes {
        cfg.limits.soft_bytes = soft_bytes;
        cfg.limits.hard_bytes = hard_bytes;
    }
    add_rtmp_session_with(m, url, key, cfg, NetOptions::default(), info)
}

/// Like `add_rtmp_session`, with the full session configuration (queue limits,
/// maximum latency, retries) and its network options.
pub fn add_rtmp_session_with(
    m: &SessionManager,
    url: &str,
    key: Option<&str>,
    cfg: SessionConfig,
    net: NetOptions,
    info: Option<&OmrStreamInfo>,
) -> Option<u64> {
    let parsed = RtmpUrl::parse(url, key).ok()?;
    let metadata = info.map(StreamMetadata::from);
    Some(m.add(
        cfg,
        Box::new(move || Box::new(RtmpTransport::new(parsed.clone()).with_metadata(metadata.clone()).with_net(net.clone()))),
    ))
}

#[no_mangle]
pub unsafe extern "C" fn omr_session_remove(m: *mut SessionManager, id: u64) -> c_int {
    (!m.is_null() && (*m).remove(id)) as c_int
}

/// Stops sending on a session without destroying it (state 5). Non-blocking.
#[no_mangle]
pub unsafe extern "C" fn omr_session_pause(m: *mut SessionManager, id: u64) -> c_int {
    (!m.is_null() && (*m).pause(id)) as c_int
}

/// Resumes a paused session: reconnects and rejoins at the next keyframe. Non-blocking.
#[no_mangle]
pub unsafe extern "C" fn omr_session_resume(m: *mut SessionManager, id: u64) -> c_int {
    (!m.is_null() && (*m).resume(id)) as c_int
}

/// Forces an immediate reconnect of a session (even if it was paused). Non-blocking.
#[no_mangle]
pub unsafe extern "C" fn omr_session_restart(m: *mut SessionManager, id: u64) -> c_int {
    (!m.is_null() && (*m).restart(id)) as c_int
}

/// kind: 0 = video, 1 = audio. Copies the data once and shares it across sessions.
///
/// # Safety
/// `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn omr_push(
    m: *mut SessionManager,
    kind: c_int,
    dts_ms: u32,
    keyframe: c_int,
    is_header: c_int,
    data: *const u8,
    len: usize,
) {
    if m.is_null() || data.is_null() {
        return;
    }
    let pkt = MediaPacket {
        kind: if kind == 0 { Kind::Video } else { Kind::Audio },
        dts_ms,
        keyframe: keyframe != 0,
        is_header: is_header != 0,
        data: Bytes::copy_from_slice(std::slice::from_raw_parts(data, len)),
    };
    (*m).push_all(&pkt);
}

#[no_mangle]
pub unsafe extern "C" fn omr_session_stats(m: *mut SessionManager, id: u64, out: *mut OmrStats) -> c_int {
    if m.is_null() || out.is_null() {
        return 0;
    }
    match (*m).snapshot(id) {
        Some(s) => {
            *out = OmrStats {
                state: s.state as u8,
                packets_sent: s.packets_sent,
                bytes_sent: s.bytes_sent,
                reconnects: s.reconnects,
                slow_sends: s.slow_sends,
                queued_bytes: s.queued_bytes as u64,
                dropped_video_frames: s.drops.dropped_video_frames,
                dropped_audio_frames: s.drops.dropped_audio_frames,
                dropped_gops: s.drops.dropped_gops,
            };
            let _ = SessionState::Idle;
            1
        }
        None => 0,
    }
}

// ---- Encoded packets exactly as OBS delivers them (H.264 Annex-B + raw AAC) ----

/// SPS+PPS in Annex-B, or an AVCDecoderConfigurationRecord with 4-byte lengths.
/// # Safety
/// `data` points to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn omr_set_video_extradata(m: *mut SessionManager, data: *const u8, len: usize) -> c_int {
    if m.is_null() || data.is_null() {
        return 0;
    }
    match crate::flv::video_header(std::slice::from_raw_parts(data, len)) {
        Some(h) => {
            (*m).set_header(h);
            1
        }
        None => 0,
    }
}

/// AudioSpecificConfig (`obs_encoder_get_extra_data` of the AAC encoder).
/// # Safety
/// `data` points to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn omr_set_audio_extradata(m: *mut SessionManager, data: *const u8, len: usize) -> c_int {
    if m.is_null() || data.is_null() || len == 0 {
        return 0;
    }
    (*m).set_header(crate::flv::audio_header(std::slice::from_raw_parts(data, len)));
    1
}

/// # Safety
/// `data` points to `len` readable bytes (Annex-B packet).
#[no_mangle]
pub unsafe extern "C" fn omr_push_video(m: *mut SessionManager, dts_ms: i64, pts_ms: i64, data: *const u8, len: usize) {
    if m.is_null() || data.is_null() {
        return;
    }
    if let Some(p) = crate::flv::video_frame(dts_ms, pts_ms, std::slice::from_raw_parts(data, len)) {
        (*m).push_all(&p);
    }
}

/// # Safety
/// `data` points to `len` readable bytes (raw AAC).
#[no_mangle]
pub unsafe extern "C" fn omr_push_audio(m: *mut SessionManager, dts_ms: i64, data: *const u8, len: usize) {
    if m.is_null() || data.is_null() {
        return;
    }
    (*m).push_all(&crate::flv::audio_frame(dts_ms, std::slice::from_raw_parts(data, len)));
}

/// Forgets the sequence headers when streaming ends.
#[no_mangle]
pub unsafe extern "C" fn omr_clear_headers(m: *mut SessionManager) {
    if !m.is_null() {
        (*m).clear_headers();
    }
}

/// Copies the session's last error (UTF-8, NUL-terminated) into `buf`.
/// Returns the length excluding the NUL, or 0 if there is no error or the session does not exist.
///
/// # Safety
/// `buf` points to `cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn omr_session_last_error(m: *mut SessionManager, id: u64, buf: *mut c_char, cap: usize) -> usize {
    if m.is_null() || buf.is_null() || cap == 0 {
        return 0;
    }
    let msg = (*m).snapshot(id).and_then(|s| s.last_error).unwrap_or_default();
    let n = msg.len().min(cap - 1);
    std::ptr::copy_nonoverlapping(msg.as_ptr(), buf as *mut u8, n);
    *buf.add(n) = 0;
    n
}
