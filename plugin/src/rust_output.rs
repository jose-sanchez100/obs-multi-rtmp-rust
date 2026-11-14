//! OBS output "multi_rtmp_rust_output": OBS only encodes and hands over each
//! packet; the plugin opens its own RTMP/RTMPS sessions (primary + optional
//! backup, byte-identical) in the obs-rtmp-sessions engine.
//!
//! Destinations: from the assigned service (multi_rtmp_rust_service: server, key,
//! backup_server, backup_key), like rtmp_output with rtmp_custom. If backup_key
//! is empty, key is reused (the YouTube case). For compatibility, if the service
//! has no server, those keys are read from the output settings (unmigrated
//! version 0.2.0 config).
//!
//! Output settings: per-stream engine options (drop threshold, reconnect on
//! congestion, IP family, local IP, buffer, retries), like rtmp_output's but
//! for Live and Backup.
use crate::module::obs_module_text;
use crate::obs::{c, cstr, sys};
use obs_rtmp_sessions::ffi::{add_rtmp_session_with, OmrStreamInfo};
use obs_rtmp_sessions::{flv, IpFamily, NetOptions, SessionConfig, SessionManager};
use std::time::Duration;
pub use obs_rtmp_sessions::SessionState;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const OUTPUT_ID: &std::ffi::CStr = c"multi_rtmp_rust_output";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Primary,
    Backup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Pause,
    Resume,
    Restart,
}

/// Per-stream status for the UI. `backup` is None if there is no backup.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub primary: Option<obs_rtmp_sessions::SessionSnapshot>,
    pub backup: Option<obs_rtmp_sessions::SessionSnapshot>,
}

/// Advanced menu option: lets streams reconnect on sustained congestion, after
/// the time set per stream in the target's Output settings. Off by default.
static CONGESTION_RECONNECT: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Engine {
    mgr: Option<Arc<SessionManager>>,
    primary: u64,
    backup: u64,
    /// Reconnect-on-congestion windows of the Live and Backup streams, from the
    /// output settings; applied only while CONGESTION_RECONNECT is on.
    congestion_window: [Duration; 2],
    origin_us: Option<i64>,
    headers_sent: bool,
    running: bool,
}

struct Ctx {
    output: *mut sys::obs_output_t,
    /// Guards the manager during capture, stop and stats reads.
    engine: Mutex<Engine>,
    /// Target name, for the sessions' log messages.
    label: Mutex<String>,
}

// The OBS pointer is only used as an identity and in thread-safe libobs calls.
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

fn registry() -> &'static Mutex<HashMap<usize, Arc<Ctx>>> {
    static R: OnceLock<Mutex<HashMap<usize, Arc<Ctx>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn find(output: *mut sys::obs_output_t) -> Option<Arc<Ctx>> {
    registry().lock().unwrap().get(&(output as usize)).cloned()
}

impl Ctx {
    /// Joining the network threads (bounded by socket timeouts) happens OUTSIDE
    /// the mutex: the UI never waits on a slow network when reading stats.
    fn free_manager(&self) {
        let mgr = {
            let mut e = self.engine.lock().unwrap();
            e.running = false;
            e.primary = 0;
            e.backup = 0;
            e.mgr.take()
        };
        if let Some(m) = mgr {
            m.shutdown();
        }
    }

    fn fail(&self, msg: &str) -> bool {
        let m = c(msg);
        unsafe { sys::obs_output_set_last_error(self.output, m.as_ptr()) };
        crate::error!("rust output: {msg}");
        false
    }
}

unsafe extern "C" fn get_name(_: *mut c_void) -> *const c_char {
    c"RTMP/RTMPS (Rust session engine)".as_ptr()
}

unsafe extern "C" fn create(_: *mut sys::obs_data_t, output: *mut sys::obs_output_t) -> *mut c_void {
    let ctx = Arc::new(Ctx { output, engine: Mutex::new(Engine::default()), label: Mutex::new(String::new()) });
    registry().lock().unwrap().insert(output as usize, ctx.clone());
    Arc::into_raw(ctx) as *mut c_void
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    let ctx = Arc::from_raw(data as *const Ctx);
    registry().lock().unwrap().remove(&(ctx.output as usize));
    ctx.free_manager();
}

