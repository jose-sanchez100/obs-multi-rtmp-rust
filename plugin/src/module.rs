//! Entry points OBS expects from a module (equivalent to OBS_DECLARE_MODULE
//! and OBS_MODULE_USE_DEFAULT_LOCALE from obs-module.h).
use crate::obs::sys;
use std::ffi::{c_char, c_int};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

static MODULE: AtomicPtr<sys::obs_module_t> = AtomicPtr::new(ptr::null_mut());
static LOOKUP: AtomicPtr<sys::lookup_t> = AtomicPtr::new(ptr::null_mut());

const LIBOBS_API_VER: u32 = (33 << 24) | (0 << 16) | 1; // OBS 33.0.1 headers

#[no_mangle]
pub extern "C" fn obs_module_set_pointer(module: *mut sys::obs_module_t) {
    MODULE.store(module, Ordering::SeqCst);
}

#[no_mangle]
pub extern "C" fn obs_current_module() -> *mut sys::obs_module_t {
    MODULE.load(Ordering::SeqCst)
}

#[no_mangle]
pub extern "C" fn obs_module_ver() -> u32 {
    LIBOBS_API_VER
}

#[no_mangle]
pub unsafe extern "C" fn obs_module_set_locale(locale: *const c_char) {
    let old = LOOKUP.swap(ptr::null_mut(), Ordering::SeqCst);
    if !old.is_null() {
        sys::text_lookup_destroy(old);
    }
    let new = sys::obs_module_load_locale(obs_current_module(), c"en-US".as_ptr(), locale);
    LOOKUP.store(new, Ordering::SeqCst);
}

#[no_mangle]
pub unsafe extern "C" fn obs_module_free_locale() {
    let old = LOOKUP.swap(ptr::null_mut(), Ordering::SeqCst);
    if !old.is_null() {
        sys::text_lookup_destroy(old);
    }
}

/// Translated text; if the key is missing, returns the key itself (like OBS).
/// The Qt layer uses it too.
#[no_mangle]
pub unsafe extern "C" fn obs_module_text(key: *const c_char) -> *const c_char {
    let mut out = key;
    let lookup = LOOKUP.load(Ordering::SeqCst);
    if !lookup.is_null() {
        sys::text_lookup_getstr(lookup, key, &mut out);
    }
    out
}

#[no_mangle]
pub unsafe extern "C" fn obs_module_get_string(key: *const c_char, out: *mut *const c_char) -> bool {
    let lookup = LOOKUP.load(Ordering::SeqCst);
    !lookup.is_null() && sys::text_lookup_getstr(lookup, key, out)
}

#[no_mangle]
pub extern "C" fn obs_module_name() -> *const c_char {
    c"obs-multi-rtmp".as_ptr()
}

#[no_mangle]
pub extern "C" fn obs_module_description() -> *const c_char {
    c"Multiple RTMP Output Plugin (Rust)".as_ptr()
}

#[no_mangle]
pub extern "C" fn obs_module_author() -> *const c_char {
    c"SoraYuki (original), Rust port".as_ptr()
}

/// Translated text as a String (internal use in Rust).
pub fn text(key: &str) -> String {
    let k = crate::obs::c(key);
    crate::obs::cstr(unsafe { obs_module_text(k.as_ptr()) })
}

#[no_mangle]
pub extern "C" fn obs_module_load() -> bool {
    crate::plugin_load()
}

#[no_mangle]
pub extern "C" fn obs_module_unload() {
    crate::plugin_unload()
}

#[allow(dead_code)]
fn _int(_: c_int) {}
