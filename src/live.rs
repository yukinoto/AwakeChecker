use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU8, Ordering},
};

use crate::model::{Presence, Segment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveSnapshot {
    pub online: bool,
    pub idle_secs: u64,
    pub locked: bool,
    pub since_ms: i64,
}

static PRESENCE_CODE: AtomicU8 = AtomicU8::new(0);

pub fn presence_code() -> u8 {
    PRESENCE_CODE.load(Ordering::Relaxed)
}

impl Default for LiveSnapshot {
    fn default() -> Self {
        Self {
            online: false,
            idle_secs: 0,
            locked: false,
            since_ms: 0,
        }
    }
}

impl LiveSnapshot {
    pub fn from_segment(segment: &Segment, idle_secs: u64, locked: bool) -> Self {
        Self {
            online: segment.state == Presence::Online,
            idle_secs,
            locked,
            since_ms: segment.started_at,
        }
    }
}

#[derive(Clone, Default)]
pub struct Live {
    inner: Arc<RwLock<LiveSnapshot>>,
}

impl Live {
    pub fn publish(&self, snapshot: LiveSnapshot) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = snapshot;
        }
        let code = if snapshot.online { 1 } else { 2 };
        PRESENCE_CODE.store(code, Ordering::Relaxed);
    }

    pub fn get(&self) -> LiveSnapshot {
        self.inner
            .read()
            .map(|guard| *guard)
            .unwrap_or_default()
    }
}
