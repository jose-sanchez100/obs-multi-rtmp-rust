# Evaluation: encoding in the plugin instead of OBS

**Question:** can the plugin take over video/audio encoding from OBS, so that a stall in
OBS no longer turns into a broken stream (frozen, corrupted, or no data at the ingest)?

**Short answer:** partly. OBS cannot be removed from the pipeline, because the image itself
(scenes, sources, filters, transitions) is composited by OBS's graphics thread, and no plugin
can produce new frames while that thread is stuck. What the plugin *can* own is everything
after compositing: frame hand-off, encoding, timestamps and sending. That removes the stalls
that come from encoders, and lets the plugin keep a valid stream (last frame or a slate, plus
silence) going while OBS is stalled, instead of sending nothing or garbage.

References are to OBS 33.0.1 (`libobs/`).

## How OBS produces a stream today

```
graphics thread ──render──▶ main texture ──┬─▶ GPU encode thread ──▶ NVENC/AMF/QSV (texture encoders)
 (obs-video.c)                              │   (obs-video-gpu-encode.c)
                                            └─▶ GPU→CPU copy ──▶ video-io thread ──▶ x264 & other raw encoders
                                                                 (media-io/video-io.c)
encoder packets ──▶ outputs (rtmp_output / this plugin's Rust session engine) ──▶ network
```

Findings from the source:

1. **One video-io thread drives every raw encoder in turn.** `video_output_cur_frame()`
   (`media-io/video-io.c:126`) loops over all connected inputs and calls each callback
   synchronously; for an encoder that callback is `receive_video()` → `do_encode()`
   (`obs-encoder.c:1586`, `:1456`). A slow encoder holds the thread, so *every* raw encoder on
   that video output skips frames ("skipped frames due to encoding lag").
2. **One GPU encode thread drives every texture encoder in turn, and delivers their packets.**
   `gpu_encode_thread()` (`obs-video-gpu-encode.c`) walks all texture encoders for each frame
   and, for each packet, calls every connected output synchronously (`send_packet()` →
   `cb->new_packet()`, `obs-encoder.c:1360`). A hung NVENC call, or an output that blocks while
   receiving a packet, stalls *all* NVENC encoders and therefore every stream. The video-io
   thread has the same property for raw encoders.
3. **Render lag duplicates frames; a render hang produces none.** When the graphics thread is
   late, `obs-video.c:814-828` outputs the last frame `count` times (`lagged_frames`), so the
   stream stays valid but freezes. If the graphics thread stops entirely (GPU hang, driver
   timeout, a source deadlocking it), no frames reach any encoder: the ingest gets audio only,
   or nothing, and YouTube reports the stream as unhealthy or ends it.
4. **Network congestion is already isolated.** The Rust session engine gives each destination
   its own thread, bounded queue and frame dropping, so a slow destination no longer stalls OBS
   or the other destinations.

So of the three places a stall can start, the network is handled, encoders can be moved into
the plugin, and compositing cannot.

## Case study: a real incident

An OBS 33 log from a 1h20 broadcast with unstable bitrate and frame rate, using the original
C++ obs-multi-rtmp 0.8.0 (i7-8700, GTX 1070, 1080p60). The plugin's three targets connected
directly over the internet with RTMPS (YouTube primary, YouTube backup, Facebook), each with
its own NVENC encoder (10 + 10 + 7 Mbps, about 28 Mbps of upload in total); only OBS's main
stream (x264, 1 Mbps) went to a local nginx on `127.0.0.1`:

| Log line / profiler entry | Value | Meaning |
|---|---|---|
| `obs_graphics_thread` | 99.99% of frames under 16.7 ms; 2 lagged frames | Compositing was healthy |
| `gpu_encode_frame` | median 0.69 ms, max 23.7 ms | NVENC itself kept up |
| `obs_gpu_encode_thread` → `send_packet` | **max 5999 ms**, 99th percentile 31.7 ms | Handing packets to the RTMP outputs blocked the shared encode thread, up to 6 s at a time |
| Skipped frames due to encoding lag | 47,577 / 541,012 (8.8%) | Consequence: every NVENC encoder starved together |
| Dropped frames due to insufficient bandwidth | 16.4%, 65.8%, 34.1% (successive restarts) | The uplink could not carry all three streams |
| One output at stop | 82,254 of 240,564 frames sent in 67 min; **138,092 packets still queued** | That destination averaged ~20 fps and its send queue grew without bound |

Why a slow destination stalls the encoders: OBS's RTMP output (`plugins/obs-outputs/rtmp-stream.c`)
receives each packet on the encoder's thread (`rtmp_stream_data()`), and while holding
`packets_mutex` calls `check_to_drop_frames()`. When the queue is over the drop threshold,
`drop_frames()` pops and re-pushes **every** queued packet to remove non-keyframes; audio and
keyframes are never dropped, so on a link that cannot keep up the queue keeps growing, and each
new packet walks a longer queue. With tens of thousands of packets queued this takes tens of
milliseconds per packet, and seconds when the output is stopped and frees the whole queue,
all of it on the single GPU encode thread (finding 2).

This matches what was seen on site: one of the uplinks (a cable modem) started dropping data
at about 06:07 and got worse at about 06:20, after which every destination degraded.

