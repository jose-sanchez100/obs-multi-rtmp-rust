//! "multi_rtmp_rust_service" service: stores the destinations of a target that
//! uses the Rust engine (Live server and key, Backup server and key), just as
//! rtmp_custom stores those of classic RTMP. This way the editor shows them in
//! the "Service" tab and the Rust output reads them from its assigned service.
//!
//! Custom protocol: only the Rust output accepts it, and the OBS Settings window
//! does not list this service (it only knows rtmp_common, rtmp_custom and whip_custom).
use crate::module::obs_module_text;
use crate::obs::{cstr, sys};
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;

pub const SERVICE_ID: &CStr = c"multi_rtmp_rust_service";
pub const PROTOCOL: &CStr = c"MULTI_RTMP_RUST";

/// Settings keys (shared with the migration of old configs).
pub const KEYS: [&str; 4] = ["server", "key", "backup_server", "backup_key"];

// OBS_SERVICE_CONNECT_INFO_*
const CONNECT_INFO_SERVER_URL: u32 = 0;
const CONNECT_INFO_STREAM_KEY: u32 = 2;

#[derive(Default)]
struct State {
    server: CString,
    key: CString,
}

fn t(key: &CStr) -> *const c_char {
    unsafe { obs_module_text(key.as_ptr()) }
}

unsafe fn get(settings: *mut sys::obs_data_t, key: &CStr) -> CString {
    CString::new(cstr(sys::obs_data_get_string(settings, key.as_ptr()))).unwrap_or_default()
}

unsafe extern "C" fn get_name(_: *mut c_void) -> *const c_char {
    t(c"RustService.Name")
}

unsafe extern "C" fn create(settings: *mut sys::obs_data_t, _: *mut sys::obs_service_t) -> *mut c_void {
    let st = Box::into_raw(Box::<State>::default());
    update(st as *mut c_void, settings);
    st as *mut c_void
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    drop(Box::from_raw(data as *mut State));
}

unsafe extern "C" fn update(data: *mut c_void, settings: *mut sys::obs_data_t) {
    let st = &mut *(data as *mut State);
    st.server = get(settings, c"server");
    st.key = get(settings, c"key");
}

unsafe extern "C" fn get_defaults(settings: *mut sys::obs_data_t) {
    for k in [c"server", c"key", c"backup_server", c"backup_key"] {
        sys::obs_data_set_default_string(settings, k.as_ptr(), c"".as_ptr());
    }
}

unsafe extern "C" fn get_properties(_: *mut c_void) -> *mut sys::obs_properties_t {
    let p = sys::obs_properties_create();
    sys::obs_properties_add_text(p, c"server".as_ptr(), t(c"RustService.Server"), sys::OBS_TEXT_DEFAULT);
    sys::obs_properties_add_text(p, c"key".as_ptr(), t(c"RustService.Key"), sys::OBS_TEXT_PASSWORD);
    sys::obs_properties_add_text(p, c"backup_server".as_ptr(), t(c"RustService.BackupServer"), sys::OBS_TEXT_DEFAULT);
    sys::obs_properties_add_text(p, c"backup_key".as_ptr(), t(c"RustService.BackupKey"), sys::OBS_TEXT_PASSWORD);
    p
}

unsafe extern "C" fn get_url(data: *mut c_void) -> *const c_char {
    (*(data as *const State)).server.as_ptr()
}

unsafe extern "C" fn get_key(data: *mut c_void) -> *const c_char {
    (*(data as *const State)).key.as_ptr()
}

unsafe extern "C" fn get_connect_info(data: *mut c_void, kind: u32) -> *const c_char {
    match kind {
        CONNECT_INFO_SERVER_URL => get_url(data),
        CONNECT_INFO_STREAM_KEY => get_key(data),
        _ => ptr::null(),
    }
}

unsafe extern "C" fn get_protocol(_: *mut c_void) -> *const c_char {
    PROTOCOL.as_ptr()
}

unsafe extern "C" fn get_output_type(_: *mut c_void) -> *const c_char {
    crate::rust_output::OUTPUT_ID.as_ptr()
}

unsafe extern "C" fn can_try_to_connect(_: *mut c_void) -> bool {
    // The output validates server and key and gives a clear error if they are missing.
    true
}

pub fn register() {
    let svc = sys::obs_service_info {
        id: SERVICE_ID.as_ptr(),
        get_name: Some(get_name),
        create: Some(create),
        destroy: Some(destroy),
        activate: None,
        deactivate: None,
        update: Some(update),
        get_defaults: Some(get_defaults),
        get_properties: Some(get_properties),
        initialize: None,
        get_url: Some(get_url),
        get_key: Some(get_key),
        get_username: None,
        get_password: None,
        deprecated_1: None,
        apply_encoder_settings: None,
        type_data: ptr::null_mut(),
        free_type_data: None,
        get_output_type: Some(get_output_type),
        get_supported_resolutions: None,
        get_max_fps: None,
        get_max_bitrate: None,
        get_supported_video_codecs: None,
        get_protocol: Some(get_protocol),
        get_supported_audio_codecs: None,
        get_connect_info: Some(get_connect_info),
        can_try_to_connect: Some(can_try_to_connect),
    };
    unsafe { sys::obs_register_service_s(&svc, std::mem::size_of::<sys::obs_service_info>()) };
}