// Engine options (the editor's "Output" tab), separately for each stream:
// "live_*" and "backup_*" keys. Custom prefixes so as not to inherit rtmp_output
// keys (drop_threshold_ms, bind_ip...) left over from when the target was RTMP.
const DEF_DROP_THRESHOLD_MS: i64 = 4000;
const DEF_BUFFER_MB: i64 = 2;
const DEF_RETRY_MAX_S: i64 = 30;
const DEF_MAX_RETRIES: i64 = 0;
/// Seconds of sustained drops before reconnecting (0 = never).
const DEF_CONGESTION_RECONNECT_S: i64 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prefix {
    Live,
    Backup,
}

impl Prefix {
    const ALL: [Prefix; 2] = [Prefix::Live, Prefix::Backup];

    fn key(self, name: &str) -> std::ffi::CString {
        c(&format!("{}_{name}", if self == Prefix::Live { "live" } else { "backup" }))
    }
}

unsafe fn t(key: &std::ffi::CStr) -> *const c_char {
    obs_module_text(key.as_ptr())
}

/// Local IPs for "Bind IP", in the same format as rtmp_output: "[adapter] ip".
fn local_addresses() -> Vec<(String, String)> {
    let Ok(ifs) = if_addrs::get_if_addrs() else { return Vec::new() };
    ifs.into_iter()
        .filter(|i| !i.is_loopback())
        .map(|i| (format!("[{}] {}", i.name, i.ip()), i.ip().to_string()))
        .collect()
}

unsafe extern "C" fn get_properties(_: *mut c_void) -> *mut sys::obs_properties_t {
    let p = sys::obs_properties_create();
    let addrs = local_addresses();
    for prefix in Prefix::ALL {
        let header = if prefix == Prefix::Live { c"RustOutput.Live" } else { c"RustOutput.Backup" };
        sys::obs_properties_add_text(p, prefix.key("header").as_ptr(), t(header), sys::OBS_TEXT_INFO);
        sys::obs_properties_add_int(p, prefix.key("drop_threshold_ms").as_ptr(), t(c"RustOutput.DropThreshold"), 0, 30000, 100);
        let congestion = sys::obs_properties_add_int(
            p,
            prefix.key("congestion_reconnect_s").as_ptr(),
            t(c"RustOutput.CongestionReconnect"),
            0,
            600,
            5,
        );
        sys::obs_property_set_long_description(congestion, t(c"RustOutput.CongestionReconnect.Tooltip"));

        let fam = sys::obs_properties_add_list(
            p,
            prefix.key("ip_family").as_ptr(),
            t(c"RustOutput.IPFamily"),
            sys::OBS_COMBO_TYPE_LIST,
            sys::OBS_COMBO_FORMAT_STRING,
        );
        sys::obs_property_list_add_string(fam, t(c"RustOutput.IPFamily.Both"), c"IPv4+IPv6".as_ptr());
        sys::obs_property_list_add_string(fam, t(c"RustOutput.IPFamily.V4Only"), c"IPv4".as_ptr());
        sys::obs_property_list_add_string(fam, t(c"RustOutput.IPFamily.V6Only"), c"IPv6".as_ptr());

        let bind = sys::obs_properties_add_list(
            p,
            prefix.key("bind_ip").as_ptr(),
            t(c"RustOutput.BindIP"),
            sys::OBS_COMBO_TYPE_LIST,
            sys::OBS_COMBO_FORMAT_STRING,
        );
        sys::obs_property_list_add_string(bind, t(c"RustOutput.BindIP.Default"), c"default".as_ptr());
        for (label, ip) in &addrs {
            sys::obs_property_list_add_string(bind, c(label).as_ptr(), c(ip).as_ptr());
        }

        sys::obs_properties_add_int(p, prefix.key("buffer_mb").as_ptr(), t(c"RustOutput.BufferMB"), 1, 64, 1);
        sys::obs_properties_add_int(p, prefix.key("retry_max_s").as_ptr(), t(c"RustOutput.RetryMax"), 1, 300, 1);
        sys::obs_properties_add_int(p, prefix.key("max_retries").as_ptr(), t(c"RustOutput.MaxRetries"), 0, 1000, 1);
    }
    p
}

