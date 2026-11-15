//! Central plugin state. It lives ONLY on the UI thread (thread_local):
//! frontend events, dock commands, signals and hotkeys (queued) all arrive
//! there. Modal dialogs are always opened with the state not borrowed,
//! because their event loop may re-enter the plugin.
use crate::config::{self, Config, Target};
use crate::hotkeys::{self, GlobalHotkeys};
use crate::module::text;
use crate::obs::{c, cstr, sys};
use crate::rust_output::{Action as RustAction, Slot};
use crate::target::{self, Runtime, Signal};
use crate::{null_stream, rust_output, ui};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ffi::{c_char, c_int, c_void};

pub struct Core {
    pub config: Config,
    runtimes: HashMap<String, Runtime>,
    frontend_ready: bool,
    global_hotkeys: Option<GlobalHotkeys>,
    last_model: String,
}

thread_local! {
    static CORE: RefCell<Option<Core>> = const { RefCell::new(None) };
}

/// Runs `f` with the state. `None` if it does not exist or is already borrowed
/// (re-entry from a modal dialog's event loop).
fn with_core<R>(f: impl FnOnce(&mut Core) -> R) -> Option<R> {
    CORE.with(|cell| cell.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// Like `with_core`, but retries later if the state is busy.
fn with_core_or_retry(f: impl FnOnce(&mut Core) + Send + Clone + 'static) {
    let retry = f.clone();
    if with_core(f).is_none() && CORE.with(|cell| cell.try_borrow().map(|g| g.is_some()).unwrap_or(true)) {
        ui::post(move || with_core_or_retry(retry));
    }
}

// ---------------------------------------------------------------- persistence

fn config_path() -> Option<String> {
    unsafe {
        let p = sys::obs_frontend_get_current_profile_path();
        if p.is_null() {
            return None;
        }
        let s = cstr(p);
        sys::bfree(p as *mut c_void);
        Some(format!("{s}/obs-multi-rtmp.json"))
    }
}

fn load_config() -> Config {
    let Some(path) = config_path() else { return Config::default() };
    match std::fs::read_to_string(&path) {
        Ok(text) => config::parse(&text).unwrap_or_else(|| {
            crate::error!("invalid JSON in {path}; starting with an empty config");
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

impl Core {
    fn new() -> Self {
        let mut core =
            Self { config: Config::default(), runtimes: HashMap::new(), frontend_ready: false, global_hotkeys: None, last_model: String::new() };
        core.load();
        core.sync_runtimes();
        core
    }

    /// Loads the current profile's config (and migrates configs from older versions).
    fn load(&mut self) {
        self.config = load_config();
        if config::migrate_rust_destinations(&mut self.config) {
            crate::info!("moved Rust engine destinations from output to service settings");
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = config_path() else { return };
        let content = config::to_json(&self.config);
        let (p, s) = (c(&path), c(&content));
        unsafe { sys::os_quick_write_utf8_file_safe(p.as_ptr(), s.as_ptr(), content.len(), true, c"tmp".as_ptr(), c"bak".as_ptr()) };
        crate::info!(
            "Save {} targets, {} video configs, {} audio configs",
            self.config.targets.len(),
            self.config.video.len(),
            self.config.audio.len()
        );
    }

    /// One runtime (and its hotkeys) per target; releases those of deleted targets.
    fn sync_runtimes(&mut self) {
        let ids: BTreeSet<String> = self.config.targets.iter().map(|t| t.id.clone()).collect();
        self.runtimes.retain(|id, rt| ids.contains(id) || rt.is_busy());
        for t in &self.config.targets {
            let rt = self.runtimes.entry(t.id.clone()).or_insert_with(|| Runtime::new(&t.id));
            if self.frontend_ready {
                rt.hotkeys.sync(&t.id, &t.name);
            }
        }
    }

    fn target_and_runtime(&mut self, id: &str) -> Option<(Target, &mut Runtime)> {
        let t = self.config.target(id)?.clone();
        Some((t, self.runtimes.get_mut(id)?))
    }

    fn start(&mut self, id: &str) {
        let cfg = self.config.clone();
        let Some((t, rt)) = self.target_and_runtime(id) else { return };
        if let Err(e) = rt.start(&t, &cfg) {
            crate::warn!("target '{}' failed to start: {}", t.name, e.0.replace('\n', " "));
            rt.msg = e.0;
        }
    }

    fn force_stop_all(&mut self) {
        for rt in self.runtimes.values_mut() {
            rt.stop(true);
        }
    }

    pub fn model(&mut self) -> String {
        let streaming = unsafe { sys::obs_frontend_streaming_active() };
        // Before FINISHED_LOADING the profile's service does not exist yet: do not query it.
        let null_checked = self.frontend_ready && null_stream::is_active();
        let targets: Vec<Value> =
            self.config.targets.iter().filter_map(|t| self.runtimes.get(&t.id).map(|rt| rt.model(t))).collect();
        let m = json!({
            "null_stream": {"checked": null_checked, "enabled": self.frontend_ready && !streaming},
            "congestion_reconnect": {"checked": rust_output::congestion_reconnect(), "enabled": self.frontend_ready},
            "targets": targets,
        });
        self.last_model = m.to_string();
        self.last_model.clone()
    }

    /// "Edit targets": applies the edited config (ApplyEditedConfig logic).
    /// Returns the notices to show.
    fn apply_edited(&mut self, mut edited: Config) -> Vec<String> {
        let old = std::mem::take(&mut self.config);
        let mut notices: Vec<String> = Vec::new();
        let mut push = |n: String| {
            if !notices.contains(&n) {
                notices.push(n)
            }
        };
        let mut updated_video = BTreeSet::new();
        let mut updated_audio = BTreeSet::new();

        for (id, rt) in &self.runtimes {
            let (Some(o), Some(n)) = (old.target(id), edited.target(id)) else { continue };
            let busy = rt.is_busy();
            let name = if n.name.is_empty() { text("NewStreaming") } else { n.name.clone() };
            let notice = |key: &str| text(key).replace("%1", &name);
            if busy && (o.protocol != n.protocol || o.service_param != n.service_param) {
                push(notice("Notice.ServiceChanged"));
            }
            if busy && (o.video_config != n.video_config || o.audio_config != n.audio_config) {
                push(notice("Notice.EncoderConfigChanged"));
            }
            // Same video config with new parameters: update the live encoder.
            if let (Some(vid), true) = (&n.video_config, o.video_config == n.video_config) {
                if let (Some(ov), Some(nv)) = (old.video_config(vid), edited.video_config(vid)) {
                    if ov != nv {
                        if let Some(enc) = target::find_encoder(&format!("multi-rtmp-venc{vid}")) {
                            if updated_video.insert(vid.clone()) {
                                target::update_encoder_settings(&enc, &ov.param, &nv.param, "video encoder");
                            }
                            if busy {
                                push(notice("Notice.EncoderParamsChanged"));
                            }
                        }
                    }
                    rt.update_scene(nv.scene.as_deref());
                }
            }
            if let (Some(aid), true) = (&n.audio_config, o.audio_config == n.audio_config) {
                if let (Some(oa), Some(na)) = (old.audio_config(aid), edited.audio_config(aid)) {
                    if oa != na {
                        let tracks: BTreeSet<i64> =
                            std::iter::once(0).chain(oa.tracks.iter().chain(&na.tracks).map(|t| t.output_track)).collect();
                        for tr in tracks {
                            let enc_name = format!("multi-rtmp-aenc{aid}-track-idx-{tr}");
                            if let Some(enc) = target::find_encoder(&enc_name) {
                                if updated_audio.insert(enc_name) {
                                    target::update_encoder_settings(&enc, &oa.param, &na.param, "audio encoder");
                                }
                                if busy {
                                    push(notice("Notice.EncoderParamsChanged"));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Deleted targets that are still active: they cannot be deleted yet.
        let mut failed = Vec::new();
        for (id, rt) in &self.runtimes {
            if edited.target(id).is_some() || !rt.is_busy() {
                continue;
            }
            if let Some(t) = old.target(id) {
                for (cid, is_video) in [(t.video_config.clone(), true), (t.audio_config.clone(), false)] {
                    let Some(cid) = cid else { continue };
                    if is_video && edited.video_config(&cid).is_none() {
                        if let Some(v) = old.video_config(&cid) {
                            edited.video.push(v.clone());
                        }
                    } else if !is_video && edited.audio_config(&cid).is_none() {
                        if let Some(a) = old.audio_config(&cid) {
                            edited.audio.push(a.clone());
                        }
                    }
                }
                failed.push(t.name.clone());
                edited.targets.push(t.clone());
            }
        }

        self.config = edited;
        self.sync_runtimes();
        self.save();
        if !failed.is_empty() {
            push(text("Notice.DeleteFailed"));
            for n in failed {
                push(n);
            }
        }
        notices
    }
}

// ---------------------------------------------------------------- advanced options

/// Where the Advanced menu's reconnect-on-congestion option is kept: the OBS
/// profile (basic.ini), so obs-multi-rtmp.json keeps the original format.
const PROFILE_SECTION: &std::ffi::CStr = c"obs-multi-rtmp";
const KEY_CONGESTION_RECONNECT: &std::ffi::CStr = c"CongestionReconnect";

fn load_congestion_reconnect() {
    let pc = unsafe { sys::obs_frontend_get_profile_config() };
    let enabled = !pc.is_null()
        && unsafe { sys::config_get_bool(pc, PROFILE_SECTION.as_ptr(), KEY_CONGESTION_RECONNECT.as_ptr()) };
    rust_output::set_congestion_reconnect(enabled);
}

fn save_congestion_reconnect(enabled: bool) {
    rust_output::set_congestion_reconnect(enabled);
    let pc = unsafe { sys::obs_frontend_get_profile_config() };
    if !pc.is_null() {
        unsafe {
            sys::config_set_bool(pc, PROFILE_SECTION.as_ptr(), KEY_CONGESTION_RECONNECT.as_ptr(), enabled);
            sys::config_save_safe(pc, c"tmp".as_ptr(), std::ptr::null());
        }
    }
}

// ---------------------------------------------------------------- lifecycle

pub fn init() {
    CORE.with(|cell| *cell.borrow_mut() = Some(Core::new()));
    unsafe { sys::obs_frontend_add_event_callback(on_frontend_event, std::ptr::null_mut()) };
}

pub fn shutdown() {
    unsafe { sys::obs_frontend_remove_event_callback(on_frontend_event, std::ptr::null_mut()) };
    // Releases outputs (and services, views) and unregisters the hotkeys.
    let core = CORE.with(|cell| cell.borrow_mut().take());
    drop(core);
}

const EV_STREAMING_STARTING: c_int = 0;
const EV_STREAMING_STOPPING: c_int = 2;
const EV_PROFILE_CHANGED: c_int = 15;
const EV_EXIT: c_int = 17;
const EV_FINISHED_LOADING: c_int = 26;

unsafe extern "C" fn on_frontend_event(event: c_int, _: *mut c_void) {
    // If the event arrives while the state is borrowed (OBS emitted it inside
    // one of our calls), handle it later instead of dropping it.
    if CORE.with(|cell| cell.try_borrow_mut().is_err()) {
        ui::post(move || handle_frontend_event(event));
        return;
    }
    handle_frontend_event(event);
}

fn handle_frontend_event(event: c_int) {
    match event {
        EV_STREAMING_STARTING => {
            let ids = with_core(|c| {
                c.config.targets.iter().filter(|t| t.sync_start && t.enabled).map(|t| t.id.clone()).collect::<Vec<_>>()
            })
            .unwrap_or_default();
            for id in ids {
                with_core(|c| c.start(&id));
            }
        }
        EV_STREAMING_STOPPING => {
            let ids = with_core(|c| c.config.targets.iter().filter(|t| t.sync_stop).map(|t| t.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default();
            for id in ids {
                stop_target(&id, true);
            }
        }
        EV_PROFILE_CHANGED => {
            load_congestion_reconnect();
            with_core(|c| {
                c.force_stop_all();
                c.runtimes.clear();
                c.load();
                c.sync_runtimes();
                if let Some(h) = &c.global_hotkeys {
                    h.reload(); // bindings are per profile
                }
            });
        }
        EV_EXIT => {
            with_core(|c| {
                c.force_stop_all();
                c.save();
                c.frontend_ready = false;
            });
        }
        EV_FINISHED_LOADING => {
            load_congestion_reconnect();
            with_core(|c| {
                c.frontend_ready = true;
                c.global_hotkeys = Some(hotkeys::register_global());
                c.sync_runtimes();
            });
        }
        _ => {}
    }
    ui::refresh();
}

pub fn on_signal(id: &str, generation: u64, sig: Signal) {
    let id = id.to_owned();
    with_core_or_retry(move |c| {
        if let Some(rt) = c.runtimes.get_mut(&id) {
            rt.on_signal(generation, sig);
        }
    });
    ui::refresh();
}

pub fn on_hotkey(id: &str, action: hotkeys::Action) {
    match action {
        hotkeys::Action::Start => {
            with_core(|c| c.start(id));
        }
        hotkeys::Action::Stop => stop_target(id, true),
        hotkeys::Action::StartAll => start_all(),
        hotkeys::Action::StopAll => stop_all(),
    }
    ui::refresh();
}

// ---------------------------------------------------------------- actions

/// Stops a target; with stream delay active, asks whether to drop it (modal, state not borrowed).
fn stop_target(id: &str, ask: bool) {
    let Some((busy, delay)) = with_core(|c| c.runtimes.get(id).map(|rt| (rt.is_busy(), rt.use_delay))).flatten() else { return };
    if !busy {
        return;
    }
    let force = ask && delay && ui::question("?", &text("Ques.DropDelay"));
    with_core(|c| {
        if let Some(rt) = c.runtimes.get_mut(id) {
            rt.stop(force);
        }
    });
}

fn start_all() {
    let ids = with_core(|c| c.config.targets.iter().map(|t| t.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
    for id in ids {
        with_core(|c| c.start(&id));
    }
}

fn stop_all() {
    let ids = with_core(|c| c.config.targets.iter().map(|t| t.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
    for id in ids {
        stop_target(&id, true);
    }
}

fn command(cmd: &Value) {
    let id = cmd["id"].as_str().unwrap_or_default().to_owned();
    match cmd["cmd"].as_str().unwrap_or_default() {
        "start" => {
            with_core(|c| c.start(&id));
        }
        "stop" => stop_target(&id, true),
        "toggle" => {
            if with_core(|c| c.runtimes.get(&id).is_some_and(Runtime::is_busy)).unwrap_or(false) {
                stop_target(&id, true);
            } else {
                with_core(|c| c.start(&id));
            }
        }
        "start_all" => start_all(),
        "stop_all" => stop_all(),
        "tick" => {
            with_core(|c| {
                for t in &c.config.targets {
                    if let Some(rt) = c.runtimes.get_mut(&t.id) {
                        rt.tick(t);
                    }
                }
            });
        }
        "set_enabled" => {
            let enabled = cmd["enabled"].as_bool().unwrap_or(true);
            with_core(|c| {
                if let Some(t) = c.config.target_mut(&id) {
                    t.enabled = enabled;
                }
                if !enabled {
                    if let Some(rt) = c.runtimes.get_mut(&id) {
                        rt.stop(true); // even if still connecting
                    }
                }
                c.save();
            });
        }
        "reorder" => {
            let order: Vec<String> =
                cmd["ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
            with_core(|c| {
                let mut rest = std::mem::take(&mut c.config.targets);
                let mut sorted = Vec::new();
                for id in &order {
                    if let Some(pos) = rest.iter().position(|t| &t.id == id) {
                        sorted.push(rest.remove(pos));
                    }
                }
                sorted.extend(rest); // unlisted ones keep their order
                c.config.targets = sorted;
                c.save();
            });
        }
        "rust" => {
            let slot = if cmd["slot"].as_str() == Some("backup") { Slot::Backup } else { Slot::Primary };
            let action = match cmd["action"].as_str() {
                Some("pause") => RustAction::Pause,
                Some("resume") => RustAction::Resume,
                _ => RustAction::Restart,
            };
            with_core(|c| c.runtimes.get(&id).map(|rt| rt.rust_control(slot, action)));
        }
        "congestion_reconnect" => save_congestion_reconnect(cmd["enabled"].as_bool().unwrap_or(false)),
        "null_stream" => {
            // Outside the state: changing the service can fire frontend events.
            if let Err(e) = null_stream::set_active(cmd["enabled"].as_bool().unwrap_or(false)) {
                ui::message(ui::Icon::Warning, &text("Notice.Title"), &e);
            }
        }
        other => crate::warn!("unknown UI command '{other}'"),
    }
    ui::refresh();
}

// ---------------------------------------------------------------- API for the Qt layer

/// Dock view model (JSON). Free with omr_free.
#[no_mangle]
pub extern "C" fn omr_model_json() -> *mut c_char {
    let m = with_core(Core::model)
        .or_else(|| CORE.with(|cell| cell.try_borrow().ok().and_then(|g| g.as_ref().map(|c| c.last_model.clone()))))
        .unwrap_or_else(|| "{}".into());
    ui::into_c_string(m)
}

/// Dock command as JSON: {"cmd": "...", ...}.
#[no_mangle]
pub unsafe extern "C" fn omr_command(json: *const c_char) {
    let text = ui::from_c(json);
    match serde_json::from_str::<Value>(&text) {
        Ok(cmd) => command(&cmd),
        Err(e) => crate::warn!("invalid UI command: {e}"),
    }
}

/// Data for opening "Edit targets": serialized config and what is active.
#[no_mangle]
pub extern "C" fn omr_edit_begin() -> *mut c_char {
    let v = with_core(|c| {
        let mut targets = Vec::new();
        let (mut video, mut audio) = (BTreeSet::new(), BTreeSet::new());
        for t in &c.config.targets {
            if !c.runtimes.get(&t.id).is_some_and(Runtime::is_busy) {
                continue;
            }
            targets.push(t.id.clone());
            if let Some(v) = t.video_config.as_ref().filter(|v| c.config.video_config(v).is_some()) {
                video.insert(v.clone());
            }
            if let Some(a) = t.audio_config.as_ref().filter(|a| c.config.audio_config(a).is_some()) {
                audio.insert(a.clone());
            }
        }
        json!({"config": config::to_json(&c.config), "active_targets": targets, "active_video": video, "active_audio": audio})
    })
    .unwrap_or(Value::Null);
    ui::into_c_string(v.to_string())
}

/// Applies the edited config (JSON from the dialog) and shows the notices.
#[no_mangle]
pub unsafe extern "C" fn omr_edit_apply(json: *const c_char) {
    let Some(edited) = config::parse(&ui::from_c(json)) else { return };
    let notices = with_core(|c| c.apply_edited(edited)).unwrap_or_default();
    ui::refresh();
    if !notices.is_empty() {
        ui::message(ui::Icon::Info, &text("Notice.Title"), &notices.join("\n"));
    }
}
