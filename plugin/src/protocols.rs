//! Protocol table (same as protocols.cpp in the C++ plugin).
use serde_json::json;

pub struct Protocol {
    pub id: &'static str,
    pub label: &'static str,
    pub output_id: &'static str,
    /// None: no obs_service.
    pub service_id: Option<&'static str>,
}

pub const PROTOCOLS: &[Protocol] = &[
    Protocol { id: "RTMP", label: "RTMP", output_id: "rtmp_output", service_id: Some("rtmp_custom") },
    Protocol { id: "SRT_RIST", label: "SRT/RIST", output_id: "ffmpeg_mpegts_muxer", service_id: Some("rtmp_custom") },
    Protocol {
        id: "RTMP_RUST",
        label: "RTMP/RTMPS (Rust session engine)",
        output_id: "multi_rtmp_rust_output",
        service_id: Some("multi_rtmp_rust_service"),
    },
    Protocol { id: "WHIP", label: "WebRTC (WHIP)", output_id: "whip_output", service_id: Some("whip_custom") },
];

pub fn find(id: &str) -> Option<&'static Protocol> {
    PROTOCOLS.iter().find(|p| p.id == id)
}

/// If the protocol does not exist, the first entry (RTMP) is used, like the C++ plugin.
pub fn find_or_default(id: &str) -> &'static Protocol {
    find(id).unwrap_or(&PROTOCOLS[0])
}

/// For the Qt editor: [{protocol,label,output_id,service_id|null}].
#[no_mangle]
pub extern "C" fn omr_protocols_json() -> *mut std::ffi::c_char {
    let list: Vec<_> = PROTOCOLS
        .iter()
        .map(|p| json!({"protocol": p.id, "label": p.label, "output_id": p.output_id, "service_id": p.service_id}))
        .collect();
    crate::ui::into_c_string(serde_json::to_string(&list).unwrap())
}
