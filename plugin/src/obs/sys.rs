//! C declarations from libobs 33 and obs-frontend-api used by the plugin.
//!
//! Hand-written (no bindgen/libclang). Structs with a layout are verified
//! twice: here with const asserts and in native/layout_check.cpp against the
//! real headers, with the same numbers.
#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int, c_void};

macro_rules! opaque {
    ($($name:ident),* $(,)?) => {$(
        #[repr(C)]
        pub struct $name { _p: [u8; 0], _m: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)> }
    )*};
}

opaque!(
    config_t, obs_module_t, obs_output_t, obs_encoder_t, obs_service_t, obs_data_t, obs_data_array_t,
    obs_source_t, obs_view_t, obs_properties_t, obs_property_t, lookup_t, signal_handler_t,
    calldata_t, video_t, audio_t, obs_weak_source_t, obs_canvas_t, obs_hotkey_t,
);

pub type obs_hotkey_id = usize;
pub const OBS_TEXT_DEFAULT: c_int = 0;
pub const OBS_TEXT_PASSWORD: c_int = 1;
pub const OBS_TEXT_INFO: c_int = 3;
pub const OBS_COMBO_TYPE_LIST: c_int = 2;
pub const OBS_COMBO_FORMAT_STRING: c_int = 3;
pub const OBS_INVALID_HOTKEY_ID: obs_hotkey_id = usize::MAX;

pub const LOG_ERROR: c_int = 100;
pub const LOG_WARNING: c_int = 200;
pub const LOG_INFO: c_int = 300;
pub const LOG_DEBUG: c_int = 400;

pub const OBS_OUTPUT_VIDEO: u32 = 1 << 0;
pub const OBS_OUTPUT_AUDIO: u32 = 1 << 1;
pub const OBS_OUTPUT_AV: u32 = OBS_OUTPUT_VIDEO | OBS_OUTPUT_AUDIO;
pub const OBS_OUTPUT_ENCODED: u32 = 1 << 2;
pub const OBS_OUTPUT_SERVICE: u32 = 1 << 3;
pub const OBS_OUTPUT_MULTI_TRACK: u32 = 1 << 4;

// obs_output_signal_stop / "stop" signal codes
pub const OBS_OUTPUT_SUCCESS: c_int = 0;
pub const OBS_OUTPUT_BAD_PATH: c_int = -1;
pub const OBS_OUTPUT_CONNECT_FAILED: c_int = -2;
pub const OBS_OUTPUT_INVALID_STREAM: c_int = -3;
pub const OBS_OUTPUT_ERROR: c_int = -4;
pub const OBS_OUTPUT_DISCONNECTED: c_int = -5;
pub const OBS_OUTPUT_UNSUPPORTED: c_int = -6;
pub const OBS_OUTPUT_NO_SPACE: c_int = -7;
pub const OBS_OUTPUT_ENCODE_ERROR: c_int = -8;
pub const OBS_OUTPUT_HDR_DISABLED: c_int = -9;

pub const OBS_OUTPUT_DELAY_PRESERVE: u32 = 1 << 0;
pub const OBS_SCALE_BICUBIC: c_int = 2;

pub const OBS_ENCODER_AUDIO: c_int = 0;
pub const OBS_ENCODER_VIDEO: c_int = 1;

#[repr(C)]
pub struct encoder_packet {
    pub data: *mut u8,
    pub size: usize,
    pub pts: i64,
    pub dts: i64,
    pub timebase_num: i32,
    pub timebase_den: i32,
    pub type_: c_int,
    pub keyframe: bool,
    pub dts_usec: i64,
    pub sys_dts_usec: i64,
    pub priority: c_int,
    pub drop_priority: c_int,
    pub track_idx: usize,
    pub encoder: *mut obs_encoder_t,
}

type Fp<T> = Option<T>;

