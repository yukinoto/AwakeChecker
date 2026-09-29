use crate::model::{EndReason, Observation, Policy, Presence};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Engine {
    state: Presence,
    online_candidate_since: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepOutcome {
    pub state: Presence,
    pub changed: bool,
    pub reason: Option<EndReason>,
}

impl Engine {
    pub fn new(state: Presence) -> Self {
        Self {
            state,
            online_candidate_since: None,
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> Presence {
        self.state
    }

    pub fn raw_online(obs: &Observation, policy: &Policy) -> bool {
        let locked_away = obs.locked && policy.locked_is_offline;
        !locked_away && obs.idle_secs < policy.idle_threshold_secs
    }

    pub fn step(&mut self, obs: Observation, policy: &Policy, policy_changed: bool) -> StepOutcome {
        let raw = Self::raw_online(&obs, policy);
        match self.state {
            Presence::Online => {
                self.online_candidate_since = None;
                if raw {
                    return self.unchanged();
                }
                self.state = Presence::Offline;
                let reason = if policy_changed {
                    EndReason::Policy
                } else if obs.locked && policy.locked_is_offline {
                    EndReason::Locked
                } else {
                    EndReason::Idle
                };
                StepOutcome {
                    state: self.state,
                    changed: true,
                    reason: Some(reason),
                }
            }
            Presence::Offline => {
                if !raw {
                    self.online_candidate_since = None;
                    return self.unchanged();
                }
                let since = *self.online_candidate_since.get_or_insert(obs.at_ms);
                let confirm_ms = policy.online_confirm_secs as i64 * 1000;
                if obs.at_ms - since < confirm_ms {
                    return self.unchanged();
                }
                self.online_candidate_since = None;
                self.state = Presence::Online;
                StepOutcome {
                    state: self.state,
                    changed: true,
                    reason: Some(EndReason::Returned),
                }
            }
        }
    }

    fn unchanged(&self) -> StepOutcome {
        StepOutcome {
            state: self.state,
            changed: false,
            reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(idle: u64, confirm: u64, locked_offline: bool) -> Policy {
        Policy {
            idle_threshold_secs: idle,
            locked_is_offline: locked_offline,
            online_confirm_secs: confirm,
            sample_interval_secs: 2,
        }
    }

    fn obs(at_ms: i64, idle_secs: u64, locked: bool) -> Observation {
        Observation {
            at_ms,
            idle_secs,
            locked,
        }
    }

    #[test]
    fn stays_online_while_idle_is_inside_threshold() {
        let mut engine = Engine::new(Presence::Online);
        let outcome = engine.step(obs(1_000, 10, false), &policy(180, 5, true), false);
        assert!(!outcome.changed);
        assert_eq!(engine.state(), Presence::Online);
    }

    #[test]
    fn idle_past_threshold_goes_offline() {
        let mut engine = Engine::new(Presence::Online);
        let outcome = engine.step(obs(1_000, 181, false), &policy(180, 5, true), false);
        assert!(outcome.changed);
        assert_eq!(outcome.reason, Some(EndReason::Idle));
        assert_eq!(engine.state(), Presence::Offline);
    }

    #[test]
    fn lock_goes_offline_immediately() {
        let mut engine = Engine::new(Presence::Online);
        let outcome = engine.step(obs(1_000, 1, true), &policy(180, 5, true), false);
        assert_eq!(outcome.reason, Some(EndReason::Locked));
        assert_eq!(engine.state(), Presence::Offline);
    }

    #[test]
    fn lock_can_be_ignored() {
        let mut engine = Engine::new(Presence::Online);
        let outcome = engine.step(obs(1_000, 1, true), &policy(180, 5, false), false);
        assert!(!outcome.changed);
        assert_eq!(engine.state(), Presence::Online);
    }

    #[test]
    fn brief_return_does_not_count_as_online() {
        let mut engine = Engine::new(Presence::Offline);
        let first = engine.step(obs(0, 0, false), &policy(180, 5, true), false);
        assert!(!first.changed);
        let second = engine.step(obs(4_999, 0, false), &policy(180, 5, true), false);
        assert!(!second.changed);
        assert_eq!(engine.state(), Presence::Offline);
    }

    #[test]
    fn confirmed_return_becomes_online() {
        let mut engine = Engine::new(Presence::Offline);
        engine.step(obs(0, 0, false), &policy(180, 5, true), false);
        let outcome = engine.step(obs(5_000, 0, false), &policy(180, 5, true), false);
        assert!(outcome.changed);
        assert_eq!(outcome.reason, Some(EndReason::Returned));
        assert_eq!(engine.state(), Presence::Online);
    }

    #[test]
    fn interrupted_return_restarts_confirm() {
        let mut engine = Engine::new(Presence::Offline);
        engine.step(obs(0, 0, false), &policy(180, 5, true), false);
        engine.step(obs(2_000, 400, false), &policy(180, 5, true), false);
        engine.step(obs(3_000, 0, false), &policy(180, 5, true), false);
        let too_soon = engine.step(obs(7_000, 0, false), &policy(180, 5, true), false);
        assert!(!too_soon.changed);
        let online = engine.step(obs(8_000, 0, false), &policy(180, 5, true), false);
        assert!(online.changed);
        assert_eq!(engine.state(), Presence::Online);
    }

    #[test]
    fn stricter_policy_ends_the_online_segment() {
        let mut engine = Engine::new(Presence::Online);
        let outcome = engine.step(obs(1_000, 10, false), &policy(5, 5, true), true);
        assert!(outcome.changed);
        assert_eq!(outcome.reason, Some(EndReason::Policy));
        assert_eq!(engine.state(), Presence::Offline);
    }

    #[test]
    fn zero_confirm_flips_online_on_the_first_active_sample() {
        let mut engine = Engine::new(Presence::Offline);
        let outcome = engine.step(obs(1_000, 0, false), &policy(180, 0, true), false);
        assert!(outcome.changed);
        assert_eq!(engine.state(), Presence::Online);
    }
}
