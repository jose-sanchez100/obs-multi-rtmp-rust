//! "Main OBS stream -> null output".
//!
//! OBS 33 picks the output for the "Start Streaming" button from the profile's
//! streaming service (get_protocol / get_output_type). We register our own
//! service and output: the output starts capture (the main encoders keep
//! running, so "same as OBS" and synchronized start work) but discards every
//! packet. OBS connects to nothing.
//!
//! The profile's original service is stored inside the null service's settings
//! (the profile's service.json) and restored when the option is disabled, even
//! if OBS was restarted or the profile was switched in between.
use crate::module::text;
use crate::obs::{c, cstr, sys};
use std::ffi::{c_char, c_int, c_void};
use std::ptr;

pub const SERVICE_ID: &std::ffi::CStr = c"multi_rtmp_null_service";
pub const OUTPUT_ID: &std::ffi::CStr = c"multi_rtmp_null_stream_output";
pub const PROTOCOL: &std::ffi::CStr = c"MULTI_RTMP_NULL";

// ---------------------------------------------------------------- service

unsafe extern "C" fn service_name(_: *mut c_void) -> *const c_char {
    c"Multi-RTMP: do not stream (null output)".as_ptr()
}
unsafe extern "C" fn service_create(_: *mut sys::obs_data_t, service: *mut sys::obs_service_t) -> *mut c_void {
    service as *mut c_void // stateless: the settings live in the obs_service itself
}
unsafe extern "C" fn service_destroy(_: *mut c_void) {}
unsafe extern "C" fn service_protocol(_: *mut c_void) -> *const c_char {
    PROTOCOL.as_ptr()
}
unsafe extern "C" fn service_output_type(_: *mut c_void) -> *const c_char {
    OUTPUT_ID.as_ptr()
}
unsafe extern "C" fn service_connect_info(_: *mut c_void, _: u32) -> *const c_char {
    c"".as_ptr()
}
unsafe extern "C" fn service_can_connect(_: *mut c_void) -> bool {
    true // nothing to validate: there is no server or key
}

// ---------------------------------------------------------------- output

unsafe extern "C" fn output_name(_: *mut c_void) -> *const c_char {
    c"Multi-RTMP null stream output".as_ptr()
}
unsafe extern "C" fn output_create(_: *mut sys::obs_data_t, output: *mut sys::obs_output_t) -> *mut c_void {
    output as *mut c_void
}
unsafe extern "C" fn output_destroy(_: *mut c_void) {}
unsafe extern "C" fn output_start(data: *mut c_void) -> bool {
    let out = data as *mut sys::obs_output_t;
    if !sys::obs_output_can_begin_data_capture(out, 0)
        || !sys::obs_output_initialize_encoders(out, 0)
        || !sys::obs_output_begin_data_capture(out, 0)
    {
        return false;
    }
    crate::info!("main OBS stream started on the null output (nothing is sent)");
    true
}
unsafe extern "C" fn output_stop(data: *mut c_void, _: u64) {
    sys::obs_output_end_data_capture(data as *mut sys::obs_output_t);
}
unsafe extern "C" fn output_packet(_: *mut c_void, _: *mut sys::encoder_packet) {
    // discarded: the plugin's targets have their own outputs
}
unsafe extern "C" fn output_total_bytes(_: *mut c_void) -> u64 {
    0
}
unsafe extern "C" fn output_dropped(_: *mut c_void) -> c_int {
    0
}