#[repr(C)]
pub struct obs_output_info {
    pub id: *const c_char,
    pub flags: u32,
    pub get_name: Fp<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub create: Fp<unsafe extern "C" fn(*mut obs_data_t, *mut obs_output_t) -> *mut c_void>,
    pub destroy: Fp<unsafe extern "C" fn(*mut c_void)>,
    pub start: Fp<unsafe extern "C" fn(*mut c_void) -> bool>,
    pub stop: Fp<unsafe extern "C" fn(*mut c_void, u64)>,
    pub raw_video: Fp<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    pub raw_audio: Fp<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    pub encoded_packet: Fp<unsafe extern "C" fn(*mut c_void, *mut encoder_packet)>,
    pub update: Fp<unsafe extern "C" fn(*mut c_void, *mut obs_data_t)>,
    pub get_defaults: Fp<unsafe extern "C" fn(*mut obs_data_t)>,
    pub get_properties: Fp<unsafe extern "C" fn(*mut c_void) -> *mut obs_properties_t>,
    pub unused1: Fp<unsafe extern "C" fn()>,
    pub get_total_bytes: Fp<unsafe extern "C" fn(*mut c_void) -> u64>,
    pub get_dropped_frames: Fp<unsafe extern "C" fn(*mut c_void) -> c_int>,
    pub type_data: *mut c_void,
    pub free_type_data: Fp<unsafe extern "C" fn(*mut c_void)>,
    pub get_congestion: Fp<unsafe extern "C" fn(*mut c_void) -> f32>,
    pub get_connect_time_ms: Fp<unsafe extern "C" fn(*mut c_void) -> c_int>,
    pub encoded_video_codecs: *const c_char,
    pub encoded_audio_codecs: *const c_char,
    pub raw_audio2: Fp<unsafe extern "C" fn(*mut c_void, usize, *mut c_void)>,
    pub protocols: *const c_char,
}

#[repr(C)]
pub struct obs_service_info {
    pub id: *const c_char,
    pub get_name: Fp<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub create: Fp<unsafe extern "C" fn(*mut obs_data_t, *mut obs_service_t) -> *mut c_void>,
    pub destroy: Fp<unsafe extern "C" fn(*mut c_void)>,
    pub activate: Fp<unsafe extern "C" fn(*mut c_void, *mut obs_data_t)>,
    pub deactivate: Fp<unsafe extern "C" fn(*mut c_void)>,
    pub update: Fp<unsafe extern "C" fn(*mut c_void, *mut obs_data_t)>,
    pub get_defaults: Fp<unsafe extern "C" fn(*mut obs_data_t)>,
    pub get_properties: Fp<unsafe extern "C" fn(*mut c_void) -> *mut obs_properties_t>,
    pub initialize: Fp<unsafe extern "C" fn(*mut c_void, *mut obs_output_t) -> bool>,
    pub get_url: Fp<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub get_key: Fp<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub get_username: Fp<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub get_password: Fp<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub deprecated_1: Fp<unsafe extern "C" fn()>,
    pub apply_encoder_settings: Fp<unsafe extern "C" fn(*mut c_void, *mut obs_data_t, *mut obs_data_t)>,
    pub type_data: *mut c_void,
    pub free_type_data: Fp<unsafe extern "C" fn(*mut c_void)>,
    pub get_output_type: Fp<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub get_supported_resolutions: Fp<unsafe extern "C" fn(*mut c_void, *mut *mut c_void, *mut usize)>,
    pub get_max_fps: Fp<unsafe extern "C" fn(*mut c_void, *mut c_int)>,
    pub get_max_bitrate: Fp<unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int)>,
    pub get_supported_video_codecs: Fp<unsafe extern "C" fn(*mut c_void) -> *mut *const c_char>,
    pub get_protocol: Fp<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    pub get_supported_audio_codecs: Fp<unsafe extern "C" fn(*mut c_void) -> *mut *const c_char>,
    pub get_connect_info: Fp<unsafe extern "C" fn(*mut c_void, u32) -> *const c_char>,
    pub can_try_to_connect: Fp<unsafe extern "C" fn(*mut c_void) -> bool>,
}

