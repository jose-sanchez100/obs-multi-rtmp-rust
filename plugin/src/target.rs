//! Runtime of a target: creates the OBS output (and its service), obtains or
//! shares encoders, connects the signals and keeps the state the dock renders.
//! Everything runs on the UI thread; libobs signals arrive queued.
use crate::config::{self, Config, Target};
use crate::module::text;
use crate::obs::{c, cstr, sys, Data, Encoder, Output, Service};
use crate::protocols;
use crate::rust_output::{self, SessionState};
use crate::scene_view::{self, SceneView};
use crate::ui;
use serde_json::{json, Value};
use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

static GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
pub enum Signal {
    Starting,
    Start,
    Reconnect,
    ReconnectSuccess,
    Stopping,
    Stop(i64),
}

/// Data for the connected signals; freed after disconnecting them.
struct SignalCtx {
    target_id: String,
    generation: u64,
}

macro_rules! signal_trampoline {
    ($name:ident, $kind:expr) => {
        unsafe extern "C" fn $name(data: *mut c_void, cd: *mut sys::calldata_t) {
            let ctx = &*(data as *const SignalCtx);
            #[allow(unused_mut)]
            let mut code: i64 = 0; // long long in calldata
            let _ = cd;
            let kind: fn(i64) -> Signal = $kind;
            if matches!(kind(0), Signal::Stop(_)) {
                sys::calldata_get_data(cd, c"code".as_ptr(), &mut code as *mut i64 as *mut c_void, std::mem::size_of::<i64>());
            }
            let (id, generation, sig) = (ctx.target_id.clone(), ctx.generation, kind(code as i64));
            ui::post(move || crate::core::on_signal(&id, generation, sig));
        }
    };
}
signal_trampoline!(sig_starting, |_| Signal::Starting);
signal_trampoline!(sig_start, |_| Signal::Start);
signal_trampoline!(sig_reconnect, |_| Signal::Reconnect);
signal_trampoline!(sig_reconnect_success, |_| Signal::ReconnectSuccess);
signal_trampoline!(sig_stopping, |_| Signal::Stopping);
signal_trampoline!(sig_stop, Signal::Stop);

const SIGNALS: [(&CStr, sys::signal_callback_t); 6] = [
    (c"starting", sig_starting),
    (c"start", sig_start),
    (c"reconnect", sig_reconnect),
    (c"reconnect_success", sig_reconnect_success),
    (c"stopping", sig_stopping),
    (c"stop", sig_stop),
];

pub struct Runtime {
    pub id: String,
    output: Option<Output>,
    /// The output only holds a weak pointer to the service: we own the reference
    /// and release it AFTER the output is destroyed.
    service: Option<Service>,
    scene_view: Option<Arc<SceneView>>,
    signal_ctx: Option<Box<SignalCtx>>,
    generation: u64,

    pub busy: bool,
    starting: bool,
    started: bool,
    connecting: bool,
    reconnecting: bool,
    pub use_delay: bool,
    last_error_code: i64,
    pub msg: String,
    tooltip: String,

    begin: Instant,
    last_info: Instant,
    total_bytes: u64,
    total_frames: i32,
    timer: bool,
    rust_degraded: bool,
    /// Per Rust stream (primary, backup): bytes at the previous tick, bitrate and state.
    slot_bytes: [u64; 2],
    slot_bps: [f64; 2],
    slot_frames: [u64; 2],
    slot_fps: [f64; 2],
    /// Rust targets: streaming time shown next to the name.
    clock: String,
    slot_state: [Option<SessionState>; 2],
    /// Since when each stream has been LIVE (reset on reconnect or resume).
    slot_live_since: [Option<Instant>; 2],

    pub hotkeys: crate::hotkeys::TargetHotkeys,
}

pub struct StartError(pub String);

fn obs_data(v: &Value) -> Option<Data> {
    match v {
        Value::Object(_) => Data::from_json(&v.to_string()),
        _ => None,
    }
}

