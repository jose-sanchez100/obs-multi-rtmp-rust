// Checks at compile time that the libobs structs Rust declares by hand
// (plugin/src/obs/sys.rs) have the same layout as the real OBS headers.
// If an OBS version changes anything, the build fails.
// The same numbers are verified in Rust with const asserts.
#include <cstddef>
#include <obs-module.h>

#define CHECK_SIZE(t, n) static_assert(sizeof(t) == (n), "layout of " #t " differs: check sys.rs")
#define CHECK_OFF(t, f, n) static_assert(offsetof(t, f) == (n), "offset of " #t "." #f " differs: check sys.rs")

CHECK_SIZE(obs_output_info, 192);
CHECK_OFF(obs_output_info, flags, 8);
CHECK_OFF(obs_output_info, encoded_packet, 72);
CHECK_OFF(obs_output_info, get_total_bytes, 112);
CHECK_OFF(obs_output_info, get_dropped_frames, 120);
CHECK_OFF(obs_output_info, encoded_video_codecs, 160);
CHECK_OFF(obs_output_info, protocols, 184);

CHECK_SIZE(obs_service_info, 216);
CHECK_OFF(obs_service_info, get_output_type, 144);
CHECK_OFF(obs_service_info, get_protocol, 184);
CHECK_OFF(obs_service_info, get_connect_info, 200);
CHECK_OFF(obs_service_info, can_try_to_connect, 208);

CHECK_SIZE(encoder_packet, 88);
CHECK_OFF(encoder_packet, size, 8);
CHECK_OFF(encoder_packet, pts, 16);
CHECK_OFF(encoder_packet, timebase_num, 32);
CHECK_OFF(encoder_packet, type, 40);
CHECK_OFF(encoder_packet, keyframe, 44);
CHECK_OFF(encoder_packet, dts_usec, 48);
CHECK_OFF(encoder_packet, track_idx, 72);
CHECK_OFF(encoder_packet, encoder, 80);

static_assert(OBS_OUTPUT_AV == 3 && OBS_OUTPUT_ENCODED == 4 && OBS_OUTPUT_SERVICE == 8 && OBS_OUTPUT_MULTI_TRACK == 16,
              "obs_output flags differ");
static_assert(LIBOBS_API_MAJOR_VER == 33, "this plugin is built against the OBS 33 headers");

#include <obs-frontend-api.h>
// Event values used by src/events.rs
static_assert(OBS_FRONTEND_EVENT_STREAMING_STARTING == 0 && OBS_FRONTEND_EVENT_STREAMING_STOPPING == 2, "events");
static_assert(OBS_FRONTEND_EVENT_PROFILE_CHANGED == 15 && OBS_FRONTEND_EVENT_PROFILE_LIST_CHANGED == 16, "events");
static_assert(OBS_FRONTEND_EVENT_EXIT == 17 && OBS_FRONTEND_EVENT_FINISHED_LOADING == 26, "events");
static_assert(OBS_SCALE_BICUBIC == 2, "obs_scale_type");
static_assert(OBS_TEXT_DEFAULT == 0 && OBS_TEXT_PASSWORD == 1 && OBS_TEXT_INFO == 3, "obs_text_type");
static_assert(OBS_COMBO_TYPE_LIST == 2 && OBS_COMBO_FORMAT_STRING == 3, "obs_combo_type/format");