// Same numbers as native/layout_check.cpp (measured with MSVC against OBS 33.0.1).
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<obs_output_info>() == 192);
    assert!(offset_of!(obs_output_info, flags) == 8);
    assert!(offset_of!(obs_output_info, encoded_packet) == 72);
    assert!(offset_of!(obs_output_info, get_total_bytes) == 112);
    assert!(offset_of!(obs_output_info, get_dropped_frames) == 120);
    assert!(offset_of!(obs_output_info, encoded_video_codecs) == 160);
    assert!(offset_of!(obs_output_info, protocols) == 184);
    assert!(size_of::<obs_service_info>() == 216);
    assert!(offset_of!(obs_service_info, get_output_type) == 144);
    assert!(offset_of!(obs_service_info, get_protocol) == 184);
    assert!(offset_of!(obs_service_info, get_connect_info) == 200);
    assert!(offset_of!(obs_service_info, can_try_to_connect) == 208);
    assert!(size_of::<encoder_packet>() == 88);
    assert!(offset_of!(encoder_packet, size) == 8);
    assert!(offset_of!(encoder_packet, pts) == 16);
    assert!(offset_of!(encoder_packet, timebase_num) == 32);
    assert!(offset_of!(encoder_packet, type_) == 40);
    assert!(offset_of!(encoder_packet, keyframe) == 44);
    assert!(offset_of!(encoder_packet, dts_usec) == 48);
    assert!(offset_of!(encoder_packet, track_idx) == 72);
    assert!(offset_of!(encoder_packet, encoder) == 80);
};

pub type signal_callback_t = unsafe extern "C" fn(*mut c_void, *mut calldata_t);
pub type obs_hotkey_func = unsafe extern "C" fn(*mut c_void, obs_hotkey_id, *mut obs_hotkey_t, bool);
pub type obs_frontend_event_cb = unsafe extern "C" fn(c_int, *mut c_void);

