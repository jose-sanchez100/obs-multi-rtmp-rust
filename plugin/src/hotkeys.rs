//! Plugin hotkeys.
//!
//! Differences from the C++ plugin (bugs fixed):
//! - Actions run on the UI thread (the callback arrives on the libobs hotkey
//!   thread and only queues).
//! - Bindings are reloaded from the profile's `[Hotkeys]` when each hotkey is
//!   registered (OBS stores them there but does not reload them for plugin hotkeys).
//! - The description is updated when the target is renamed.
use crate::module::text;
use crate::obs::{c, cstr, sys, Data};
use crate::ui;
use std::ffi::c_void;

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Start,
    Stop,
    StartAll,
    StopAll,
}

struct Ctx {
    target_id: String,
    action: Action,
}

unsafe extern "C" fn on_hotkey(data: *mut c_void, _: sys::obs_hotkey_id, _: *mut sys::obs_hotkey_t, pressed: bool) {
    if !pressed || data.is_null() {
        return;
    }
    let ctx = &*(data as *const Ctx);
    let (id, action) = (ctx.target_id.clone(), ctx.action);
    ui::post(move || crate::core::on_hotkey(&id, action));
}

/// A registered hotkey; unregistered when dropped.
pub struct Hotkey {
    id: sys::obs_hotkey_id,
    ctx: *mut Ctx,
    name: String,
}

impl Hotkey {
    fn register(name: &str, description: &str, target_id: &str, action: Action) -> Option<Self> {
        let ctx = Box::into_raw(Box::new(Ctx { target_id: target_id.to_owned(), action }));
        let (n, d) = (c(name), c(description));
        let id = unsafe { sys::obs_hotkey_register_frontend(n.as_ptr(), d.as_ptr(), on_hotkey, ctx as *mut c_void) };
        if id == sys::OBS_INVALID_HOTKEY_ID {
            drop(unsafe { Box::from_raw(ctx) });
            return None;
        }
        load_bindings(id, name);
        Some(Self { id, ctx, name: name.to_owned() })
    }

    /// Bindings are per profile: reload when the profile changes.
    fn reload(&self) {
        load_bindings(self.id, &self.name);
    }

    fn set_description(&self, description: &str) {
        let d = c(description);
        unsafe { sys::obs_hotkey_set_description(self.id, d.as_ptr()) };
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        unsafe {
            sys::obs_hotkey_unregister(self.id);
            drop(Box::from_raw(self.ctx));
        }
    }
}

/// Reads `[Hotkeys] <name> = {"bindings":[...]}` from the profile config.
fn load_bindings(id: sys::obs_hotkey_id, name: &str) {
    unsafe {
        let pc = sys::obs_frontend_get_profile_config();
        if pc.is_null() {
            return;
        }
        let n = c(name);
        let json = cstr(sys::config_get_string(pc, c"Hotkeys".as_ptr(), n.as_ptr()));
        if json.is_empty() {
            return;
        }
        let Some(data) = Data::from_json(&json) else { return };
        let arr = sys::obs_data_get_array(data.as_ptr(), c"bindings".as_ptr());
        if !arr.is_null() {
            sys::obs_hotkey_load(id, arr);
            sys::obs_data_array_release(arr);
        }
    }
}

#[derive(Default)]
pub struct TargetHotkeys {
    start: Option<Hotkey>,
    stop: Option<Hotkey>,
    name: String,
}

impl TargetHotkeys {
    /// Registers a target's hotkeys (or updates their description).
    pub fn sync(&mut self, target_id: &str, name: &str) {
        let desc = |key: &str| format!("{} - {name}", text(key));
        if self.start.is_none() {
            self.start = Hotkey::register(&format!("obs-multi-rtmp.start.{target_id}"), &desc("Hotkey.StartTarget"), target_id, Action::Start);
            self.stop = Hotkey::register(&format!("obs-multi-rtmp.stop.{target_id}"), &desc("Hotkey.StopTarget"), target_id, Action::Stop);
            self.name = name.to_owned();
        } else if self.name != name {
            if let Some(h) = &self.start {
                h.set_description(&desc("Hotkey.StartTarget"));
            }
            if let Some(h) = &self.stop {
                h.set_description(&desc("Hotkey.StopTarget"));
            }
            self.name = name.to_owned();
        }
    }
}

pub struct GlobalHotkeys {
    start_all: Option<Hotkey>,
    stop_all: Option<Hotkey>,
}

impl GlobalHotkeys {
    pub fn reload(&self) {
        for h in [&self.start_all, &self.stop_all].into_iter().flatten() {
            h.reload();
        }
    }
}

pub fn register_global() -> GlobalHotkeys {
    GlobalHotkeys {
        start_all: Hotkey::register("obs-multi-rtmp.start_all", &text("Hotkey.StartAllTargets"), "", Action::StartAll),
        stop_all: Hotkey::register("obs-multi-rtmp.stop_all", &text("Hotkey.StopAllTargets"), "", Action::StopAll),
    }
}
