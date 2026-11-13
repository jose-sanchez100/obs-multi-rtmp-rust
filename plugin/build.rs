//! Compiles the Qt (C++) layer and the layout check against the OBS headers,
//! and links libobs, obs-frontend-api and Qt 6.
//!
//! Windows (MSVC), environment variables:
//! - OBS_DEPS_DIR: directory containing OBS include/ and lib/ plus the Qt directory
//!   (default ../../obs-multi-rtmp/.deps, as prepared by the original plugin's buildspec).
//! - OBS_QT_DIR: Qt 6 directory (default <OBS_DEPS_DIR>/obs-deps-qt6-2026-10-23-x64).
//!
//! Linux: OBS and Qt 6 are found with pkg-config (`libobs`, `Qt6Widgets`), as installed
//! by the distribution's OBS and Qt development packages. Optional overrides:
//! - OBS_INCLUDE_DIR / OBS_LIB_DIR: OBS headers (the folder with obs-module.h) and libraries.
//! - OBS_QT_DIR: Qt 6 directory with include/ and lib/.
use std::env;
use std::path::{Path, PathBuf};

/// Include directories, library directories and libraries found for OBS and Qt.
/// The libraries are emitted after the static C++ library, which uses them: with
/// `--as-needed` (Linux) a shared library listed before its users is dropped.
#[derive(Default)]
struct Deps {
    obs_include: Vec<PathBuf>,
    qt_include: Vec<PathBuf>,
    lib_dirs: Vec<PathBuf>,
    libs: Vec<String>,
}

impl Deps {
    fn link(&mut self, dirs: &[PathBuf], libs: &[&str]) {
        self.lib_dirs.extend_from_slice(dirs);
        self.libs.extend(libs.iter().map(|l| l.to_string()));
    }

    fn link_pkg(&mut self, lib: &pkg_config::Library) {
        self.lib_dirs.extend_from_slice(&lib.link_paths);
        self.libs.extend(lib.libs.iter().cloned());
    }
}

fn pkg(name: &str, hint: &str) -> pkg_config::Library {
    pkg_config::Config::new()
        .cargo_metadata(false)
        .probe(name)
        .unwrap_or_else(|e| panic!("{name} not found with pkg-config ({hint}): {e}"))
}

fn qt_dir_includes(qt: &Path) -> Vec<PathBuf> {
    assert!(qt.join("include/QtWidgets").exists(), "Qt 6 not found in {}", qt.display());
    ["", "QtCore", "QtGui", "QtWidgets"].iter().map(|m| qt.join("include").join(m)).collect()
}

fn windows_deps(manifest: &Path) -> Deps {
    let deps = env::var("OBS_DEPS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../../obs-multi-rtmp/.deps"));
    let qt = env::var("OBS_QT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| deps.join("obs-deps-qt6-2026-10-23-x64"));
    let obs_include = deps.join("include");
    assert!(obs_include.join("obs-module.h").exists(), "OBS headers not found in {}", obs_include.display());
    let mut d = Deps { obs_include: vec![obs_include], qt_include: qt_dir_includes(&qt), ..Default::default() };
    d.link(&[deps.join("lib"), qt.join("lib")], &["obs", "obs-frontend-api", "Qt6Core", "Qt6Gui", "Qt6Widgets"]);
    d
}

fn pkg_config_deps() -> Deps {
    let mut d = Deps::default();
    match env::var("OBS_INCLUDE_DIR") {
        Ok(dir) => {
            let lib_dirs: Vec<PathBuf> = env::var("OBS_LIB_DIR").map(PathBuf::from).into_iter().collect();
            d.obs_include.push(PathBuf::from(dir));
            d.link(&lib_dirs, &["obs"]);
        }
        Err(_) => {
            let obs = pkg("libobs", "install the OBS development package or set OBS_INCLUDE_DIR");
            d.obs_include = obs.include_paths.clone();
            d.link_pkg(&obs);
        }
    }
    // OBS installs its headers in <includedir>/obs, but libobs.pc may only point at
    // <includedir> (Ubuntu: Cflags without -I, so pkg-config reports /usr/include).
    let nested: Vec<PathBuf> = d.obs_include.iter().map(|dir| dir.join("obs")).filter(|dir| dir.join("obs-module.h").exists()).collect();
    d.obs_include.extend(nested);
    assert!(
        d.obs_include.iter().any(|dir| dir.join("obs-module.h").exists()),
        "obs-module.h not found in {:?}",
        d.obs_include
    );
    // obs-frontend-api has no pkg-config file; it is installed next to libobs.
    d.link(&[], &["obs-frontend-api"]);

    match env::var("OBS_QT_DIR") {
        Ok(qt) => {
            let qt = PathBuf::from(qt);
            d.qt_include = qt_dir_includes(&qt);
            d.link(&[qt.join("lib")], &["Qt6Core", "Qt6Gui", "Qt6Widgets"]);
        }
        Err(_) => {
            let qt = pkg("Qt6Widgets", "install the Qt 6 base development package or set OBS_QT_DIR");
            d.qt_include = qt.include_paths.clone();
            d.link_pkg(&qt);
        }
    }
    d
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let windows = env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows";
    let deps = if windows { windows_deps(&manifest) } else { pkg_config_deps() };

    let mut b = cc::Build::new();
    b.cpp(true).std("c++17").define("QT_NO_KEYWORDS", None);
    if env::var("CARGO_CFG_TARGET_ENV").unwrap() == "msvc" {
        b.flag("/utf-8")
            .flag("/Zc:__cplusplus")
            .flag("/permissive-")
            .flag("/EHsc")
            .define("UNICODE", None)
            .define("_UNICODE", None);
    } else {
        // Qt 6 requires position-independent code; the plugin is a shared library anyway.
        b.pic(true).flag_if_supported("-Wno-deprecated-declarations");
    }
    b.include(manifest.join("native/third_party/nlohmann"));
    for dir in deps.obs_include.iter().chain(&deps.qt_include) {
        b.include(dir);
    }
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

    for dir in &deps.lib_dirs {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    for lib in &deps.libs {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }

    for var in ["OBS_DEPS_DIR", "OBS_QT_DIR", "OBS_INCLUDE_DIR", "OBS_LIB_DIR"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}