extern "C" {
    // ---- base / module
    pub fn blog(level: c_int, format: *const c_char, ...);
    pub fn obs_module_load_locale(module: *mut obs_module_t, default_locale: *const c_char, locale: *const c_char) -> *mut lookup_t;
    pub fn text_lookup_getstr(lookup: *mut lookup_t, lookup_val: *const c_char, out: *mut *const c_char) -> bool;
    pub fn text_lookup_destroy(lookup: *mut lookup_t);
    pub fn bfree(ptr: *mut c_void);
    pub fn os_quick_write_utf8_file_safe(path: *const c_char, s: *const c_char, len: usize, marker: bool, temp_ext: *const c_char, backup_ext: *const c_char) -> bool;
    pub fn config_get_string(config: *mut config_t, section: *const c_char, name: *const c_char) -> *const c_char;
    pub fn config_get_int(config: *mut config_t, section: *const c_char, name: *const c_char) -> i64;
    pub fn config_get_bool(config: *mut config_t, section: *const c_char, name: *const c_char) -> bool;

    // ---- obs_data
    pub fn obs_data_create() -> *mut obs_data_t;
    pub fn obs_data_create_from_json(json: *const c_char) -> *mut obs_data_t;
    pub fn obs_data_create_from_json_file_safe(file: *const c_char, backup_ext: *const c_char) -> *mut obs_data_t;
    pub fn obs_data_get_json(data: *mut obs_data_t) -> *const c_char;
    pub fn obs_data_get_json_with_defaults(data: *mut obs_data_t) -> *const c_char;
    pub fn obs_data_save_json_safe(data: *mut obs_data_t, file: *const c_char, temp_ext: *const c_char, backup_ext: *const c_char) -> bool;
    pub fn obs_data_release(data: *mut obs_data_t);
    pub fn obs_data_addref(data: *mut obs_data_t);
    pub fn obs_data_get_string(data: *mut obs_data_t, name: *const c_char) -> *const c_char;
    pub fn obs_data_get_int(data: *mut obs_data_t, name: *const c_char) -> i64;
    pub fn obs_data_get_bool(data: *mut obs_data_t, name: *const c_char) -> bool;
    pub fn obs_data_get_obj(data: *mut obs_data_t, name: *const c_char) -> *mut obs_data_t;
    pub fn obs_data_get_array(data: *mut obs_data_t, name: *const c_char) -> *mut obs_data_array_t;
    pub fn obs_data_set_string(data: *mut obs_data_t, name: *const c_char, val: *const c_char);
    pub fn obs_data_set_int(data: *mut obs_data_t, name: *const c_char, val: i64);
    pub fn obs_data_set_bool(data: *mut obs_data_t, name: *const c_char, val: bool);
    pub fn obs_data_set_obj(data: *mut obs_data_t, name: *const c_char, obj: *mut obs_data_t);
    pub fn obs_data_set_array(data: *mut obs_data_t, name: *const c_char, arr: *mut obs_data_array_t);
    pub fn obs_data_set_default_string(data: *mut obs_data_t, name: *const c_char, val: *const c_char);
    pub fn obs_data_set_default_int(data: *mut obs_data_t, name: *const c_char, val: i64);
    pub fn obs_data_array_release(arr: *mut obs_data_array_t);
    pub fn obs_data_array_count(arr: *mut obs_data_array_t) -> usize;
    pub fn obs_data_array_item(arr: *mut obs_data_array_t, idx: usize) -> *mut obs_data_t;

    // ---- outputs
    pub fn obs_register_output_s(info: *const obs_output_info, size: usize);
    pub fn obs_output_create(id: *const c_char, name: *const c_char, settings: *mut obs_data_t, hotkey_data: *mut obs_data_t) -> *mut obs_output_t;
    pub fn obs_output_release(output: *mut obs_output_t);
    pub fn obs_output_get_ref(output: *mut obs_output_t) -> *mut obs_output_t;
    pub fn obs_output_start(output: *mut obs_output_t) -> bool;
    pub fn obs_output_stop(output: *mut obs_output_t);
    pub fn obs_output_force_stop(output: *mut obs_output_t);
    pub fn obs_output_active(output: *const obs_output_t) -> bool;
    pub fn obs_output_reconnecting(output: *const obs_output_t) -> bool;
    pub fn obs_output_get_name(output: *const obs_output_t) -> *const c_char;
    pub fn obs_output_get_id(output: *const obs_output_t) -> *const c_char;
    pub fn obs_output_get_settings(output: *const obs_output_t) -> *mut obs_data_t;
    pub fn obs_output_get_service(output: *const obs_output_t) -> *mut obs_service_t;
    pub fn obs_output_update(output: *mut obs_output_t, settings: *mut obs_data_t);
    pub fn obs_output_set_service(output: *mut obs_output_t, service: *mut obs_service_t);
    pub fn obs_output_set_video_encoder(output: *mut obs_output_t, encoder: *mut obs_encoder_t);
    pub fn obs_output_set_audio_encoder(output: *mut obs_output_t, encoder: *mut obs_encoder_t, idx: usize);
    pub fn obs_output_get_video_encoder(output: *const obs_output_t) -> *mut obs_encoder_t;
    pub fn obs_output_get_audio_encoder(output: *const obs_output_t, idx: usize) -> *mut obs_encoder_t;
    pub fn obs_output_set_delay(output: *mut obs_output_t, delay_sec: u32, flags: u32);
    pub fn obs_output_set_reconnect_settings(output: *mut obs_output_t, retry_count: c_int, retry_sec: c_int);
    pub fn obs_output_get_total_bytes(output: *const obs_output_t) -> u64;
    pub fn obs_output_get_total_frames(output: *const obs_output_t) -> c_int;
    pub fn obs_output_get_frames_dropped(output: *const obs_output_t) -> c_int;
    pub fn obs_output_get_last_error(output: *mut obs_output_t) -> *const c_char;
    pub fn obs_output_set_last_error(output: *mut obs_output_t, message: *const c_char);
    pub fn obs_output_get_signal_handler(output: *const obs_output_t) -> *mut signal_handler_t;
    pub fn obs_output_can_begin_data_capture(output: *const obs_output_t, flags: u32) -> bool;
    pub fn obs_output_initialize_encoders(output: *mut obs_output_t, flags: u32) -> bool;
    pub fn obs_output_begin_data_capture(output: *mut obs_output_t, flags: u32) -> bool;
    pub fn obs_output_end_data_capture(output: *mut obs_output_t);
    pub fn obs_output_signal_stop(output: *mut obs_output_t, code: c_int);

    // ---- encoders
    pub fn obs_video_encoder_create(id: *const c_char, name: *const c_char, settings: *mut obs_data_t, hotkey_data: *mut obs_data_t) -> *mut obs_encoder_t;
    pub fn obs_audio_encoder_create(id: *const c_char, name: *const c_char, settings: *mut obs_data_t, mixer_idx: usize, hotkey_data: *mut obs_data_t) -> *mut obs_encoder_t;
    pub fn obs_encoder_release(encoder: *mut obs_encoder_t);
    pub fn obs_encoder_get_ref(encoder: *mut obs_encoder_t) -> *mut obs_encoder_t;
    pub fn obs_encoder_get_codec(encoder: *const obs_encoder_t) -> *const c_char;
    pub fn obs_encoder_get_id(encoder: *const obs_encoder_t) -> *const c_char;
    pub fn obs_encoder_get_name(encoder: *const obs_encoder_t) -> *const c_char;
    pub fn obs_encoder_get_extra_data(encoder: *const obs_encoder_t, extra_data: *mut *mut u8, size: *mut usize) -> bool;
    pub fn obs_encoder_get_settings(encoder: *const obs_encoder_t) -> *mut obs_data_t;
    pub fn obs_encoder_get_defaults(encoder: *const obs_encoder_t) -> *mut obs_data_t;
    pub fn obs_get_encoder_by_name(name: *const c_char) -> *mut obs_encoder_t;
    pub fn obs_encoder_set_gpu_scale_type(encoder: *mut obs_encoder_t, scale_type: c_int);
    pub fn obs_encoder_parent_video(encoder: *const obs_encoder_t) -> *mut video_t;
    pub fn obs_encoder_update(encoder: *mut obs_encoder_t, settings: *mut obs_data_t);
    pub fn obs_encoder_active(encoder: *const obs_encoder_t) -> bool;
    pub fn obs_encoder_set_video(encoder: *mut obs_encoder_t, video: *mut video_t);
    pub fn obs_encoder_set_audio(encoder: *mut obs_encoder_t, audio: *mut audio_t);
    pub fn obs_encoder_set_scaled_size(encoder: *mut obs_encoder_t, width: u32, height: u32);
    pub fn obs_encoder_set_frame_rate_divisor(encoder: *mut obs_encoder_t, divisor: u32) -> bool;
    pub fn obs_encoder_get_width(encoder: *const obs_encoder_t) -> u32;
    pub fn obs_encoder_get_height(encoder: *const obs_encoder_t) -> u32;
    pub fn obs_encoder_get_sample_rate(encoder: *const obs_encoder_t) -> u32;
    pub fn obs_encoder_get_frame_rate_divisor(encoder: *const obs_encoder_t) -> u32;
    pub fn obs_get_encoder_properties(id: *const c_char) -> *mut obs_properties_t;
    pub fn obs_encoder_defaults(id: *const c_char) -> *mut obs_data_t;
    pub fn obs_get_encoder_codec(id: *const c_char) -> *const c_char;
    pub fn obs_enum_encoder_types(idx: usize, id: *mut *const c_char) -> bool;
    pub fn obs_get_encoder_type(id: *const c_char) -> c_int;
    pub fn obs_encoder_get_display_name(id: *const c_char) -> *const c_char;
    pub fn obs_get_encoder_caps(encoder_id: *const c_char) -> u32;
    pub fn obs_get_video() -> *mut video_t;
    pub fn obs_encoder_video(encoder: *const obs_encoder_t) -> *mut video_t;
    pub fn obs_encoder_audio(encoder: *const obs_encoder_t) -> *mut audio_t;
    pub fn video_output_get_frame_rate(video: *const video_t) -> f64;
    pub fn audio_output_get_channels(audio: *const audio_t) -> usize;
    pub fn obs_parse_avc_header(header: *mut *mut u8, data: *const u8, size: usize) -> usize;
    pub fn obs_get_audio() -> *mut audio_t;

    // ---- properties
    pub fn obs_properties_create() -> *mut obs_properties_t;
    pub fn obs_properties_destroy(props: *mut obs_properties_t);
    pub fn obs_properties_add_text(props: *mut obs_properties_t, name: *const c_char, desc: *const c_char, type_: c_int) -> *mut obs_property_t;
    pub fn obs_properties_add_list(
        props: *mut obs_properties_t,
        name: *const c_char,
        desc: *const c_char,
        type_: c_int,
        format: c_int,
    ) -> *mut obs_property_t;
    pub fn obs_property_list_add_string(p: *mut obs_property_t, name: *const c_char, val: *const c_char) -> usize;
    pub fn obs_properties_add_int(
        props: *mut obs_properties_t,
        name: *const c_char,
        desc: *const c_char,
        min: c_int,
        max: c_int,
        step: c_int,
    ) -> *mut obs_property_t;

    // ---- services
    pub fn obs_register_service_s(info: *const obs_service_info, size: usize);
    pub fn obs_service_create(id: *const c_char, name: *const c_char, settings: *mut obs_data_t, hotkey_data: *mut obs_data_t) -> *mut obs_service_t;
    pub fn obs_service_release(service: *mut obs_service_t);
    pub fn obs_service_get_ref(service: *mut obs_service_t) -> *mut obs_service_t;
    pub fn obs_service_get_id(service: *const obs_service_t) -> *const c_char;
    pub fn obs_service_get_settings(service: *const obs_service_t) -> *mut obs_data_t;
    pub fn obs_service_apply_encoder_settings(service: *mut obs_service_t, video: *mut obs_data_t, audio: *mut obs_data_t);
    pub fn obs_hotkeys_save_service(service: *mut obs_service_t) -> *mut obs_data_t;

    // ---- sources / views (per-target scene)
    pub fn obs_get_source_by_name(name: *const c_char) -> *mut obs_source_t;
    pub fn obs_source_release(source: *mut obs_source_t);
    pub fn obs_source_get_ref(source: *mut obs_source_t) -> *mut obs_source_t;
    pub fn obs_view_create() -> *mut obs_view_t;
    pub fn obs_view_destroy(view: *mut obs_view_t);
    pub fn obs_view_set_source(view: *mut obs_view_t, channel: u32, source: *mut obs_source_t);
    pub fn obs_view_remove(view: *mut obs_view_t);
    pub fn obs_view_add(view: *mut obs_view_t) -> *mut video_t;
    pub fn obs_source_inc_active(source: *mut obs_source_t);
    pub fn obs_source_dec_active(source: *mut obs_source_t);
    pub fn obs_source_get_name(source: *const obs_source_t) -> *const c_char;

    // ---- signals
    pub fn signal_handler_connect(handler: *mut signal_handler_t, signal: *const c_char, callback: signal_callback_t, data: *mut c_void);
    pub fn signal_handler_disconnect(handler: *mut signal_handler_t, signal: *const c_char, callback: signal_callback_t, data: *mut c_void);
    /// calldata_get_int is inline in the headers; this is the exported function it uses.
    pub fn calldata_get_data(data: *const calldata_t, name: *const c_char, out: *mut c_void, size: usize) -> bool;

    // ---- hotkeys
    pub fn obs_hotkey_register_frontend(name: *const c_char, description: *const c_char, func: obs_hotkey_func, data: *mut c_void) -> obs_hotkey_id;
    pub fn obs_hotkey_unregister(id: obs_hotkey_id);
    pub fn obs_hotkey_save(id: obs_hotkey_id) -> *mut obs_data_array_t;
    pub fn obs_hotkey_load(id: obs_hotkey_id, data: *mut obs_data_array_t);
    pub fn obs_hotkey_set_description(id: obs_hotkey_id, desc: *const c_char);

    // ---- frontend
    pub fn obs_frontend_get_main_window() -> *mut c_void;
    pub fn obs_frontend_add_event_callback(callback: obs_frontend_event_cb, private_data: *mut c_void);
    pub fn obs_frontend_remove_event_callback(callback: obs_frontend_event_cb, private_data: *mut c_void);
    pub fn obs_frontend_get_streaming_output() -> *mut obs_output_t;
    pub fn obs_frontend_get_recording_output() -> *mut obs_output_t;
    pub fn obs_frontend_streaming_active() -> bool;
    pub fn obs_frontend_recording_active() -> bool;
    pub fn obs_frontend_get_streaming_service() -> *mut obs_service_t;
    pub fn obs_frontend_set_streaming_service(service: *mut obs_service_t);
    pub fn obs_frontend_save_streaming_service();
    pub fn obs_frontend_get_current_profile_path() -> *mut c_char;
    pub fn obs_frontend_get_profile_config() -> *mut config_t;
}