fn data_ptr(d: &Option<Data>) -> *mut sys::obs_data_t {
    d.as_ref().map(Data::as_ptr).unwrap_or(std::ptr::null_mut())
}

/// `\s*(\d{1,5})\s*x\s*(\d{1,5})\s*` (full match, lowercase 'x' only).
pub fn parse_resolution(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.trim().split_once('x')?;
    let num = |p: &str| {
        let p = p.trim();
        (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()).then_some(()).is_some()
    };
    if num(w) && num(h) {
        Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
    } else {
        None
    }
}

fn main_output(recording: bool) -> Option<Output> {
    unsafe {
        Output::from_owned(if recording { sys::obs_frontend_get_recording_output() } else { sys::obs_frontend_get_streaming_output() })
    }
}

struct Encoders {
    video: Encoder,
    video_is_obs: bool,
    audio: Vec<(usize, Encoder)>,
    audio_is_obs: bool,
}

fn encoder_by_name(name: &str) -> Option<Encoder> {
    let n = c(name);
    unsafe { Encoder::from_owned(sys::obs_get_encoder_by_name(n.as_ptr())) }
}

fn video_encoder(t: &Target, cfg: &Config) -> Option<(Encoder, bool)> {
    match t.video_config.as_deref() {
        None | Some("") | Some(config::OBS_STREAMING_ENCODER) | Some(config::OBS_RECORDING_ENCODER) => {
            let rec = t.video_config.as_deref() == Some(config::OBS_RECORDING_ENCODER);
            let out = main_output(rec)?;
            unsafe { Encoder::from_borrowed(sys::obs_output_get_video_encoder(out.as_ptr())) }.map(|e| (e, true))
        }
        Some(id) => {
            let name = format!("multi-rtmp-venc{id}");
            if let Some(e) = encoder_by_name(&name) {
                return Some((e, false));
            }
            let Some(vc) = cfg.video_config(id) else {
                crate::warn!("video config {id} not found for target {}; using the OBS encoder", t.name);
                let mut t2 = t.clone();
                t2.video_config = Some(config::OBS_STREAMING_ENCODER.into());
                return video_encoder(&t2, cfg);
            };
            let settings = obs_data(&vc.param);
            let (eid, n) = (c(&vc.encoder), c(&name));
            let e = unsafe {
                Encoder::from_owned(sys::obs_video_encoder_create(eid.as_ptr(), n.as_ptr(), data_ptr(&settings), std::ptr::null_mut()))
            }?;
            unsafe {
                if let Some((w, h)) = vc.resolution.as_deref().and_then(parse_resolution) {
                    sys::obs_encoder_set_gpu_scale_type(e.as_ptr(), sys::OBS_SCALE_BICUBIC);
                    sys::obs_encoder_set_scaled_size(e.as_ptr(), w, h);
                }
                sys::obs_encoder_set_frame_rate_divisor(e.as_ptr(), vc.fps_divisor.clamp(1, 60) as u32);
            }
            Some((e, false))
        }
    }
}

fn audio_encoder(t: &Target, cfg: &Config, track: usize, mixer: Option<i64>) -> Option<(Encoder, bool)> {
    match t.audio_config.as_deref() {
        None | Some("") | Some(config::OBS_STREAMING_ENCODER) | Some(config::OBS_RECORDING_ENCODER) => {
            let rec = t.audio_config.as_deref() == Some(config::OBS_RECORDING_ENCODER);
            let out = main_output(rec)?;
            unsafe { Encoder::from_borrowed(sys::obs_output_get_audio_encoder(out.as_ptr(), track)) }.map(|e| (e, true))
        }
        Some(id) => {
            let name = format!("multi-rtmp-aenc{id}-track-idx-{track}");
            if let Some(e) = encoder_by_name(&name) {
                return Some((e, false));
            }
            let Some(ac) = cfg.audio_config(id) else {
                crate::warn!("audio config {id} not found for target {}; using the OBS encoder", t.name);
                let mut t2 = t.clone();
                t2.audio_config = Some(config::OBS_STREAMING_ENCODER.into());
                return audio_encoder(&t2, cfg, track, mixer);
            };
            let settings = obs_data(&ac.param);
            let (eid, n) = (c(&ac.encoder), c(&name));
            let mixer = mixer.unwrap_or(ac.mixer_id).clamp(0, 5) as usize;
            unsafe {
                Encoder::from_owned(sys::obs_audio_encoder_create(
                    eid.as_ptr(),
                    n.as_ptr(),
                    data_ptr(&settings),
                    mixer,
                    std::ptr::null_mut(),
                ))
            }
            .map(|e| (e, false))
        }
    }
}

