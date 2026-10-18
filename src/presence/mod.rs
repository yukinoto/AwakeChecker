mod engine;
mod windows;

use std::time::Duration;

use tokio::sync::watch;
use tracing::{info, warn};

pub use engine::Engine;

use crate::{
    db::Db,
    live::Live,
    model::{EndReason, Observation, Policy, Presence, STALE_GAP_MS, now_ms},
};

pub async fn run(db: Db, live: Live, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
    let mut engine = bootstrap(&db, &live)?;
    let mut last_policy = db.policy()?;
    loop {
        let interval = last_policy.sample_interval_secs.max(1);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
            changed = shutdown.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
        if *shutdown.borrow() {
            finish(&db)?;
            break;
        }
        if let Err(err) = tick(&db, &live, &mut engine, &mut last_policy) {
            warn!(error = %err, "采样失败");
        }
    }
    Ok(())
}

pub fn finish(db: &Db) -> anyhow::Result<()> {
    if let Some(open) = db.current_segment()? {
        db.close_segment(open.id, now_ms(), EndReason::Stopped)?;
        info!("当前时段已按退出收尾");
    }
    Ok(())
}

fn bootstrap(db: &Db, live: &Live) -> anyhow::Result<Engine> {
    let now = now_ms();
    if let Some(open) = db.current_segment()?
        && now.saturating_sub(open.last_seen_at) > STALE_GAP_MS
    {
        db.close_segment(open.id, open.last_seen_at, EndReason::Stopped)?;
        info!(segment = open.id, "上次未收尾的时段已截断");
    }

    let policy = db.policy()?;
    let (idle_secs, locked) = windows::sample_activity()?;
    let obs = Observation {
        at_ms: now,
        idle_secs,
        locked,
    };
    let raw = if Engine::raw_online(&obs, &policy) {
        Presence::Online
    } else {
        Presence::Offline
    };
    let segment = if let Some(open) = db.current_segment()? {
        if open.state == raw {
            db.touch_segment(open.id, now)?;
            open
        } else {
            let reason = end_reason(open.state, &obs, &policy, false);
            db.transition(raw, now, reason)?
        }
    } else {
        db.insert_segment(raw, now)?
    };
    live.publish(crate::live::LiveSnapshot::from_segment(
        &segment, idle_secs, locked,
    ));
    Ok(Engine::new(segment.state))
}

fn tick(db: &Db, live: &Live, engine: &mut Engine, last_policy: &mut Policy) -> anyhow::Result<()> {
    let policy = db.policy()?;
    let (idle_secs, locked) = windows::sample_activity()?;
    let obs = Observation {
        at_ms: now_ms(),
        idle_secs,
        locked,
    };
    let policy_changed = policy != *last_policy;
    let outcome = engine.step(obs, &policy, policy_changed);
    let segment = if outcome.changed {
        let reason = outcome.reason.unwrap_or(EndReason::Idle);
        db.transition(outcome.state, obs.at_ms, reason)?
    } else if let Some(open) = db.current_segment()? {
        db.touch_segment(open.id, obs.at_ms)?;
        open
    } else {
        db.insert_segment(outcome.state, obs.at_ms)?
    };
    live.publish(crate::live::LiveSnapshot::from_segment(
        &segment, idle_secs, locked,
    ));
    *last_policy = policy;
    Ok(())
}

fn end_reason(
    previous: Presence,
    obs: &Observation,
    policy: &Policy,
    policy_changed: bool,
) -> EndReason {
    if previous == Presence::Offline {
        return EndReason::Returned;
    }
    if policy_changed {
        EndReason::Policy
    } else if obs.locked && policy.locked_is_offline {
        EndReason::Locked
    } else {
        EndReason::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (Db, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("awake-presence-{}-{n}.db", std::process::id()));
        (Db::open(&path).unwrap(), path)
    }

    #[test]
    fn end_reason_prefers_return_then_policy_then_lock() {
        let policy = Policy::default();
        let locked = Observation {
            at_ms: 0,
            idle_secs: 1,
            locked: true,
        };
        assert_eq!(
            end_reason(Presence::Offline, &locked, &policy, true),
            EndReason::Returned
        );
        assert_eq!(
            end_reason(Presence::Online, &locked, &policy, true),
            EndReason::Policy
        );
        assert_eq!(
            end_reason(Presence::Online, &locked, &policy, false),
            EndReason::Locked
        );
        let idle = Observation {
            at_ms: 0,
            idle_secs: 999,
            locked: false,
        };
        assert_eq!(
            end_reason(Presence::Online, &idle, &policy, false),
            EndReason::Idle
        );
        let ignore_lock = Policy {
            locked_is_offline: false,
            ..Policy::default()
        };
        assert_eq!(
            end_reason(Presence::Online, &locked, &ignore_lock, false),
            EndReason::Idle
        );
    }

    #[test]
    fn finish_closes_only_an_open_segment() {
        let (db, path) = temp_db();
        finish(&db).unwrap();
        assert!(db.current_segment().unwrap().is_none());
        db.insert_segment(Presence::Online, 1_000).unwrap();
        finish(&db).unwrap();
        let rows = db.segments_between(0, i64::MAX).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].end_reason, Some(EndReason::Stopped));
        assert!(rows[0].ended_at.unwrap() >= 1_000);
        assert!(db.current_segment().unwrap().is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn bootstrap_truncates_a_stale_segment_and_keeps_a_fresh_one() {
        let (db, path) = temp_db();
        let stale = db.insert_segment(Presence::Online, 1_000).unwrap();
        db.touch_segment(stale.id, 1_000).unwrap();
        let live = Live::default();
        let stale_result = bootstrap(&db, &live);
        let closed = db.segments_between(0, i64::MAX).unwrap();
        let closed = closed.iter().find(|row| row.id == stale.id).unwrap();
        assert_eq!(closed.end_reason, Some(EndReason::Stopped));
        assert_eq!(closed.ended_at, Some(1_000));
        if !cfg!(windows) {
            assert!(stale_result.unwrap_err().to_string().contains("Windows"));
            let fresh = db.insert_segment(Presence::Offline, now_ms()).unwrap();
            let fresh_result = bootstrap(&db, &live);
            let current = db.current_segment().unwrap().unwrap();
            assert_eq!(current.id, fresh.id);
            assert!(current.ended_at.is_none());
            assert!(fresh_result.unwrap_err().to_string().contains("Windows"));
            let mut engine = Engine::new(Presence::Offline);
            let mut policy = db.policy().unwrap();
            assert!(
                tick(&db, &live, &mut engine, &mut policy)
                    .unwrap_err()
                    .to_string()
                    .contains("Windows")
            );
        }
        let _ = std::fs::remove_file(path);
    }
}