pub fn register() {
    let out = sys::obs_output_info {
        id: OUTPUT_ID.as_ptr(),
        flags: sys::OBS_OUTPUT_AV | sys::OBS_OUTPUT_ENCODED | sys::OBS_OUTPUT_SERVICE,
        get_name: Some(output_name),
        create: Some(output_create),
        destroy: Some(output_destroy),
        start: Some(output_start),
        stop: Some(output_stop),
        raw_video: None,
        raw_audio: None,
        encoded_packet: Some(output_packet),
        update: None,
        get_defaults: None,
        get_properties: None,
        unused1: None,
        get_total_bytes: Some(output_total_bytes),
        get_dropped_frames: Some(output_dropped),
        type_data: ptr::null_mut(),
        free_type_data: None,
        get_congestion: None,
        get_connect_time_ms: None,
        encoded_video_codecs: ptr::null(),
        encoded_audio_codecs: ptr::null(),
        raw_audio2: None,
        protocols: PROTOCOL.as_ptr(),
    };
    let svc = sys::obs_service_info {
        id: SERVICE_ID.as_ptr(),
        get_name: Some(service_name),
        create: Some(service_create),
        destroy: Some(service_destroy),
        activate: None,
        deactivate: None,
        update: None,
        get_defaults: None,
        get_properties: None,
        initialize: None,
        get_url: None,
        get_key: None,
        get_username: None,
        get_password: None,
        deprecated_1: None,
        apply_encoder_settings: None,
        type_data: ptr::null_mut(),
        free_type_data: None,
        get_output_type: Some(service_output_type),
        get_supported_resolutions: None,
        get_max_fps: None,
        get_max_bitrate: None,
        get_supported_video_codecs: None,
        get_protocol: Some(service_protocol),
        get_supported_audio_codecs: None,
        get_connect_info: Some(service_connect_info),
        can_try_to_connect: Some(service_can_connect),
    };
    unsafe {
        sys::obs_register_output_s(&out, std::mem::size_of::<sys::obs_output_info>());
        sys::obs_register_service_s(&svc, std::mem::size_of::<sys::obs_service_info>());
    }
}

/// true if the profile's current streaming service is the null service.
pub fn is_active() -> bool {
    let current = unsafe { sys::obs_frontend_get_streaming_service() };
    !current.is_null() && cstr(unsafe { sys::obs_service_get_id(current) }) == SERVICE_ID.to_str().unwrap()
}

/// Switches the profile's service (and saves it). Fails if OBS is streaming.
pub fn set_active(enable: bool) -> Result<(), String> {
    unsafe {
        if sys::obs_frontend_streaming_active() {
            return Err(text("MainStreamNull.StopFirst"));
        }
        if enable == is_active() {
            return Ok(());
        }
        let current = sys::obs_frontend_get_streaming_service();
        let replacement = if enable {
            // Store the original service inside the null service's settings.
            let settings = sys::obs_data_create();
            if !current.is_null() {
                let original = sys::obs_service_get_settings(current);
                let hotkeys = sys::obs_hotkeys_save_service(current);
                sys::obs_data_set_string(settings, c"original_type".as_ptr(), sys::obs_service_get_id(current));
                sys::obs_data_set_obj(settings, c"original_settings".as_ptr(), original);
                if !hotkeys.is_null() {
                    sys::obs_data_set_obj(settings, c"original_hotkeys".as_ptr(), hotkeys);
                    sys::obs_data_release(hotkeys);
                }
                sys::obs_data_release(original);
            }
            let s = sys::obs_service_create(SERVICE_ID.as_ptr(), c"default_service".as_ptr(), settings, ptr::null_mut());
            sys::obs_data_release(settings);
            s
        } else {
            let settings = sys::obs_service_get_settings(current);
            let mut kind = cstr(sys::obs_data_get_string(settings, c"original_type".as_ptr()));
            if kind.is_empty() {
                kind = "rtmp_common".into(); // same default as OBS
            }
            let original = sys::obs_data_get_obj(settings, c"original_settings".as_ptr());
            let hotkeys = sys::obs_data_get_obj(settings, c"original_hotkeys".as_ptr());
            let kind = c(&kind);
            let s = sys::obs_service_create(kind.as_ptr(), c"default_service".as_ptr(), original, hotkeys);
            for d in [original, hotkeys, settings] {
                if !d.is_null() {
                    sys::obs_data_release(d);
                }
            }
            s
        };
        if replacement.is_null() {
            return Err(text("MainStreamNull.Failed"));
        }
        sys::obs_frontend_set_streaming_service(replacement); // OBS takes its own reference
        sys::obs_frontend_save_streaming_service();
        crate::info!("main OBS stream service set to '{}'", cstr(sys::obs_service_get_id(replacement)));
        sys::obs_service_release(replacement);
    }
    Ok(())
}
