//! Registry of independent sessions and packet fan-out.

use crate::queue::MediaPacket;
use crate::session::{Session, SessionConfig, SessionSnapshot, TransportFactory};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

pub struct SessionManager {
    sessions: RwLock<HashMap<u64, Arc<Session>>>,
    next_id: AtomicU64,
    /// Current sequence headers [video, audio]; applied to new sessions.
    headers: Mutex<[Option<MediaPacket>; 2]>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self { sessions: RwLock::new(HashMap::new()), next_id: AtomicU64::new(1), headers: Mutex::new([None, None]) }
    }

    pub fn add(&self, cfg: SessionConfig, factory: TransportFactory) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let s = Session::spawn(id, cfg, factory);
        {
            let hs = self.headers.lock().unwrap();
            if hs.iter().any(|h| h.is_some()) {
                s.start_at_keyframe(); // stream already running: join at the next IDR
            }
            for h in hs.iter().flatten() {
                s.push(h.clone());
            }
        }
        self.sessions.write().unwrap().insert(id, s);
        id
    }

    /// Returns immediately; the thread finishes on its own.
    pub fn remove(&self, id: u64) -> bool {
        match self.sessions.write().unwrap().remove(&id) {
            Some(s) => {
                s.stop();
                true
            }
            None => false,
        }
    }

    /// Stores the sequence header and sends it to every session.
    pub fn set_header(&self, pkt: MediaPacket) {
        debug_assert!(pkt.is_header);
        let idx = (pkt.kind == crate::queue::Kind::Audio) as usize;
        self.headers.lock().unwrap()[idx] = Some(pkt.clone());
        self.push_all(&pkt);
    }

    /// When streaming ends: forget the headers from the previous capture session.
    pub fn clear_headers(&self) {
        *self.headers.lock().unwrap() = [None, None];
    }

    /// Fan-out: `Bytes` is shared by reference, not copied per destination.
    pub fn push_all(&self, pkt: &MediaPacket) {
        for s in self.sessions.read().unwrap().values() {
            s.push(pkt.clone());
        }
    }

    pub fn push_to(&self, id: u64, pkt: MediaPacket) -> bool {
        self.sessions.read().unwrap().get(&id).map(|s| s.push(pkt)).unwrap_or(false)
    }

    pub fn pause(&self, id: u64) -> bool {
        self.with(id, |s| s.pause())
    }

    pub fn resume(&self, id: u64) -> bool {
        self.with(id, |s| s.resume())
    }

    pub fn restart(&self, id: u64) -> bool {
        self.with(id, |s| s.restart())
    }

    fn with(&self, id: u64, f: impl FnOnce(&Session)) -> bool {
        self.sessions.read().unwrap().get(&id).map(|s| f(s)).is_some()
    }

    pub fn snapshot(&self, id: u64) -> Option<SessionSnapshot> {
        self.sessions.read().unwrap().get(&id).map(|s| s.snapshot())
    }

    pub fn shutdown(&self) {
        let all: Vec<_> = self.sessions.write().unwrap().drain().map(|(_, s)| s).collect();
        for s in &all {
            s.stop();
        }
        for s in &all {
            s.join();
        }
    }
}