fn prepare_encoders(t: &Target, cfg: &Config) -> Option<Encoders> {
    let (video, video_is_obs) = video_encoder(t, cfg)?;
    let (a0, audio_is_obs) = audio_encoder(t, cfg, 0, None)?;
    let mut audio = vec![(0, a0)];
    if !audio_is_obs {
        if let Some(ac) = t.audio_config.as_deref().and_then(|id| cfg.audio_config(id)) {
            for tr in &ac.tracks {
                let idx = tr.output_track.clamp(0, 5) as usize;
                if idx == 0 {
                    continue; // would collide with the main track
                }
                if let Some((e, _)) = audio_encoder(t, cfg, idx, Some(tr.mixer_track)) {
                    audio.push((idx, e));
                }
            }
        }
    }
    Some(Encoders { video, video_is_obs, audio, audio_is_obs })
}

fn profile_config() -> *mut sys::config_t {
    unsafe { sys::obs_frontend_get_profile_config() }
}

/// Bitrate format of the C++ plugin: "2.50 Mbps", "123 Mbps", "0 bps".
pub fn format_bitrate(bps: f64) -> String {
    const UNITS: [&str; 9] = ["bps", "Kbps", "Mbps", "Gbps", "Tbps", "Pbps", "Ebps", "Zbps", "Ybps"];
    if bps <= 0.0 || !bps.is_finite() {
        return "0 bps".into();
    }
    let idx = ((bps.log10() / 3.0) as usize).min(UNITS.len() - 1);
    let val = bps / 1000f64.powi(idx as i32);
    let mut s: String = format!("{val:.6}").chars().take(4).collect();
    if s.ends_with('.') {
        s.pop();
    }
    format!("{s} {}", UNITS[idx])
}

fn state_name(s: Option<SessionState>) -> &'static str {
    match s {
        Some(SessionState::Idle) => "idle",
        Some(SessionState::Connecting) => "connecting",
        Some(SessionState::Live) => "LIVE",
        Some(SessionState::Backoff) => "retry",
        Some(SessionState::Stopped) => "stopped",
        Some(SessionState::Paused) => "paused",
        None => "?",
    }
}

/// "00:12:03 · recon 1 · drop 3 · queue 2.1 MB" (time in LIVE; the green dot already
/// shows the state), or the state and the last error.
fn slot_info(s: &obs_rtmp_sessions::SessionSnapshot, live_since: Option<Instant>) -> String {
    let mut parts = Vec::new();
    match (s.state, live_since) {
        (SessionState::Live, Some(t0)) => {
            let secs = t0.elapsed().as_secs();
            parts.push(format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60));
        }
        (st, _) => parts.push(state_name(Some(st)).to_owned()),
    }
    if s.reconnects != 0 {
        parts.push(format!("recon {}", s.reconnects));
    }
    if s.drops.dropped_video_frames != 0 {
        parts.push(format!("drop {}", s.drops.dropped_video_frames));
    }
    if s.state == SessionState::Live {
        // Dropping now (last 10 s), and TCP retransmissions: a lossy path.
        if s.recent_drop_pct >= 5.0 {
            parts.push(format!("dropping {:.0}%", s.recent_drop_pct));
        }
        if let Some(r) = s.retrans_pct.filter(|r| *r >= 1.0) {
            parts.push(format!("retx {r:.1}%"));
        }
    }
    // A queue of more than half a megabyte means this target is falling behind.
    if s.queued_bytes >= 512 * 1024 {
        parts.push(format!("queue {:.1} MB", s.queued_bytes as f64 / (1024.0 * 1024.0)));
    }
    if s.state != SessionState::Live && s.state != SessionState::Paused {
        if let Some(e) = s.last_error.as_deref().filter(|e| !e.is_empty()) {
            parts.push(e.to_owned());
        }
    }
    parts.join(" · ")
}

