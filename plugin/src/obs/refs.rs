//! Owning (RAII) references to libobs objects: released when they go out of scope.
use super::{c, cstr, sys};
use std::ffi::CStr;
use std::ptr::NonNull;

macro_rules! owned {
    ($name:ident, $raw:ty, $release:path, $addref:path) => {
        pub struct $name(NonNull<$raw>);

        // libobs protects its objects with its own atomic reference counting.
        unsafe impl Send for $name {}
        unsafe impl Sync for $name {}

        impl $name {
            /// Takes ownership of an already counted reference (result of *_create / *_get_*).
            pub unsafe fn from_owned(p: *mut $raw) -> Option<Self> {
                NonNull::new(p).map(Self)
            }
            /// Adds a reference to a borrowed pointer.
            pub unsafe fn from_borrowed(p: *mut $raw) -> Option<Self> {
                if p.is_null() {
                    None
                } else {
                    Self::from_owned($addref(p))
                }
            }
            pub fn as_ptr(&self) -> *mut $raw {
                self.0.as_ptr()
            }
        }

        impl Clone for $name {
            fn clone(&self) -> Self {
                unsafe { Self::from_borrowed(self.as_ptr()) }.expect("addref failed")
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                unsafe { $release(self.0.as_ptr()) }
            }
        }
    };
}

owned!(Output, sys::obs_output_t, sys::obs_output_release, sys::obs_output_get_ref);
owned!(Encoder, sys::obs_encoder_t, sys::obs_encoder_release, sys::obs_encoder_get_ref);
owned!(Service, sys::obs_service_t, sys::obs_service_release, sys::obs_service_get_ref);
owned!(Source, sys::obs_source_t, sys::obs_source_release, sys::obs_source_get_ref);

unsafe fn data_addref(p: *mut sys::obs_data_t) -> *mut sys::obs_data_t {
    sys::obs_data_addref(p);
    p
}
owned!(Data, sys::obs_data_t, sys::obs_data_release, data_addref);

impl Data {
    pub fn new() -> Self {
        unsafe { Self::from_owned(sys::obs_data_create()) }.expect("obs_data_create")
    }
    pub fn from_json(json: &str) -> Option<Self> {
        let j = c(json);
        unsafe { Self::from_owned(sys::obs_data_create_from_json(j.as_ptr())) }
    }
    pub fn json(&self) -> String {
        cstr(unsafe { sys::obs_data_get_json(self.as_ptr()) })
    }
    pub fn get_str(&self, key: &CStr) -> String {
        cstr(unsafe { sys::obs_data_get_string(self.as_ptr(), key.as_ptr()) })
    }
    pub fn get_int(&self, key: &CStr) -> i64 {
        unsafe { sys::obs_data_get_int(self.as_ptr(), key.as_ptr()) }
    }
    pub fn set_str(&self, key: &CStr, val: &str) {
        let v = c(val);
        unsafe { sys::obs_data_set_string(self.as_ptr(), key.as_ptr(), v.as_ptr()) }
    }
    pub fn set_int(&self, key: &CStr, val: i64) {
        unsafe { sys::obs_data_set_int(self.as_ptr(), key.as_ptr(), val) }
    }
    pub fn set_bool(&self, key: &CStr, val: bool) {
        unsafe { sys::obs_data_set_bool(self.as_ptr(), key.as_ptr(), val) }
    }
}

impl Default for Data {
    fn default() -> Self {
        Self::new()
    }
}

impl Output {
    pub fn name(&self) -> String {
        cstr(unsafe { sys::obs_output_get_name(self.as_ptr()) })
    }
    pub fn active(&self) -> bool {
        unsafe { sys::obs_output_active(self.as_ptr()) }
    }
    pub fn last_error(&self) -> String {
        cstr(unsafe { sys::obs_output_get_last_error(self.as_ptr()) })
    }
}

impl Encoder {
    pub fn codec(&self) -> String {
        cstr(unsafe { sys::obs_encoder_get_codec(self.as_ptr()) })
    }
    pub fn active(&self) -> bool {
        unsafe { sys::obs_encoder_active(self.as_ptr()) }
    }
}
