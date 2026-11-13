//! Per-encoder scene view (for targets whose scene differs from the program
//! scene). One view per encoder, reference-counted and shared by the targets
//! that share that encoder; the video of an active encoder is never reassigned.
use crate::obs::{c, cstr, sys, Encoder, Source};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

pub struct SceneView {
    view: *mut sys::obs_view_t,
    source: Mutex<Source>,
    _encoder: Encoder,
}

unsafe impl Send for SceneView {}
unsafe impl Sync for SceneView {}

impl SceneView {
    /// Switches the view's scene on the fly.
    pub fn set_source(&self, scene: Source) {
        let mut cur = self.source.lock().unwrap();
        if cur.as_ptr() == scene.as_ptr() {
            return;
        }
        unsafe {
            sys::obs_view_set_source(self.view, 0, scene.as_ptr());
            sys::obs_source_inc_active(scene.as_ptr());
            sys::obs_source_dec_active(cur.as_ptr());
        }
        *cur = scene;
    }

    pub fn scene_name(&self) -> String {
        cstr(unsafe { sys::obs_source_get_name(self.source.lock().unwrap().as_ptr()) })
    }
}

impl Drop for SceneView {
    fn drop(&mut self) {
        unsafe {
            sys::obs_view_remove(self.view);
            sys::obs_source_dec_active(self.source.lock().unwrap().as_ptr());
            sys::obs_view_set_source(self.view, 0, std::ptr::null_mut());
            sys::obs_view_destroy(self.view);
        }
    }
}

fn views() -> &'static Mutex<HashMap<usize, Weak<SceneView>>> {
    static V: OnceLock<Mutex<HashMap<usize, Weak<SceneView>>>> = OnceLock::new();
    V.get_or_init(Default::default)
}

pub fn find_scene(name: &str) -> Option<Source> {
    let n = c(name);
    unsafe { Source::from_owned(sys::obs_get_source_by_name(n.as_ptr())) }
}

/// Gets (or creates) the encoder's view with the given scene.
pub fn acquire(encoder: &Encoder, scene_name: &str) -> Result<Arc<SceneView>, String> {
    let scene = find_scene(scene_name).ok_or_else(|| format!("Output scene is not found: {scene_name}"))?;
    let mut map = views().lock().unwrap();
    map.retain(|_, w| w.strong_count() > 0);
    let key = encoder.as_ptr() as usize;
    if let Some(view) = map.get(&key).and_then(Weak::upgrade) {
        view.set_source(scene);
        return Ok(view);
    }
    if encoder.active() {
        return Err("encoder is already active without a scene view".into());
    }
    unsafe {
        let view = sys::obs_view_create();
        sys::obs_view_set_source(view, 0, scene.as_ptr());
        sys::obs_source_inc_active(scene.as_ptr());
        let video = sys::obs_view_add(view);
        let built = Arc::new(SceneView { view, source: Mutex::new(scene), _encoder: encoder.clone() });
        if video.is_null() {
            return Err("could not create the scene view video output".into());
        }
        sys::obs_encoder_set_video(encoder.as_ptr(), video);
        if sys::obs_encoder_parent_video(encoder.as_ptr()) != video {
            return Err("encoder did not accept the scene view video".into());
        }
        map.insert(key, Arc::downgrade(&built));
        Ok(built)
    }
}