/// Tooltip of a stream row: where the connection went and how it is doing.
fn slot_tooltip(s: &obs_rtmp_sessions::SessionSnapshot) -> String {
    let mut lines = Vec::new();
    if let Some(c) = &s.connection {
        lines.push(format!("Ingest: {} ({})", c.host, c.remote));
        lines.push(format!("Local: {}", c.local));
    }
    if s.state == SessionState::Live {
        let mut net = Vec::new();
        if let Some(rtt) = s.rtt_ms {
            net.push(format!("RTT {rtt:.0} ms"));
        }
        if let Some(r) = s.retrans_pct {
            net.push(format!("retransmitted {r:.1}%"));
        }
        net.push(format!("dropped {:.0}% (last 10 s)", s.recent_drop_pct));
        lines.push(net.join(", "));
    }
    if s.reconnects != 0 {
        lines.push(format!("Reconnects: {} ({} because of congestion)", s.reconnects, s.congestion_reconnects));
    }
    if let Some(e) = s.last_error.as_deref().filter(|e| !e.is_empty()) {
        lines.push(e.to_owned());
    }
    lines.join("\n")
}

fn slot_color(s: SessionState) -> &'static str {
    match s {
        SessionState::Live => "#2ecc71",
        SessionState::Connecting | SessionState::Backoff => "#e6b800",
        SessionState::Stopped => "#e74c3c",
        SessionState::Idle | SessionState::Paused => "#888888",
    }
}