unsafe extern "C" fn get_defaults(settings: *mut sys::obs_data_t) {
    for prefix in Prefix::ALL {
        sys::obs_data_set_default_int(settings, prefix.key("drop_threshold_ms").as_ptr(), DEF_DROP_THRESHOLD_MS);
        sys::obs_data_set_default_string(settings, prefix.key("ip_family").as_ptr(), c"IPv4+IPv6".as_ptr());
        sys::obs_data_set_default_string(settings, prefix.key("bind_ip").as_ptr(), c"default".as_ptr());
        sys::obs_data_set_default_int(settings, prefix.key("buffer_mb").as_ptr(), DEF_BUFFER_MB);
        sys::obs_data_set_default_int(settings, prefix.key("retry_max_s").as_ptr(), DEF_RETRY_MAX_S);
        sys::obs_data_set_default_int(settings, prefix.key("max_retries").as_ptr(), DEF_MAX_RETRIES);
        sys::obs_data_set_default_int(settings, prefix.key("congestion_reconnect_s").as_ptr(), DEF_CONGESTION_RECONNECT_S);
    }
}

/// Session configuration for one stream, built from the output settings.
unsafe fn session_config(settings: *mut sys::obs_data_t, prefix: Prefix) -> SessionConfig {
    let int = |k: &str, lo: i64, hi: i64| sys::obs_data_get_int(settings, prefix.key(k).as_ptr()).clamp(lo, hi);
    let mut cfg = SessionConfig::default();
    let soft = int("buffer_mb", 1, 64) as usize * 1024 * 1024;
    // Hard limit (also drops audio) = 3x the soft one, as in the defaults.
    cfg.limits.soft_bytes = soft;
    cfg.limits.hard_bytes = soft * 3;
    cfg.limits.max_latency_ms = int("drop_threshold_ms", 0, 30000) as u32;
    cfg.backoff_max = Duration::from_secs(int("retry_max_s", 1, 300) as u64);
    cfg.backoff_min = cfg.backoff_min.min(cfg.backoff_max);
    cfg.max_retries = int("max_retries", 0, 1000) as u32;
    cfg.congestion_window = Duration::from_secs(int("congestion_reconnect_s", 0, 600) as u64);
    cfg
}

/// Network options for one stream (IP family and local IP).
unsafe fn net_options(settings: *mut sys::obs_data_t, prefix: Prefix) -> NetOptions {
    let family = match setting(settings, &prefix.key("ip_family")).as_str() {
        "IPv4" => IpFamily::V4,
        "IPv6" => IpFamily::V6,
        _ => IpFamily::Any,
    };
    let bind = setting(settings, &prefix.key("bind_ip"));
    let bind_ip = match bind.as_str() {
        "" | "default" => None,
        ip => ip.parse().map_err(|_| crate::warn!("rust output: invalid bind IP '{ip}', using default")).ok(),
    };
    NetOptions { family, bind_ip, ..Default::default() }
}

unsafe fn encoder_bitrate(encoder: *mut sys::obs_encoder_t) -> u32 {
    let s = sys::obs_encoder_get_settings(encoder);
    let b = sys::obs_data_get_int(s, c"bitrate".as_ptr());
    sys::obs_data_release(s);
    u32::try_from(b).ok().filter(|b| *b > 0).unwrap_or(0)
}

unsafe fn stream_info(video: *mut sys::obs_encoder_t, audio: *mut sys::obs_encoder_t) -> OmrStreamInfo {
    let divisor = sys::obs_encoder_get_frame_rate_divisor(video).max(1);
    OmrStreamInfo {
        width: sys::obs_encoder_get_width(video),
        height: sys::obs_encoder_get_height(video),
        frame_rate: (sys::video_output_get_frame_rate(sys::obs_encoder_video(video)) / divisor as f64) as f32,
        video_bitrate_kbps: encoder_bitrate(video),
        audio_bitrate_kbps: encoder_bitrate(audio),
        audio_sample_rate: sys::obs_encoder_get_sample_rate(audio),
        audio_channels: sys::audio_output_get_channels(sys::obs_encoder_audio(audio)) as u32,
    }
}

