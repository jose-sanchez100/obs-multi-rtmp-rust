//! Minimal safe layer over libobs.
pub mod refs;
pub mod sys;

pub use refs::{Data, Encoder, Output, Service, Source};

use std::ffi::{c_char, CStr, CString};

/// Writes to the OBS log with the plugin prefix.
pub fn log(level: std::ffi::c_int, msg: &str) {
    let line = CString::new(format!("[obs-multi-rtmp] {msg}").replace('\0', "")).unwrap();
    unsafe { sys::blog(level, c"%s".as_ptr(), line.as_ptr()) }
}

/// Forwards the session engine's `log` records (connections, congestion,
/// reconnects) to the OBS log.
struct EngineLogger;

impl log::Log for EngineLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info && m.target().starts_with("obs_rtmp_sessions")
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let level = match r.level() {
            log::Level::Error => sys::LOG_ERROR,
            log::Level::Warn => sys::LOG_WARNING,
            _ => sys::LOG_INFO,
        };
        log(level, &format!("rust output: {}", r.args()));
    }

    fn flush(&self) {}
}

pub fn install_logger() {
    static LOGGER: EngineLogger = EngineLogger;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

#[macro_export]
macro_rules! info { ($($t:tt)*) => { $crate::obs::log($crate::obs::sys::LOG_INFO, &format!($($t)*)) } }
#[macro_export]
macro_rules! warn { ($($t:tt)*) => { $crate::obs::log($crate::obs::sys::LOG_WARNING, &format!($($t)*)) } }
#[macro_export]
macro_rules! error { ($($t:tt)*) => { $crate::obs::log($crate::obs::sys::LOG_ERROR, &format!($($t)*)) } }

/// Copies a C string from OBS (may be NULL).
pub fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

pub fn c(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}
