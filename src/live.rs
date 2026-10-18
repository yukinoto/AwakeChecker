use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU8, Ordering},
};

use crate::model::{Presence, Segment};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveSnapshot {
    pub online: bool,
    pub idle_secs: u64,
    pub locked: bool,
    pub since_ms: i64,
}

static PRESENCE_CODE: AtomicU8 = AtomicU8::new(0);

#[cfg_attr(not(windows), allow(dead_code))]
pub fn presence_code() -> u8 {
    PRESENCE_CODE.load(Ordering::Relaxed)
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
        self.inner.read().map(|guard| *guard).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Presence, Segment};

    #[test]
    fn publish_replaces_the_snapshot_and_the_tray_code() {
        let segment = Segment {
            id: 1,
            state: Presence::Online,
            started_at: 42,
            ended_at: None,
            end_reason: None,
            last_seen_at: 42,
        };
        let snapshot = LiveSnapshot::from_segment(&segment, 7, true);
        assert!(snapshot.online);
        assert_eq!(snapshot.idle_secs, 7);
        assert!(snapshot.locked);
        assert_eq!(snapshot.since_ms, 42);

        let live = Live::default();
        assert_eq!(live.get(), LiveSnapshot::default());
        live.publish(snapshot);
        assert_eq!(live.get(), snapshot);
        assert_eq!(presence_code(), 1);
        live.publish(LiveSnapshot {
            online: false,
            ..snapshot
        });
        assert!(!live.get().online);
        assert_eq!(presence_code(), 2);
    }
}
