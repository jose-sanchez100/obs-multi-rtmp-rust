//! Compiles the Qt (C++) layer and the layout check against the OBS headers,
//! and links libobs, obs-frontend-api and Qt 6.
//!
//! Environment variables:
//! - OBS_DEPS_DIR: directory containing OBS include/ and lib/ plus the Qt directory
//!   (default ../../obs-multi-rtmp/.deps, as prepared by the original plugin's buildspec).
//! - OBS_QT_DIR: Qt 6 directory (default <OBS_DEPS_DIR>/obs-deps-qt6-2026-10-23-x64).
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let deps = env::var("OBS_DEPS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../../obs-multi-rtmp/.deps"));
    let qt = env::var("OBS_QT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| deps.join("obs-deps-qt6-2026-10-23-x64"));
    let obs_include = deps.join("include");
    assert!(obs_include.join("obs-module.h").exists(), "OBS headers not found in {}", obs_include.display());
    assert!(qt.join("include/QtWidgets").exists(), "Qt 6 not found in {}", qt.display());

    let mut b = cc::Build::new();
    b.cpp(true)
        .std("c++17")
        .flag("/utf-8")
        .flag("/Zc:__cplusplus")
        .flag("/permissive-")
        .flag("/EHsc")
        .define("UNICODE", None)
        .define("_UNICODE", None)
        .define("QT_NO_KEYWORDS", None)
        .include(manifest.join("native/third_party/nlohmann"))
        .include(&obs_include)
        .include(qt.join("include"))
        .include(qt.join("include/QtCore"))
        .include(qt.join("include/QtGui"))
        .include(qt.join("include/QtWidgets"));
    for f in std::fs::read_dir(manifest.join("native")).unwrap() {
        let path = f.unwrap().path();
        if path.extension().is_some_and(|e| e == "cpp") {
            println!("cargo:rerun-if-changed={}", path.display());
            b.file(&path);
        } else if path.extension().is_some_and(|e| e == "h") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    b.compile("omr_native");

    println!("cargo:rustc-link-search=native={}", deps.join("lib").display());
    println!("cargo:rustc-link-search=native={}", qt.join("lib").display());
    for lib in ["obs", "obs-frontend-api", "Qt6Core", "Qt6Gui", "Qt6Widgets"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!("cargo:rerun-if-env-changed=OBS_DEPS_DIR");
    println!("cargo:rerun-if-env-changed=OBS_QT_DIR");
}
