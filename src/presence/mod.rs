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

fn tick(
    db: &Db,
    live: &Live,
    engine: &mut Engine,
    last_policy: &mut Policy,
) -> anyhow::Result<()> {
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