unsafe fn setting(settings: *mut sys::obs_data_t, key: &std::ffi::CStr) -> String {
    cstr(sys::obs_data_get_string(settings, key.as_ptr()))
}

unsafe extern "C" fn start(data: *mut c_void) -> bool {
    let ctx = &*(data as *const Ctx);
    let out = ctx.output;
    if !sys::obs_output_can_begin_data_capture(out, 0) {
        return false;
    }
    let venc = sys::obs_output_get_video_encoder(out);
    let aenc = sys::obs_output_get_audio_encoder(out, 0);
    if venc.is_null() || aenc.is_null() {
        return false;
    }
    // The engine's FLV mux only covers H.264 + AAC.
    let (vcodec, acodec) = (cstr(sys::obs_encoder_get_codec(venc)), cstr(sys::obs_encoder_get_codec(aenc)));
    if vcodec != "h264" || acodec != "aac" {
        return ctx.fail(&format!("Rust session engine only supports H.264 video and AAC audio (got {vcodec}/{acodec})"));
    }

    // Destinations: from the assigned service; old configs: from the output settings.
    let settings = sys::obs_output_get_settings(out);
    let svc = sys::obs_output_get_service(out);
    let svc_settings = if svc.is_null() { ptr::null_mut() } else { sys::obs_service_get_settings(svc) };
    let from_service = !svc_settings.is_null() && !setting(svc_settings, c"server").is_empty();
    let dest = if from_service { svc_settings } else { settings };
    let url = setting(dest, c"server");
    let key = setting(dest, c"key");
    let burl = setting(dest, c"backup_server");
    let mut bkey = setting(dest, c"backup_key");
    let label = ctx.label.lock().unwrap().clone();
    let opts = Prefix::ALL.map(|p| {
        let mut cfg = session_config(settings, p);
        let slot = if p == Prefix::Live { "Live" } else { "Backup" };
        cfg.label = if label.is_empty() { slot.to_owned() } else { format!("{label} / {slot}") };
        (cfg, net_options(settings, p))
    });
    let congestion_window = opts.each_ref().map(|(cfg, _)| cfg.congestion_window);
    let mut opts = opts;
    if !CONGESTION_RECONNECT.load(Ordering::Relaxed) {
        for (cfg, _) in &mut opts {
            cfg.congestion_window = Duration::ZERO;
        }
    }
    if !svc_settings.is_null() {
        sys::obs_data_release(svc_settings);
    }
    sys::obs_data_release(settings);
    if bkey.is_empty() {
        bkey = key.clone();
    }
    if url.is_empty() {
        return ctx.fail("Missing server URL");
    }
    if !sys::obs_output_initialize_encoders(out, 0) {
        return false;
    }

    ctx.free_manager();
    let info = stream_info(venc, aenc);
    {
        let mut e = ctx.engine.lock().unwrap();
        let mgr = Arc::new(SessionManager::new());
        let [(live_cfg, live_net), (backup_cfg, backup_net)] = opts;
        let Some(primary) = add_rtmp_session_with(&mgr, &url, Some(&key), live_cfg, live_net, Some(&info)) else {
            drop(e);
            mgr.shutdown();
            return ctx.fail("Invalid RTMP/RTMPS URL or stream key");
        };
        let mut backup = 0;
        if !burl.is_empty() {
            match add_rtmp_session_with(&mgr, &burl, Some(&bkey), backup_cfg, backup_net, Some(&info)) {
                Some(id) => backup = id,
                None => {
                    drop(e);
                    mgr.shutdown();
                    return ctx.fail("Invalid backup RTMP/RTMPS URL");
                }
            }
        }
        *e = Engine {
            mgr: Some(mgr),
            primary,
            backup,
            congestion_window,
            origin_us: None,
            headers_sent: false,
            running: true,
        };
    }

    if !sys::obs_output_begin_data_capture(out, 0) {
        ctx.free_manager();
        return false;
    }
    true
}

