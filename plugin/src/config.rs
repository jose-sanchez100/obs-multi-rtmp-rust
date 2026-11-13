//! Data model and format of `obs-multi-rtmp.json` (identical to the C++ plugin).
//!
//! - Tolerant reading: strict types (a field with a different type counts as
//!   missing), non-object entries are skipped, invalid JSON = empty config.
//! - Writing: compact JSON with keys in alphabetical order (like nlohmann),
//!   only the encoder configs referenced by some target.
use serde_json::{json, Map, Value};

pub const OBS_STREAMING_ENCODER: &str = "<OBS_STREAMING_ENCODER>";
pub const OBS_RECORDING_ENCODER: &str = "<OBS_RECORDING_ENCODER>";

pub fn is_special_encoder(id: &str) -> bool {
    id == OBS_STREAMING_ENCODER || id == OBS_RECORDING_ENCODER
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoConfig {
    pub id: String,
    pub encoder: String,
    pub param: Value,
    pub scene: Option<String>,
    pub resolution: Option<String>,
    pub fps_divisor: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioTrack {
    pub mixer_track: i64,
    pub output_track: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioConfig {
    pub id: String,
    pub encoder: String,
    pub param: Value,
    pub mixer_id: i64,
    pub tracks: Vec<AudioTrack>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub service_param: Value,
    pub output_param: Value,
    pub sync_start: bool,
    pub sync_stop: bool,
    pub enabled: bool,
    pub video_config: Option<String>,
    pub audio_config: Option<String>,
}

impl Target {
    pub fn new(id: String) -> Self {
        Self {
            id,
            name: String::new(),
            protocol: "RTMP".into(),
            service_param: Value::Null,
            output_param: Value::Null,
            sync_start: false,
            sync_stop: false,
            enabled: true,
            video_config: None,
            audio_config: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    pub targets: Vec<Target>,
    pub video: Vec<VideoConfig>,
    pub audio: Vec<AudioConfig>,
}

impl Config {
    pub fn target(&self, id: &str) -> Option<&Target> {
        self.targets.iter().find(|t| t.id == id)
    }
    pub fn target_mut(&mut self, id: &str) -> Option<&mut Target> {
        self.targets.iter_mut().find(|t| t.id == id)
    }
    pub fn video_config(&self, id: &str) -> Option<&VideoConfig> {
        self.video.iter().find(|v| v.id == id)
    }
    pub fn audio_config(&self, id: &str) -> Option<&AudioConfig> {
        self.audio.iter().find(|a| a.id == id)
    }

    /// Random 32-bit decimal id, unique across targets and configs.
    pub fn generate_id(&self) -> String {
        use std::hash::{BuildHasher, Hasher};
        loop {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
            let id = (h.finish() as u32).to_string();
            let used = self.targets.iter().any(|t| t.id == id)
                || self.video.iter().any(|v| v.id == id)
                || self.audio.iter().any(|a| a.id == id);
            if !used {
                return id;
            }
        }
    }
}

// ---------------------------------------------------------------- reading

fn get_str(o: &Map<String, Value>, k: &str) -> Option<String> {
    o.get(k).and_then(Value::as_str).map(str::to_owned)
}
fn get_bool(o: &Map<String, Value>, k: &str) -> Option<bool> {
    o.get(k).and_then(Value::as_bool)
}
/// Like nlohmann's `is_number_integer()`: `2.0` does not count as an integer.
fn get_int(o: &Map<String, Value>, k: &str) -> Option<i64> {
    o.get(k).and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
}
/// Objects only; anything else (including missing) becomes null.
fn get_obj(o: &Map<String, Value>, k: &str) -> Value {
    match o.get(k) {
        Some(v @ Value::Object(_)) => v.clone(),
        _ => Value::Null,
    }
}
fn objects<'a>(root: &'a Map<String, Value>, k: &str) -> impl Iterator<Item = &'a Map<String, Value>> {
    root.get(k).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object)
}

fn parse_target(o: &Map<String, Value>) -> Option<Target> {
    let sync_start = get_bool(o, "sync-start").unwrap_or(false);
    Some(Target {
        id: get_str(o, "id")?,
        name: get_str(o, "name").unwrap_or_default(),
        protocol: get_str(o, "protocol").unwrap_or_else(|| "RTMP".into()),
        service_param: get_obj(o, "service-param"),
        output_param: get_obj(o, "output-param"),
        sync_start,
        // migration: there used to be a single sync checkbox
        sync_stop: get_bool(o, "sync-stop").unwrap_or(sync_start),
        enabled: get_bool(o, "enabled").unwrap_or(true),
        video_config: get_str(o, "video-config"),
        audio_config: get_str(o, "audio-config"),
    })
}

fn parse_video(o: &Map<String, Value>) -> Option<VideoConfig> {
    Some(VideoConfig {
        id: get_str(o, "id")?,
        encoder: get_str(o, "encoder").unwrap_or_default(),
        param: get_obj(o, "param"),
        scene: get_str(o, "scene"),
        resolution: get_str(o, "resolution"),
        fps_divisor: get_int(o, "fps-denumerator").unwrap_or(1),
    })
}

fn parse_audio(o: &Map<String, Value>) -> Option<AudioConfig> {
    let tracks = objects(o, "audioTracks")
        .map(|t| AudioTrack {
            mixer_track: get_int(t, "mixer_track").unwrap_or(0),
            output_track: get_int(t, "output_track").unwrap_or(0),
        })
        .collect();
    Some(AudioConfig {
        id: get_str(o, "id")?,
        encoder: get_str(o, "encoder").unwrap_or_default(),
        param: get_obj(o, "param"),
        mixer_id: get_int(o, "mixerId").unwrap_or(0),
        tracks,
    })
}

/// `None` only if the text is not valid JSON. Accepts a BOM.
pub fn parse(text: &str) -> Option<Config> {
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let Some(root) = v.as_object() else { return Some(Config::default()) };
    Some(Config {
        targets: objects(root, "targets").filter_map(parse_target).collect(),
        video: objects(root, "video_configs").filter_map(parse_video).collect(),
        audio: objects(root, "audio_configs").filter_map(parse_audio).collect(),
    })
}

// ---------------------------------------------------------------- writing

fn target_json(t: &Target) -> Value {
    let mut o = json!({
        "id": t.id,
        "name": t.name,
        "protocol": t.protocol,
        "service-param": t.service_param,
        "output-param": t.output_param,
        "sync-start": t.sync_start,
        "sync-stop": t.sync_stop,
        "enabled": t.enabled,
    });
    if let Some(v) = &t.video_config {
        o["video-config"] = json!(v);
    }
    if let Some(a) = &t.audio_config {
        o["audio-config"] = json!(a);
    }
    o
}

fn video_json(v: &VideoConfig) -> Value {
    let mut o = json!({"id": v.id, "encoder": v.encoder, "param": v.param, "fps-denumerator": v.fps_divisor});
    if let Some(s) = &v.scene {
        o["scene"] = json!(s);
    }
    if let Some(r) = &v.resolution {
        o["resolution"] = json!(r);
    }
    o
}

fn audio_json(a: &AudioConfig) -> Value {
    let tracks: Vec<Value> =
        a.tracks.iter().map(|t| json!({"mixer_track": t.mixer_track, "output_track": t.output_track})).collect();
    json!({"id": a.id, "encoder": a.encoder, "param": a.param, "mixerId": a.mixer_id, "audioTracks": tracks})
}

/// Compact JSON with sorted keys; only configs referenced by some target.
pub fn to_json(c: &Config) -> String {
    let used = |id: &str| c.targets.iter().any(|t| t.video_config.as_deref() == Some(id) || t.audio_config.as_deref() == Some(id));
    let root = json!({
        "targets": c.targets.iter().map(target_json).collect::<Vec<_>>(),
        "video_configs": c.video.iter().filter(|v| used(&v.id)).map(video_json).collect::<Vec<_>>(),
        "audio_configs": c.audio.iter().filter(|a| used(&a.id)).map(audio_json).collect::<Vec<_>>(),
    });
    serde_json::to_string(&root).unwrap()
}

/// Engine options that were shared by Live and Backup in the first version and
/// are now per stream (old key, new key without the live_/backup_ prefix).
const SHARED_ENGINE_OPTIONS: [(&str, &str); 4] =
    [("buffer_mb", "buffer_mb"), ("max_latency_ms", "drop_threshold_ms"), ("retry_max_s", "retry_max_s"), ("max_retries", "max_retries")];

/// Version 0.2.0: the Rust engine destinations (Live and Backup server and key)
/// used to live in the output settings; they now live in the service settings.
/// Moves them without overwriting anything already in the service. Also splits
/// the shared engine options into live_* and backup_*. Returns true if anything
/// changed.
pub fn migrate_rust_destinations(c: &mut Config) -> bool {
    let mut changed = false;
    for t in c.targets.iter_mut().filter(|t| t.protocol == "RTMP_RUST") {
        let Some(out) = t.output_param.as_object_mut() else { continue };
        for (old, new) in SHARED_ENGINE_OPTIONS {
            if let Some(v) = out.remove(old) {
                for prefix in ["live_", "backup_"] {
                    out.entry(format!("{prefix}{new}")).or_insert_with(|| v.clone());
                }
                changed = true;
            }
        }
        let moved: Vec<(String, Value)> = crate::rust_service::KEYS
            .iter()
            .filter_map(|k| out.remove(*k).map(|v| (k.to_string(), v)))
            .collect();
        if moved.is_empty() {
            continue;
        }
        if !t.service_param.is_object() {
            t.service_param = Value::Object(Map::new());
        }
        let svc = t.service_param.as_object_mut().unwrap();
        for (k, v) in moved {
            let empty = svc.get(&k).and_then(Value::as_str).is_none_or(str::is_empty);
            if empty {
                svc.insert(k, v);
            }
        }
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_destinations_move_to_service() {
        let mut c = parse(
            r#"{"targets":[
                {"id":"a","protocol":"RTMP_RUST","service-param":null,
                 "output-param":{"server":"rtmps://x/live2","key":"k","backup_server":"rtmps://b/live2?backup=1","backup_key":"","buffer_mb":4}},
                {"id":"b","protocol":"RTMP","service-param":{"server":"s"},"output-param":{"server":"keep"}}
            ]}"#,
        )
        .unwrap();
        assert!(migrate_rust_destinations(&mut c));
        let a = &c.targets[0];
        assert_eq!(a.service_param["server"], "rtmps://x/live2");
        assert_eq!(a.service_param["key"], "k");
        assert_eq!(a.service_param["backup_server"], "rtmps://b/live2?backup=1");
        assert_eq!(a.output_param, json!({"live_buffer_mb": 4, "backup_buffer_mb": 4}));
        // other protocols are left untouched
        assert_eq!(c.targets[1].output_param["server"], "keep");
        // idempotent
        assert!(!migrate_rust_destinations(&mut c));
    }

    const SAMPLE: &str = r#"{"audio_configs":[{"audioTracks":[{"mixer_track":1,"output_track":1}],"encoder":"ffmpeg_aac","id":"3021","mixerId":0,"param":{"bitrate":160}},{"encoder":"x","id":"999","param":null}],"targets":[{"audio-config":"3021","enabled":true,"id":"1187","name":"YouTube","output-param":{"bind_ip":"default","drop_threshold_ms":700},"protocol":"RTMP","service-param":{"key":"xxxx","server":"rtmp://a.rtmp.youtube.com/live2","use_auth":false},"sync-start":true,"sync-stop":true,"video-config":"<OBS_STREAMING_ENCODER>"}],"video_configs":[]}"#;

    #[test]
    fn roundtrip_is_byte_identical_and_drops_unreferenced_configs() {
        let c = parse(SAMPLE).unwrap();
        assert_eq!(c.targets.len(), 1);
        assert_eq!(c.audio.len(), 2);
        let out = to_json(&c);
        // config "999" is unused: it is dropped, everything else stays the same
        let expected = SAMPLE.replace(r#",{"encoder":"x","id":"999","param":null}"#, "");
        assert_eq!(out, expected);
    }

    #[test]
    fn legacy_and_tolerant_parsing() {
        let c = parse(
            "\u{feff}{\"targets\":[{\"id\":\"1\",\"sync-start\":true},{\"name\":\"no id\"},5,\
             {\"id\":\"2\",\"enabled\":\"yes\",\"service-param\":[1],\"sync-stop\":false}],\
             \"video_configs\":[{\"id\":\"v\",\"fps-denumerator\":2.0}]}",
        )
        .unwrap();
        assert_eq!(c.targets.len(), 2);
        let t = &c.targets[0];
        assert_eq!((t.protocol.as_str(), t.sync_start, t.sync_stop, t.enabled), ("RTMP", true, true, true));
        assert_eq!(t.service_param, Value::Null);
        let t2 = &c.targets[1];
        assert!(t2.enabled, "wrong type counts as missing");
        assert_eq!(t2.service_param, Value::Null);
        assert!(!t2.sync_stop);
        assert_eq!(c.video[0].fps_divisor, 1, "2.0 is not an integer for nlohmann");
        assert!(parse("{no json").is_none());
        assert_eq!(parse("[1,2]").unwrap(), Config::default());
    }

    #[test]
    fn generated_ids_are_unique() {
        let mut c = Config::default();
        for _ in 0..200 {
            let id = c.generate_id();
            assert!(id.parse::<u32>().is_ok());
            c.targets.push(Target::new(id));
        }
    }
}

#[cfg(test)]
mod real_file {
    /// OMR_REAL_CONFIG=<path to obs-multi-rtmp.json>: compares without printing the contents (they contain keys).
    #[test]
    #[ignore]
    fn real_profile_file_roundtrips() {
        let Ok(path) = std::env::var("OMR_REAL_CONFIG") else { return };
        let raw = std::fs::read_to_string(path).unwrap();
        let original = raw.trim_start_matches('\u{feff}');
        let c = super::parse(original).expect("valid JSON");
        let out = super::to_json(&c);
        println!("targets={} video={} audio={} identical={}", c.targets.len(), c.video.len(), c.audio.len(), out == original);
        if out != original {
            let pos = out.bytes().zip(original.bytes()).position(|(a, b)| a != b).unwrap_or(out.len().min(original.len()));
            println!("first difference at byte {pos} (len out={} orig={})", out.len(), original.len());
        }
        // Migration (in memory only; prints field names, never values).
        let mut m = c.clone();
        let changed = super::migrate_rust_destinations(&mut m);
        for t in m.targets.iter().filter(|t| t.protocol == "RTMP_RUST") {
            let keys = |v: &serde_json::Value| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
            println!("migrated={changed} service={:?} output={:?}", keys(&t.service_param), keys(&t.output_param));
        }
    }
}
