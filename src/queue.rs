//! Bounded per-session queue with a GOP-aware drop policy.
//!
//! The producer (OBS encoding thread) NEVER blocks: `push` holds a mutex
//! for a minimal O(n) and returns. If the destination is congested, the
//! queue drops in this order:
//!   1. P/B-frames up to the next keyframe (video "freezes" but stays
//!      decodable as soon as the IDR arrives).
//!   2. If the hard limit is still exceeded, the oldest complete GOP.
//! Audio is only dropped when the hard byte limit is exceeded, because an
//! audio gap is more annoying than a video skip.

use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
}

#[derive(Clone, Debug)]
pub struct MediaPacket {
    pub kind: Kind,
    pub dts_ms: u32,
    pub keyframe: bool,
    /// Sequence header (SPS/PPS, AudioSpecificConfig): never dropped,
    /// and resent on reconnect.
    pub is_header: bool,
    /// Prebuilt FLV tag body (ref-counted: shared across sessions without copying).
    pub data: Bytes,
}

#[derive(Clone, Copy, Debug)]
pub struct QueueLimits {
    /// Above this, dropping of non-key frames kicks in.
    pub soft_bytes: usize,
    /// Above this, the oldest GOP is dropped (and audio if necessary).
    pub hard_bytes: usize,
    /// Maximum latency allowed in the queue (ms of media). If exceeded, skip to the
    /// oldest keyframe that fits in the window. 0 = no time limit.
    pub max_latency_ms: u32,
}