unsafe extern "C" fn stop(data: *mut c_void, _ts: u64) {
    let ctx = &*(data as *const Ctx);
    sys::obs_output_end_data_capture(ctx.output);
    {
        let mut e = ctx.engine.lock().unwrap();
        e.running = false;
        if let Some(m) = &e.mgr {
            m.remove(e.primary); // does not wait on the network
            if e.backup != 0 {
                m.remove(e.backup);
            }
            m.clear_headers();
        }
    }
    ctx.free_manager(); // joins the threads; bounded by socket timeouts
}

/// NVENC gets SPS/PPS when encoding the first packet; libobs delivers interleaved
/// packets once both encoders have produced data.
unsafe fn send_headers(out: *mut sys::obs_output_t, mgr: &SessionManager) -> Result<(), &'static str> {
    let video = sys::obs_output_get_video_encoder(out);
    let (mut extra, mut size) = (ptr::null_mut::<u8>(), 0usize);
    if !sys::obs_encoder_get_extra_data(video, &mut extra, &mut size) || extra.is_null() {
        return Err("Could not read H.264 SPS/PPS from the encoder");
    }
    // Same as the native RTMP output, including the High profile extension.
    let mut avc: *mut u8 = ptr::null_mut();
    let avc_size = sys::obs_parse_avc_header(&mut avc, extra, size);
    let header = (!avc.is_null()).then(|| flv::video_header(std::slice::from_raw_parts(avc, avc_size))).flatten();
    sys::bfree(avc as *mut c_void);
    mgr.set_header(header.ok_or("Could not build H.264 AVC configuration from the encoder")?);

    let audio = sys::obs_output_get_audio_encoder(out, 0);
    let (mut extra, mut size) = (ptr::null_mut::<u8>(), 0usize);
    if !sys::obs_encoder_get_extra_data(audio, &mut extra, &mut size) || extra.is_null() || size == 0 {
        return Err("Could not read AAC AudioSpecificConfig from the encoder");
    }
    mgr.set_header(flv::audio_header(std::slice::from_raw_parts(extra, size)));
    Ok(())
}

/// Copies the packet once into the engine, without network I/O.
unsafe extern "C" fn encoded_packet(data: *mut c_void, packet: *mut sys::encoder_packet) {
    let ctx = &*(data as *const Ctx);
    if packet.is_null() {
        sys::obs_output_signal_stop(ctx.output, sys::OBS_OUTPUT_ENCODE_ERROR);
        return;
    }
    let p = &*packet;
    let mut e = ctx.engine.lock().unwrap();
    if !e.running {
        return;
    }
    let Some(mgr) = e.mgr.clone() else { return };

    if !e.headers_sent {
        if let Err(msg) = send_headers(ctx.output, &mgr) {
            e.running = false;
            drop(e);
            ctx.fail(msg);
            sys::obs_output_signal_stop(ctx.output, sys::OBS_OUTPUT_ENCODE_ERROR);
            return;
        }
        e.headers_sent = true;
    }

    let origin = *e.origin_us.get_or_insert(p.dts_usec);
    let dts_ms = ((p.dts_usec - origin) / 1000).max(0);
    let bytes = std::slice::from_raw_parts(p.data, p.size);
    if p.type_ == sys::OBS_ENCODER_VIDEO {
        let mut cts_ms = 0;
        if p.timebase_den > 0 {
            // OBS already folds the timebase numerator into pts/dts (same as flv-mux.c).
            let den = p.timebase_den as i64;
            cts_ms = p.pts * 1000 / den - p.dts * 1000 / den;
        }
        if let Some(pkt) = flv::video_frame(dts_ms, dts_ms + cts_ms, bytes) {
            mgr.push_all(&pkt);
        }
    } else if p.track_idx == 0 {
        mgr.push_all(&flv::audio_frame(dts_ms, bytes));
    }
}

fn primary_snapshot(data: *mut c_void) -> Option<obs_rtmp_sessions::SessionSnapshot> {
    let ctx = unsafe { &*(data as *const Ctx) };
    let e = ctx.engine.lock().unwrap();
    e.mgr.as_ref()?.snapshot(e.primary)
}

