#ifndef OBS_RTMP_SESSIONS_H
#define OBS_RTMP_SESSIONS_H

#pragma once

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdint.h>
#include <stddef.h>

typedef struct SessionManager SessionManager;

/**
 * H.264 and AAC encoder metadata. Optional fields may be zero.
 */
typedef struct OmrStreamInfo {
  uint32_t width;
  uint32_t height;
  float frame_rate;
  uint32_t video_bitrate_kbps;
  uint32_t audio_bitrate_kbps;
  uint32_t audio_sample_rate;
  uint32_t audio_channels;
} OmrStreamInfo;

typedef struct OmrStats {
  uint8_t state;
  uint64_t packets_sent;
  uint64_t bytes_sent;
  uint64_t reconnects;
  uint64_t slow_sends;
  uint64_t queued_bytes;
  uint64_t dropped_video_frames;
  uint64_t dropped_audio_frames;
  uint64_t dropped_gops;
} OmrStats;

struct SessionManager *omr_manager_new(void);

/**
 * # Safety
 * `m` must come from `omr_manager_new` and must not be used afterwards.
 */
void omr_manager_free(struct SessionManager *m);

/**
 * Returns the session id (>0), or 0 if the URL is invalid.
 *
 * # Safety
 * `url` and `key` must be valid C strings (`key` may be NULL).
 */
uint64_t omr_session_add(struct SessionManager *m,
                         const char *url,
                         const char *key,
                         uintptr_t soft_bytes,
                         uintptr_t hard_bytes);

/**
 * Same as `omr_session_add`, with metadata announced on every connection.
 *
 * # Safety
 * `m`, `url` and `key` follow the contract of `omr_session_add`.
 * `info` may be NULL or point to an `OmrStreamInfo` readable for the duration of the call.
 */
uint64_t omr_session_add_with_info(struct SessionManager *m,
                                   const char *url,
                                   const char *key,
                                   uintptr_t soft_bytes,
                                   uintptr_t hard_bytes,
                                   const struct OmrStreamInfo *info);

int omr_session_remove(struct SessionManager *m, uint64_t id);

/**
 * Stops sending on a session without destroying it (state 5). Non-blocking.
 */
int omr_session_pause(struct SessionManager *m, uint64_t id);

/**
 * Resumes a paused session: reconnects and rejoins at the next keyframe. Non-blocking.
 */
int omr_session_resume(struct SessionManager *m, uint64_t id);

/**
 * Forces an immediate reconnect of a session (even if it was paused). Non-blocking.
 */
int omr_session_restart(struct SessionManager *m, uint64_t id);

/**
 * kind: 0 = video, 1 = audio. Copies the data once and shares it across sessions.
 *
 * # Safety
 * `data` must point to `len` readable bytes.
 */
void omr_push(struct SessionManager *m,
              int kind,
              uint32_t dts_ms,
              int keyframe,
              int is_header,
              const uint8_t *data,
              uintptr_t len);

int omr_session_stats(struct SessionManager *m, uint64_t id, struct OmrStats *out);

/**
 * SPS+PPS in Annex-B, or an AVCDecoderConfigurationRecord with 4-byte lengths.
 * # Safety
 * `data` points to `len` readable bytes.
 */
int omr_set_video_extradata(struct SessionManager *m, const uint8_t *data, uintptr_t len);

/**
 * AudioSpecificConfig (`obs_encoder_get_extra_data` of the AAC encoder).
 * # Safety
 * `data` points to `len` readable bytes.
 */
int omr_set_audio_extradata(struct SessionManager *m, const uint8_t *data, uintptr_t len);

/**
 * # Safety
 * `data` points to `len` readable bytes (Annex-B packet).
 */
void omr_push_video(struct SessionManager *m,
                    int64_t dts_ms,
                    int64_t pts_ms,
                    const uint8_t *data,
                    uintptr_t len);

/**
 * # Safety
 * `data` points to `len` readable bytes (raw AAC).
 */
void omr_push_audio(struct SessionManager *m, int64_t dts_ms, const uint8_t *data, uintptr_t len);

/**
 * Forgets the sequence headers when streaming ends.
 */
void omr_clear_headers(struct SessionManager *m);

/**
 * Copies the session's last error (UTF-8, NUL-terminated) into `buf`.
 * Returns the length excluding the NUL, or 0 if there is no error or the session does not exist.
 *
 * # Safety
 * `buf` points to `cap` writable bytes.
 */
uintptr_t omr_session_last_error(struct SessionManager *m, uint64_t id, char *buf, uintptr_t cap);

#endif  /* OBS_RTMP_SESSIONS_H */