impl Default for QueueLimits {
    fn default() -> Self {
        Self { soft_bytes: 2 * 1024 * 1024, hard_bytes: 6 * 1024 * 1024, max_latency_ms: 4000 }
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct DropStats {
    pub dropped_video_frames: u64,
    pub dropped_audio_frames: u64,
    pub dropped_gops: u64,
    /// Media bytes offered to the queue while it was accepting media (not paused
    /// and not waiting to resync after a new connection).
    pub offered_bytes: u64,
    /// Media bytes dropped because the destination could not keep up.
    pub dropped_bytes: u64,
}

struct Inner {
    q: VecDeque<MediaPacket>,
    bytes: usize,
    /// Current sequence headers (video, audio), resent on reconnect.
    headers: [Option<MediaPacket>; 2],
    /// While true, P/B-frames are dropped until a keyframe is seen.
    waiting_keyframe: bool,
    /// A new connection (reconnect, resume, late join) must start with the
    /// sequence headers followed by a keyframe. Until that keyframe arrives,
    /// all media is discarded; when it does, the headers are queued with the
    /// keyframe's timestamp. Sending them with their original timestamp (0)
    /// would make the media clock jump from 0 to the current stream time in an
    /// instant, which ingests such as YouTube reject as "faster than real time".
    resync: bool,
    /// dts of the most recently queued packet (excluding headers).
    newest_dts: u32,
    stats: DropStats,
    closed: bool,
    /// Paused by the user: only headers are kept; media is ignored
    /// (not counted as a congestion drop).
    paused: bool,
}

pub struct PacketQueue {
    inner: Mutex<Inner>,
    cv: Condvar,
    limits: QueueLimits,
}

impl PacketQueue {
    pub fn new(limits: QueueLimits) -> Self {
        Self {
            inner: Mutex::new(Inner {
                q: VecDeque::new(),
                bytes: 0,
                headers: [None, None],
                waiting_keyframe: false,
                resync: false,
                newest_dts: 0,
                stats: DropStats::default(),
                closed: false,
                paused: false,
            }),
            cv: Condvar::new(),
            limits,
        }
    }

    /// Never blocks on the network. Returns false if the queue is closed.
    pub fn push(&self, pkt: MediaPacket) -> bool {
        let mut g = self.inner.lock().unwrap();
        if g.closed {
            return false;
        }

        if pkt.is_header {
            let idx = (pkt.kind == Kind::Audio) as usize;
            g.headers[idx] = Some(pkt.clone());
        }
        if g.paused {
            return true;
        }

        // New connection: discard everything until a keyframe, then queue the
        // headers stamped with that keyframe's timestamp, then the keyframe.
        if g.resync {
            if pkt.is_header || pkt.kind != Kind::Video || !pkt.keyframe {
                return true; // headers are already stored above
            }
            g.resync = false;
            g.waiting_keyframe = false;
            let stamped: Vec<_> =
                g.headers.iter().flatten().map(|h| MediaPacket { dts_ms: pkt.dts_ms, ..h.clone() }).collect();
            for h in stamped {
                g.bytes += h.data.len();
                g.q.push_back(h);
            }
        }

        if !pkt.is_header {
            g.stats.offered_bytes += pkt.data.len() as u64;
        }

        // Dropping until the next IDR.
        if g.waiting_keyframe && pkt.kind == Kind::Video && !pkt.is_header {
            if pkt.keyframe {
                g.waiting_keyframe = false;
            } else {
                g.stats.dropped_video_frames += 1;
                g.stats.dropped_bytes += pkt.data.len() as u64;
                return true;
            }
        }

        g.bytes += pkt.data.len();
        if !pkt.is_header {
            g.newest_dts = g.newest_dts.max(pkt.dts_ms);
        }
        g.q.push_back(pkt);

        if self.limits.max_latency_ms > 0 {
            Self::trim_latency(&mut g, self.limits.max_latency_ms);
        }

        if g.bytes > self.limits.soft_bytes {
            Self::shed(&mut g, self.limits);
        }
        drop(g);
        self.cv.notify_one();
        true
    }

    /// If the queue holds more than `max_ms` of media, drops (video and audio alike,
    /// to stay in sync) everything before the oldest keyframe that fits in the window.
    fn trim_latency(g: &mut Inner, max_ms: u32) {
        let Some(front) = g.q.iter().find(|p| !p.is_header).map(|p| p.dts_ms) else { return };
        if g.newest_dts.saturating_sub(front) <= max_ms {
            return;
        }
        let mut in_window = None;
        let mut latest = None;
        for (i, p) in g.q.iter().enumerate() {
            if p.kind == Kind::Video && p.keyframe && !p.is_header {
                if in_window.is_none() && g.newest_dts.saturating_sub(p.dts_ms) <= max_ms {
                    in_window = Some(i);
                }
                latest = Some(i);
            }
        }
        let Some(idx) = in_window.or(latest) else { return };
        if !g.q.iter().take(idx).any(|p| !p.is_header) {
            return; // nothing droppable before the keyframe
        }
        let mut kept = Vec::new();
        let drained: Vec<_> = g.q.drain(..idx).collect();
        for p in drained {
            if p.is_header {
                kept.push(p);
            } else {
                Self::count_drop(g, &p);
            }
        }
        for h in kept.into_iter().rev() {
            g.q.push_front(h);
        }
        g.stats.dropped_gops += 1;
    }

    /// Accounts for a queued media packet removed because of congestion.
    fn count_drop(g: &mut Inner, p: &MediaPacket) {
        g.bytes -= p.data.len();
        g.stats.dropped_bytes += p.data.len() as u64;
        match p.kind {
            Kind::Video => g.stats.dropped_video_frames += 1,
            Kind::Audio => g.stats.dropped_audio_frames += 1,
        }
    }

    fn shed(g: &mut Inner, lim: QueueLimits) {
        // 1) remove already queued P/B-frames (except headers), oldest first,
        //    but keep the last keyframe so playback can resume.
        let mut i = 0;
        while g.bytes > lim.soft_bytes && i < g.q.len() {
            let p = &g.q[i];
            if p.kind == Kind::Video && !p.keyframe && !p.is_header {
                let p = g.q.remove(i).unwrap();
                Self::count_drop(g, &p);
                g.waiting_keyframe = true; // sending P-frames without their reference is useless
            } else {
                i += 1;
            }
        }

        // 2) hard limit: remove complete GOPs, oldest first.
        while g.bytes > lim.hard_bytes {
            // find the second keyframe: everything before it is a droppable GOP
            let mut cut = None;
            let mut seen_first = false;
            for (idx, p) in g.q.iter().enumerate() {
                if p.kind == Kind::Video && p.keyframe && !p.is_header {
                    if seen_first {
                        cut = Some(idx);
                        break;
                    }
                    seen_first = true;
                }
            }
            match cut {
                Some(idx) => {
                    let mut kept_hdr = Vec::new();
                    let drained: Vec<_> = g.q.drain(..idx).collect();
                    for p in drained {
                        if p.is_header {
                            kept_hdr.push(p);
                        } else {
                            Self::count_drop(g, &p);
                        }
                    }
                    for h in kept_hdr.into_iter().rev() {
                        g.q.push_front(h);
                    }
                    g.stats.dropped_gops += 1;
                }
                None => {
                    // A single giant GOP: flush everything except headers and wait for an IDR.
                    let drained: Vec<_> = g.q.drain(..).collect();
                    for p in drained {
                        if p.is_header {
                            g.q.push_back(p);
                        } else {
                            Self::count_drop(g, &p);
                        }
                    }
                    g.stats.dropped_gops += 1;
                    g.waiting_keyframe = true;
                    break;
                }
            }
        }
    }

    /// Waits up to `timeout` for a packet.
    pub fn pop_timeout(&self, timeout: Duration) -> Option<MediaPacket> {
        let mut g = self.inner.lock().unwrap();
        loop {
            if let Some(p) = g.q.pop_front() {
                g.bytes -= p.data.len();
                return Some(p);
            }
            if g.closed {
                return None;
            }
            let (ng, res) = self.cv.wait_timeout(g, timeout).unwrap();
            g = ng;
            if res.timed_out() && g.q.is_empty() {
                return None;
            }
        }
    }

    /// For sessions that join while the stream is running: start at the next
    /// keyframe, preceded by the headers stamped with its timestamp.
    pub fn wait_for_keyframe(&self) {
        let mut g = self.inner.lock().unwrap();
        g.waiting_keyframe = true;
        g.resync = true;
    }

    /// On reconnect: flush and restart at the next keyframe, preceded by the
    /// sequence headers stamped with its timestamp.
    pub fn reset_for_reconnect(&self) {
        let mut g = self.inner.lock().unwrap();
        g.q.clear();
        g.bytes = 0;
        g.waiting_keyframe = true;
        g.resync = true;
    }

    /// Pause: flushes the queue and stops queueing media. On resume, restarts at
    /// the next keyframe (see `reset_for_reconnect`).
    pub fn set_paused(&self, paused: bool) {
        let mut g = self.inner.lock().unwrap();
        g.paused = paused;
        g.q.clear();
        g.bytes = 0;
        g.waiting_keyframe = true;
        g.resync = true;
    }

    pub fn stats(&self) -> (DropStats, usize) {
        let g = self.inner.lock().unwrap();
        (g.stats, g.bytes)
    }

    pub fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(dts: u32, key: bool, n: usize) -> MediaPacket {
        MediaPacket { kind: Kind::Video, dts_ms: dts, keyframe: key, is_header: false, data: Bytes::from(vec![0u8; n]) }
    }
    fn a(dts: u32, n: usize) -> MediaPacket {
        MediaPacket { kind: Kind::Audio, dts_ms: dts, keyframe: false, is_header: false, data: Bytes::from(vec![0u8; n]) }
    }

    #[test]
    fn drops_pframes_then_resumes_at_keyframe() {
        let q = PacketQueue::new(QueueLimits { soft_bytes: 1000, hard_bytes: 10_000, max_latency_ms: 0 });
        q.push(v(0, true, 400));
        for i in 1..10 {
            q.push(v(i * 33, false, 400));
        }
        let (s, bytes) = q.stats();
        assert!(s.dropped_video_frames > 0);
        assert!(bytes <= 1000 + 400);
        // while waiting for a keyframe, subsequent P-frames are dropped
        let before = q.stats().0.dropped_video_frames;
        q.push(v(400, false, 100));
        assert_eq!(q.stats().0.dropped_video_frames, before + 1);
        // a keyframe resumes
        q.push(v(433, true, 100));
        let before = q.stats().0.dropped_video_frames;
        q.push(v(466, false, 100));
        assert_eq!(q.stats().0.dropped_video_frames, before);
    }

    #[test]
    fn headers_survive_and_hard_limit_enforced() {
        let q = PacketQueue::new(QueueLimits { soft_bytes: 500, hard_bytes: 1000, max_latency_ms: 0 });
        q.push(MediaPacket { is_header: true, ..v(0, true, 50) });
        for g in 0..20u32 {
            q.push(v(g * 1000, true, 300));
            q.push(a(g * 1000 + 10, 100));
        }
        let (_, bytes) = q.stats();
        assert!(bytes <= 1000, "bytes={bytes}");
    }

    fn vh() -> MediaPacket {
        MediaPacket { is_header: true, ..v(0, true, 20) }
    }
    fn ah() -> MediaPacket {
        MediaPacket { is_header: true, ..a(0, 4) }
    }
    fn drain(q: &PacketQueue) -> Vec<MediaPacket> {
        std::iter::from_fn(|| q.pop_timeout(Duration::from_millis(1))).collect()
    }

    /// After a reconnect the stream restarts with the headers stamped with the
    /// first keyframe's timestamp (never 0), then that keyframe; audio and
    /// P-frames that arrive before it are discarded.
    #[test]
    fn reconnect_stamps_headers_with_keyframe_time() {
        let q = PacketQueue::new(QueueLimits { soft_bytes: 1 << 30, hard_bytes: 1 << 30, max_latency_ms: 0 });
        q.push(vh());
        q.push(ah());
        for f in 0..90u32 {
            q.push(v(f * 33, f % 60 == 0, 100));
            q.push(a(f * 33, 10));
        }
        drain(&q);

        q.reset_for_reconnect();
        q.push(a(1_200_000, 10));
        q.push(v(1_200_010, false, 100));
        q.push(v(1_200_043, true, 100));
        q.push(a(1_200_050, 10));
        let out = drain(&q);
        assert_eq!(out.len(), 4, "{out:?}");
        assert!(out[0].is_header && out[0].kind == Kind::Video && out[0].dts_ms == 1_200_043);
        assert!(out[1].is_header && out[1].kind == Kind::Audio && out[1].dts_ms == 1_200_043);
        assert!(out[2].keyframe && !out[2].is_header && out[2].dts_ms == 1_200_043);
        assert_eq!(out[3].dts_ms, 1_200_050);
    }

    /// Same for a session that joins a running stream and for pause/resume.
    #[test]
    fn late_join_and_resume_never_send_timestamp_zero_headers() {
        for resume in [false, true] {
            let q = PacketQueue::new(QueueLimits::default());
            if resume {
                q.push(vh());
                q.push(ah());
                q.set_paused(true);
                q.push(v(5000, true, 100));
                q.set_paused(false);
            } else {
                q.wait_for_keyframe();
                q.push(vh());
                q.push(ah());
            }
            q.push(v(60_000, false, 100));
            q.push(v(60_033, true, 100));
            let out = drain(&q);
            assert_eq!(out.len(), 3, "resume={resume} {out:?}");
            assert!(out.iter().all(|p| p.dts_ms == 60_033), "resume={resume} {out:?}");
            assert!(out[0].is_header && out[1].is_header && out[2].keyframe);
        }
    }

    #[test]
    fn push_never_blocks() {
        let q = PacketQueue::new(QueueLimits::default());
        let t = std::time::Instant::now();
        for i in 0..200_000u32 {
            q.push(v(i, i % 60 == 0, 2000));
        }
        assert!(t.elapsed() < Duration::from_secs(3));
        assert!(q.stats().1 <= QueueLimits::default().hard_bytes);
    }

    #[test]
    fn latency_cap_jumps_to_keyframe_within_window() {
        let q = PacketQueue::new(QueueLimits { soft_bytes: 1 << 30, hard_bytes: 1 << 30, max_latency_ms: 4000 });
        for f in 0..300u32 {
            // 10 s at 30 fps, IDR every 2 s, interleaved audio; nobody consumes
            q.push(v(f * 33, f % 60 == 0, 1000));
            q.push(a(f * 33, 100));
        }
        let (s, _) = q.stats();
        assert!(s.dropped_gops > 0 && s.dropped_audio_frames > 0);
        let first = q.pop_timeout(Duration::from_millis(10)).unwrap();
        assert!(first.keyframe, "must resume at a keyframe");
        assert!(9900u32.saturating_sub(first.dts_ms) <= 4000, "dts={}", first.dts_ms);
    }
}
