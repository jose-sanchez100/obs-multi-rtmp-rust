//! Bridge to the Qt layer (native/). The C++ layer only draws: it requests the
//! view model as JSON, sends commands as JSON and opens the dialogs Rust asks for.
//! All plugin state lives on the UI thread (see core.rs).
use crate::obs::c;
use std::ffi::{c_char, c_int, c_void, CStr, CString};

extern "C" {
    fn omr_ui_create_dock() -> bool;
    fn omr_ui_post(func: unsafe extern "C" fn(*mut c_void), data: *mut c_void);
    fn omr_ui_refresh();
    fn omr_ui_question(title: *const c_char, text: *const c_char) -> bool;
    fn omr_ui_message(icon: c_int, title: *const c_char, text: *const c_char);
}

pub fn create_dock() -> bool {
    unsafe { omr_ui_create_dock() }
}

type Task = Box<dyn FnOnce() + Send>;

unsafe extern "C" fn run_task(data: *mut c_void) {
    let task = Box::from_raw(data as *mut Task);
    task();
}

/// Runs `f` on the UI thread, always queued (never synchronous), so it never
/// re-enters state the caller may currently be borrowing.
pub fn post(f: impl FnOnce() + Send + 'static) {
    let task: Box<Task> = Box::new(Box::new(f));
    unsafe { omr_ui_post(run_task, Box::into_raw(task) as *mut c_void) }
}

/// Asks the dock to re-read the model (coalesced in the Qt layer).
pub fn refresh() {
    unsafe { omr_ui_refresh() }
}

/// Modal Yes/No dialog. Do not call while plugin state is borrowed.
pub fn question(title: &str, text: &str) -> bool {
    let (t, x) = (c(title), c(text));
    unsafe { omr_ui_question(t.as_ptr(), x.as_ptr()) }
}

pub enum Icon {
    Info = 0,
    Warning = 1,
    Critical = 2,
}

/// Modal message box. Do not call while plugin state is borrowed.
pub fn message(icon: Icon, title: &str, text: &str) {
    let (t, x) = (c(title), c(text));
    unsafe { omr_ui_message(icon as c_int, t.as_ptr(), x.as_ptr()) }
}

pub fn into_c_string(s: String) -> *mut c_char {
    CString::new(s.replace('\0', "")).unwrap().into_raw()
}

pub unsafe fn from_c(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

/// Frees strings returned to the Qt layer by the omr_* functions.
#[no_mangle]
pub unsafe extern "C" fn omr_free(p: *mut c_char) {
    if !p.is_null() {
        drop(CString::from_raw(p));
    }
}
