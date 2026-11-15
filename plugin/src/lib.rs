//! obs-multi-rtmp: streaming to multiple destinations. All logic lives in Rust;
//! the C++ layer (native/) only draws the dock and the editor with Qt and talks
//! to Rust through the C bridge in ui.rs / core.rs.
pub mod config;
pub mod core;
pub mod hotkeys;
pub mod module;
pub mod null_stream;
pub mod obs;
pub mod protocols;
pub mod rust_output;
pub mod rust_service;
pub mod scene_view;
pub mod target;
pub mod ui;

pub(crate) fn plugin_load() -> bool {
    if unsafe { obs::sys::obs_frontend_get_main_window() }.is_null() {
        return false;
    }
    obs::install_logger();
    rust_service::register();
    rust_output::register();
    null_stream::register();
    core::init();
    if !ui::create_dock() {
        core::shutdown();
        return false;
    }
    info!("version {} (Rust) by SoraYuki https://github.com/sorayuki/obs-multi-rtmp/", env!("CARGO_PKG_VERSION"));
    true
}

pub(crate) fn plugin_unload() {
    core::shutdown();
}