unsafe extern "C" fn get_total_bytes(data: *mut c_void) -> u64 {
    primary_snapshot(data).map(|s| s.bytes_sent).unwrap_or(0)
}

unsafe extern "C" fn get_dropped_frames(data: *mut c_void) -> c_int {
    primary_snapshot(data).map(|s| s.drops.dropped_video_frames as c_int).unwrap_or(0)
}

pub fn congestion_reconnect() -> bool {
    CONGESTION_RECONNECT.load(Ordering::Relaxed)
}

/// Turns reconnect on congestion on or off for every Rust output, including
/// the streams that are live now.
pub fn set_congestion_reconnect(enabled: bool) {
    if CONGESTION_RECONNECT.swap(enabled, Ordering::Relaxed) == enabled {
        return;
    }
    crate::info!("rust output: reconnect on congestion {}", if enabled { "enabled" } else { "disabled" });
    let contexts: Vec<Arc<Ctx>> = registry().lock().unwrap().values().cloned().collect();
    for ctx in contexts {
        let e = ctx.engine.lock().unwrap();
        let Some(mgr) = e.mgr.as_ref().filter(|_| e.running) else { continue };
        for (id, window) in [(e.primary, e.congestion_window[0]), (e.backup, e.congestion_window[1])] {
            if id != 0 {
                mgr.set_congestion_window(id, if enabled { window } else { Duration::ZERO });
            }
        }
    }
}

/// Names the output's sessions in the log (the target name). Call before starting.
pub fn set_label(output: *mut sys::obs_output_t, label: &str) {
    if let Some(ctx) = find(output) {
        *ctx.label.lock().unwrap() = label.to_owned();
    }
}

/// None if the output is not of this type or is not streaming.
pub fn status(output: *mut sys::obs_output_t) -> Option<Status> {
    let ctx = find(output)?;
    let e = ctx.engine.lock().unwrap();
    let mgr = e.mgr.as_ref()?;
    if e.primary == 0 {
        return None;
    }
    let backup = if e.backup != 0 { mgr.snapshot(e.backup) } else { None };
    Some(Status { primary: mgr.snapshot(e.primary), backup })
}

/// Manual control of each session (primary/backup). Non-blocking.
pub fn control(output: *mut sys::obs_output_t, slot: Slot, action: Action) -> bool {
    let Some(ctx) = find(output) else { return false };
    let e = ctx.engine.lock().unwrap();
    let id = if slot == Slot::Primary { e.primary } else { e.backup };
    let (Some(mgr), true, true) = (e.mgr.as_ref(), e.running, id != 0) else { return false };
    let ok = match action {
        Action::Pause => mgr.pause(id),
        Action::Resume => mgr.resume(id),
        Action::Restart => mgr.restart(id),
    };
    crate::info!("rust output: {} / {:?}: {:?} requested by the user", ctx.label.lock().unwrap(), slot, action);
    ok
}

pub fn register() {
    let info = sys::obs_output_info {
        id: OUTPUT_ID.as_ptr(),
        // OBS_OUTPUT_SERVICE: destinations come from the assigned service; OBS itself manages no connection.
        flags: sys::OBS_OUTPUT_AV | sys::OBS_OUTPUT_ENCODED | sys::OBS_OUTPUT_SERVICE,
        get_name: Some(get_name),
        create: Some(create),
        destroy: Some(destroy),
        start: Some(start),
        stop: Some(stop),
        raw_video: None,
        raw_audio: None,
        encoded_packet: Some(encoded_packet),
        update: None,
        get_defaults: Some(get_defaults),
        get_properties: Some(get_properties),
        unused1: None,
        get_total_bytes: Some(get_total_bytes),
        get_dropped_frames: Some(get_dropped_frames),
        type_data: ptr::null_mut(),
        free_type_data: None,
        get_congestion: None,
        get_connect_time_ms: None,
        encoded_video_codecs: c"h264".as_ptr(),
        encoded_audio_codecs: c"aac".as_ptr(),
        raw_audio2: None,
        protocols: crate::rust_service::PROTOCOL.as_ptr(),
    };
    unsafe { sys::obs_register_output_s(&info, std::mem::size_of::<sys::obs_output_info>()) };
}