So the broken stream was **not** OBS's compositing or the encoders themselves: one congested
RTMP output made the shared GPU encode thread wait, and every destination froze or skipped
frames together. Moving encoding into the plugin would not
have been needed to prevent it. What prevents it is an output whose packet callback never
blocks, which is what the Rust session engine already is: `encoded_packet()` in
`plugin/src/rust_output.rs` only copies the packet into bounded per-session queues, with no
network I/O and no thread joins under its lock. Each session's queue is capped (6 MB and 4 s
of latency by default, `src/queue.rs`), so it cannot grow like OBS's did, and the congestion
tests check that the producer stays real-time while a destination stalls.

The same setup also encoded the YouTube primary and backup separately (two 10 Mbps NVENC
sessions with fullres multipass), which doubles NVENC load and makes the two streams
non-identical; a single target with Live and Backup ingests in the Rust engine uses one
encoder and sends identical packets. The x264 main stream can be replaced by the null-output
option in the Advanced menu.

One caveat remains: all texture encoders still share OBS's GPU encode thread, so a target that
uses OBS's native RTMP output (or any other blocking output) can still stall the encoders used
by Rust-engine targets. Every destination should therefore use the Rust engine.

## Options

### A. Plugin-owned encoders, in process

Tap the composited frames with `obs_add_raw_video_callback2()` / `obs_add_raw_audio_callback()`
(or a scaled `video_t` per target, as the plugin already does for per-target scenes). The
callback only copies the frame into a small ring buffer owned by the plugin and returns at
once; a plugin thread per target encodes it and feeds the session engine.

- Encoding: FFmpeg's `libavcodec`, which OBS already ships (`avcodec-62.dll`, `avutil-60.dll`,
  `swscale-9.dll` in `bin/64bit`), giving `h264_nvenc`, `hevc_nvenc`, `h264_amf`, `h264_qsv`
  and `libx264` behind one API. Linking against OBS's own copies avoids bundling FFmpeg.
- A slow or failing encoder only affects its own target; the video-io thread never waits.
- **Watchdog:** if no frame arrives for, say, 500 ms, the encoder thread repeats the last frame
  (or a "technical difficulties" slate) at the target frame rate with silent audio, keeping
  timestamps monotonic and keyframes on schedule. When frames come back it switches back.
- Limits: the raw tap forces a GPU→CPU copy (~3 MB per 1080p NV12 frame, ~190 MB/s at 60 fps)
  and, for NVENC, an upload back to the GPU. OBS's texture path avoids both, so this costs some
  extra GPU/PCIe load compared with `obs_nvenc_h264_tex`. It does not survive OBS crashing or
  hanging its whole process, because the encoder threads live inside OBS.

### B. Out-of-process encoder (helper process)

Same frame tap, but the plugin writes frames and audio into shared memory, and a separate Rust
helper process (`obs-multi-rtmp-encoder`) encodes and runs the session engine.

- Everything in A, plus: if OBS hangs or crashes, the helper keeps the RTMP sessions open and
  sends the slate until OBS comes back (or until a timeout), so YouTube does not end the
  broadcast and the primary/backup pair stays in sync.
- Costs: process supervision (start, restart, version check), a shared-memory protocol (frame
  ring + audio ring + control channel), and more moving parts to install and support on three
  platforms. Same copy cost as A.

### C. Keep OBS encoders, add only the watchdog

Leave encoding to OBS, and have the session engine fill gaps itself: if no video packet arrives
for a while, resend the last keyframe/GOP with rewritten timestamps.

- Smallest change, but re-sending compressed frames is fragile (B-frames, references, decoder
  confusion on the ingest side) and it does nothing for a hung encoder thread. Not recommended
  except as a stopgap.

## Recommendation

**Step 0, no new code:** move every destination to the RTMP/RTMPS (Rust session engine)
protocol, use one target with Live + Backup for YouTube, and turn on the null main output. This
addresses the failure in the case study.

Plugin-owned encoding is only worth building if stalls keep happening after that and the log
shows a different cause (encoder errors, device-lost, rendering lag). In that case, build it in
two steps behind a per-target option ("Encode in plugin"), keeping the current OBS encoders as
the default:

1. **Option A with the watchdog.** It removes encoder-induced stalls, isolates targets from
   each other, and keeps a valid stream during render stalls. Most of the work (frame ring,
   FFmpeg encoder wrapper, watchdog, timestamp handling) carries over to step 2 unchanged.
2. **Option B** only if the problem observed in practice is OBS hanging or crashing as a whole,
   which A cannot cover.

Before step 1, it is worth identifying which stall actually happened, from the OBS log of the
affected session: "skipped frames due to encoding lag" points at encoders (A fixes it),
"frames missed due to rendering lag" or a `DXGI_ERROR_DEVICE_REMOVED`/device-lost message
points at compositing or the GPU driver (A keeps the stream valid with a slate, B also survives
a full hang), and "dropped frames due to insufficient bandwidth" is the network (already
handled by the session engine).

## Rough scope of step 1

| Piece | Notes |
|---|---|
| Frame tap and ring buffer | Raw video/audio callbacks per target, copy-and-return, fixed capacity, drop-oldest |
| FFmpeg FFI | `avcodec` encoder open/send/receive, hardware encoder selection, settings mapped from the target's encoder config |
| Encoder thread | Pulls frames, encodes, timestamps, hands FLV packets to the session engine |
| Watchdog | Stall detection, last-frame/slate repeat, silent audio, recovery |
| UI | "Encode in plugin" checkbox and encoder choice per target; stall indicator in the dock |
| Tests | Engine-level tests with synthetic frames, including a simulated stall |