impl Runtime {
    pub fn new(id: &str) -> Self {
        let now = Instant::now();
        Self {
            id: id.to_owned(),
            output: None,
            service: None,
            scene_view: None,
            signal_ctx: None,
            generation: 0,
            busy: false,
            starting: false,
            started: false,
            connecting: false,
            reconnecting: false,
            use_delay: false,
            last_error_code: 0,
            msg: String::new(),
            tooltip: String::new(),
            begin: now,
            last_info: now,
            total_bytes: 0,
            total_frames: 0,
            timer: false,
            rust_degraded: false,
            slot_bytes: [0; 2],
            slot_bps: [0.0; 2],
            slot_frames: [0; 2],
            slot_fps: [0.0; 2],
            clock: String::new(),
            slot_state: [None; 2],
            slot_live_since: [None; 2],
            hotkeys: Default::default(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.output.as_ref().is_some_and(Output::active)
    }

    pub fn is_busy(&self) -> bool {
        self.busy || self.starting
    }

    fn set_msg(&mut self, m: impl Into<String>) {
        self.msg = m.into();
        self.tooltip = self.msg.clone();
    }

    fn reset_info(&mut self) {
        self.total_bytes = 0;
        self.total_frames = 0;
        self.slot_bytes = [0; 2];
        self.slot_bps = [0.0; 2];
        self.slot_frames = [0; 2];
        self.slot_fps = [0.0; 2];
        self.clock.clear();
        self.slot_state = [None; 2];
        self.slot_live_since = [None; 2];
        self.last_info = Instant::now();
        self.msg.clear();
    }

    /// Releases output, service and view (in that order: the service after the output).
    pub fn release_output(&mut self) {
        if let Some(out) = self.output.take() {
            if let Some(ctx) = self.signal_ctx.take() {
                let sh = unsafe { sys::obs_output_get_signal_handler(out.as_ptr()) };
                let data = &*ctx as *const SignalCtx as *mut c_void;
                for (name, cb) in SIGNALS {
                    unsafe { sys::signal_handler_disconnect(sh, name.as_ptr(), cb, data) };
                }
            }
            if out.active() {
                unsafe { sys::obs_output_force_stop(out.as_ptr()) };
            }
            drop(out);
        }
        self.service = None;
        self.scene_view = None;
    }

    pub fn start(&mut self, t: &Target, cfg: &Config) -> Result<(), StartError> {
        if self.is_busy() {
            return Ok(());
        }
        if !t.enabled {
            self.set_msg(text("Status.Disabled"));
            return Ok(());
        }
        self.starting = true;
        let result = self.start_inner(t, cfg);
        if result.is_err() {
            self.starting = false;
            self.busy = false;
            self.release_output();
        }
        result
    }

    fn start_inner(&mut self, t: &Target, cfg: &Config) -> Result<(), StartError> {
        self.release_output();
        let proto = protocols::find_or_default(&t.protocol);
        let fail = |key: &str| StartError(text(key));

        // output (recreated on every start)
        let settings = obs_data(&t.output_param);
        let (oid, oname) = (c(proto.output_id), c("multi-output"));
        let out = unsafe { Output::from_owned(sys::obs_output_create(oid.as_ptr(), oname.as_ptr(), data_ptr(&settings), std::ptr::null_mut())) }
            .ok_or_else(|| fail("Error.CreateRtmpService"))?;
        if t.protocol == "RTMP_RUST" {
            rust_output::set_label(out.as_ptr(), &t.name);
        }
        self.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        let ctx = Box::new(SignalCtx { target_id: self.id.clone(), generation: self.generation });
        let sh = unsafe { sys::obs_output_get_signal_handler(out.as_ptr()) };
        let data = &*ctx as *const SignalCtx as *mut c_void;
        for (name, cb) in SIGNALS {
            unsafe { sys::signal_handler_connect(sh, name.as_ptr(), cb, data) };
        }
        self.signal_ctx = Some(ctx);

        // profile stream delay
        unsafe {
            let pc = profile_config();
            let enable = sys::config_get_bool(pc, c"Output".as_ptr(), c"DelayEnable".as_ptr());
            let preserve = sys::config_get_bool(pc, c"Output".as_ptr(), c"DelayPreserve".as_ptr());
            let secs = sys::config_get_int(pc, c"Output".as_ptr(), c"DelaySec".as_ptr()).max(0) as u32;
            sys::obs_output_set_delay(out.as_ptr(), if enable { secs } else { 0 }, if preserve { sys::OBS_OUTPUT_DELAY_PRESERVE } else { 0 });
            self.use_delay = enable && secs > 0;
        }
        self.output = Some(out.clone());

        // service
        if let Some(sid) = proto.service_id {
            // null service-param: service with default settings (the C++ plugin failed here)
            let s = obs_data(&t.service_param).unwrap_or_default();
            let (sid, sname) = (c(sid), c("multi-output-service"));
            let svc = unsafe { Service::from_owned(sys::obs_service_create(sid.as_ptr(), sname.as_ptr(), s.as_ptr(), std::ptr::null_mut())) }
                .ok_or_else(|| fail("Error.CreateRtmpService"))?;
            unsafe { sys::obs_output_set_service(out.as_ptr(), svc.as_ptr()) };
            self.service = Some(svc);
        }

        // encoders
        let Some(enc) = prepare_encoders(t, cfg) else {
            return Err(StartError(format!("{}\n{}", text("Notice.GetEncoder"), text("Error.CreateEncoder"))));
        };
        unsafe {
            for (idx, a) in &enc.audio {
                sys::obs_output_set_audio_encoder(out.as_ptr(), a.as_ptr(), *idx);
            }
            sys::obs_output_set_video_encoder(out.as_ptr(), enc.video.as_ptr());
        }

        // video source: main mix or a dedicated scene view
        if !enc.video_is_obs {
            let scene = t.video_config.as_deref().and_then(|id| cfg.video_config(id)).and_then(|v| v.scene.clone());
            match scene {
                Some(name) => {
                    let view = scene_view::acquire(&enc.video, &name).map_err(|e| {
                        crate::warn!("{e}");
                        fail("Error.SceneNotExist")
                    })?;
                    self.scene_view = Some(view);
                }
                None => unsafe { sys::obs_encoder_set_video(enc.video.as_ptr(), sys::obs_get_video()) },
            }
        }
        if !enc.audio_is_obs {
            for (_, a) in &enc.audio {
                unsafe { sys::obs_encoder_set_audio(a.as_ptr(), sys::obs_get_audio()) };
            }
        }

        self.busy = true;
        self.started = false;
        if !unsafe { sys::obs_output_start(out.as_ptr()) } {
            let detail = out.last_error();
            let mut m = text("Error.StartOutput");
            if !detail.is_empty() {
                m = format!("{m}\n{detail}");
            }
            return Err(StartError(m));
        }
        Ok(())
    }

    /// `force`: drop the delay (or force stop).
    pub fn stop(&mut self, force: bool) {
        if !self.is_busy() {
            return;
        }
        if let Some(out) = &self.output {
            unsafe {
                if force {
                    sys::obs_output_force_stop(out.as_ptr())
                } else {
                    sys::obs_output_stop(out.as_ptr())
                }
            }
        } else {
            self.busy = false;
            self.starting = false;
        }
    }

    pub fn on_signal(&mut self, generation: u64, sig: Signal) {
        if generation != self.generation {
            return; // from a previous output
        }
        match sig {
            Signal::Starting => {
                self.begin = Instant::now();
                // The Rust output emits "start" before "starting": do not go back to "connecting".
                if !self.started {
                    self.connecting = true;
                    self.set_msg(text("Status.Connecting"));
                }
                self.reconnecting = false;
                self.last_error_code = 0;
            }
            Signal::Start => {
                self.starting = false;
                self.started = true;
                self.connecting = false;
                self.reconnecting = false;
                self.set_msg(text("Status.Streaming"));
                self.reset_info();
                self.msg = text("Status.Streaming");
                self.timer = true;
            }
            Signal::Reconnect => {
                self.timer = false;
                self.reconnecting = true;
                self.set_msg(text("Status.Reconnecting"));
            }
            Signal::ReconnectSuccess => {
                self.reconnecting = false;
                self.reset_info();
                self.set_msg(text("Status.Streaming"));
                self.timer = true;
            }
            Signal::Stopping => {
                self.timer = false;
                self.set_msg(text("Status.Stopping"));
            }
            Signal::Stop(code) => {
                let detail = self.output.as_ref().map(Output::last_error).unwrap_or_default();
                self.busy = false;
                self.starting = false;
                self.started = false;
                self.connecting = false;
                self.reconnecting = false;
                self.timer = false;
                self.rust_degraded = false;
                self.last_error_code = code;
                self.reset_info();
                let key = match code as i32 {
                    sys::OBS_OUTPUT_SUCCESS => "",
                    sys::OBS_OUTPUT_BAD_PATH => "Error.WrongRTMPUrl",
                    sys::OBS_OUTPUT_CONNECT_FAILED => "Error.ServerConnect",
                    sys::OBS_OUTPUT_INVALID_STREAM => "Error.ServerHandshake",
                    sys::OBS_OUTPUT_ERROR => "Error.ServerRefuse",
                    _ => "Error.Unknown",
                };
                let mut m = if key.is_empty() { String::new() } else { text(key) };
                if code != 0 && !detail.is_empty() {
                    m = format!("{m} ({detail})");
                }
                self.set_msg(m);
                // Release the output now: frees shared encoders and the service.
                self.release_output();
            }
        }
    }

    /// 1 s tick: line "HH:MM:SS  bitrate  N FPS". For Rust targets the bitrate and
    /// state are per stream (Live/Backup rows), and the line becomes "HH:MM:SS  N FPS".
    pub fn tick(&mut self, t: &Target) {
        let Some(out) = self.output.clone() else { return };
        if !self.timer {
            return;
        }
        let now = Instant::now();
        let interval = now.duration_since(self.last_info).as_secs_f64();
        let bytes = unsafe { sys::obs_output_get_total_bytes(out.as_ptr()) };
        let frames = unsafe { sys::obs_output_get_total_frames(out.as_ptr()) };
        if interval > 0.0 {
            let secs = now.duration_since(self.begin).as_secs();
            let fps = ((frames - self.total_frames) as f64 / interval).round() as i64;
            let bps = bytes.saturating_sub(self.total_bytes) as f64 * 8.0 / interval;
            let clock = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
            let mut line = format!("{clock}  {}  {fps} FPS", format_bitrate(bps));
            self.tooltip.clear();
            self.clock.clear();
            self.rust_degraded = false;
            if t.protocol == "RTMP_RUST" {
                if let Some(st) = rust_output::status(out.as_ptr()) {
                    // The clock goes next to the name; bitrate and FPS go on each stream.
                    line.clear();
                    self.clock = clock;
                    // Independent bitrate per stream. The session counter never goes
                    // backwards; if it did, saturating_sub yields 0 for that second.
                    for (i, snap) in [&st.primary, &st.backup].into_iter().enumerate() {
                        let (sent, vframes, state) =
                            snap.as_ref().map_or((0, 0, None), |s| (s.bytes_sent, s.video_frames_sent, Some(s.state)));
                        self.slot_bps[i] = sent.saturating_sub(self.slot_bytes[i]) as f64 * 8.0 / interval;
                        self.slot_bytes[i] = sent;
                        self.slot_fps[i] = vframes.saturating_sub(self.slot_frames[i]) as f64 / interval;
                        self.slot_frames[i] = vframes;
                        if state != Some(SessionState::Live) {
                            self.slot_live_since[i] = None;
                        } else if self.slot_live_since[i].is_none() {
                            self.slot_live_since[i] = Some(now);
                        }
                        self.slot_state[i] = state;
                    }
                    let live = |s: &Option<obs_rtmp_sessions::SessionSnapshot>| s.as_ref().is_some_and(|s| s.state == SessionState::Live);
                    self.rust_degraded = !live(&st.primary) || (st.backup.is_some() && !live(&st.backup));
                }
            }
            self.msg = line;
        }
        self.total_frames = frames;
        self.total_bytes = bytes;
        self.last_info = now;
    }

    fn dot_color(&self) -> &'static str {
        if self.reconnecting || self.connecting || self.rust_degraded {
            "#e6b800"
        } else if self.is_running() {
            "#2ecc71"
        } else if self.last_error_code != 0 {
            "#e74c3c"
        } else {
            "#888888"
        }
    }

    /// One row per stream (Live/Backup) of an active Rust target: its own controls,
    /// bitrate and connection details.
    fn rust_controls(&self, t: &Target) -> Value {
        let Some(out) = &self.output else { return Value::Null };
        if t.protocol != "RTMP_RUST" || !self.is_running() {
            return Value::Null;
        }
        let Some(st) = rust_output::status(out.as_ptr()) else { return Value::Null };
        json!({"primary": self.slot_model(0, st.primary.as_ref()), "backup": self.slot_model(1, st.backup.as_ref())})
    }

    fn slot_model(&self, i: usize, snap: Option<&obs_rtmp_sessions::SessionSnapshot>) -> Value {
        let Some(s) = snap else { return Value::Null };
        // The bitrate is only valid if the tick has already measured this state (after
        // pause/resume the previous value is stale).
        let measured = self.slot_state[i] == Some(s.state);
        let bitrate = if s.state == SessionState::Live && measured {
            format!("{}  {} FPS", format_bitrate(self.slot_bps[i]), self.slot_fps[i].round() as i64)
        } else {
            String::new()
        };
        json!({
            "paused": s.state == SessionState::Paused,
            "dot": slot_color(s.state),
            "bitrate": bitrate,
            "info": slot_info(s, self.slot_live_since[i].filter(|_| measured)),
            "tooltip": slot_tooltip(s),
        })
    }

    pub fn rust_control(&self, slot: rust_output::Slot, action: rust_output::Action) -> bool {
        self.output.as_ref().is_some_and(|o| rust_output::control(o.as_ptr(), slot, action))
    }

    pub fn model(&self, t: &Target) -> Value {
        let name = if t.name.is_empty() { text("NewStreaming") } else { t.name.clone() };
        json!({
            "id": t.id,
            "name": name,
            "enabled": t.enabled,
            "busy": self.is_busy(),
            "button": text(if self.is_busy() { "Status.Stop" } else { "Btn.Start" }),
            "msg": self.msg,
            "clock": if self.is_running() { self.clock.as_str() } else { "" },
            "tooltip": if self.tooltip.is_empty() { self.msg.clone() } else { self.tooltip.clone() },
            "dot": self.dot_color(),
            "rust": self.rust_controls(t),
        })
    }

    /// Switches the view's scene on the fly (if the target uses one).
    pub fn update_scene(&self, scene: Option<&str>) {
        if let (Some(view), Some(name)) = (&self.scene_view, scene) {
            match scene_view::find_scene(name) {
                Some(src) => view.set_source(src),
                None => crate::warn!("scene '{name}' not found; keeping '{}'", view.scene_name()),
            }
        }
    }

    pub fn output_name(&self) -> Option<String> {
        self.output.as_ref().map(Output::name)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.release_output();
    }
}

/// Applies new parameters to a live encoder: keys that disappear revert to their
/// default value (or are kept if they have none).
pub fn update_encoder_settings(encoder: &Encoder, old: &Value, new: &Value, kind: &str) {
    unsafe {
        let current = Data::from_owned(sys::obs_encoder_get_settings(encoder.as_ptr()));
        let defaults = Data::from_owned(sys::obs_encoder_get_defaults(encoder.as_ptr()));
        let parse = |p: *const std::ffi::c_char| serde_json::from_str::<Value>(&cstr(p)).unwrap_or(json!({}));
        let current = current.map(|d| parse(sys::obs_data_get_json(d.as_ptr()))).unwrap_or(json!({}));
        let defaults = defaults.map(|d| parse(sys::obs_data_get_json_with_defaults(d.as_ptr()))).unwrap_or(json!({}));
        let mut merged = if new.is_object() { new.clone() } else { json!({}) };
        if let Some(old) = old.as_object() {
            for (k, v) in old {
                if merged.get(k).is_some() {
                    continue;
                }
                if let Some(d) = defaults.get(k) {
                    merged[k] = d.clone();
                } else {
                    crate::warn!("{kind} parameter '{k}' has no default; keeping its current value");
                    merged[k] = current.get(k).cloned().unwrap_or_else(|| v.clone());
                }
            }
        }
        if let Some(d) = Data::from_json(&merged.to_string()) {
            sys::obs_encoder_update(encoder.as_ptr(), d.as_ptr());
        }
    }
}

pub fn find_encoder(name: &str) -> Option<Encoder> {
    encoder_by_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_format_matches_cpp() {
        assert_eq!(format_bitrate(2_500_000.0), "2.50 Mbps");
        assert_eq!(format_bitrate(123_456_789.0), "123 Mbps");
        assert_eq!(format_bitrate(0.0), "0 bps");
        assert_eq!(format_bitrate(999.0), "999 bps");
        assert_eq!(format_bitrate(1500.0), "1.50 Kbps");
    }

    #[test]
    fn resolution_parsing() {
        assert_eq!(parse_resolution(" 1280 x 720 "), Some((1280, 720)));
        assert_eq!(parse_resolution("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_resolution("1920X1080"), None);
        assert_eq!(parse_resolution("123456x10"), None);
        assert_eq!(parse_resolution("abc"), None);
    }
}
