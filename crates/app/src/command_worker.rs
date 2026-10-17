//! Bounded per-session command failure gates for the UI-to-host worker.

use std::collections::HashMap;

pub(crate) const MAX_GATED_TARGETS: usize = 32;

pub(crate) fn confirmed_ended_state(state: &str) -> bool {
    matches!(state, "exited" | "history")
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct CommandTarget {
    pub session: String,
    pub generation: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResizeKey {
    pub target: CommandTarget,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandNotice {
    /// Globally increasing token prevents stale ACKs after a target entry is
    /// removed and later recreated for the same session generation.
    pub token: u64,
    /// Highest request id for this target represented by this notice.
    pub last_request_id: u64,
    pub target: CommandTarget,
    pub message: String,
    pub unsent: u32,
    pub rejected_resizes: Vec<ResizeKey>,
    pub acknowledgeable: bool,
}

impl CommandNotice {
    pub fn display_message(&self) -> String {
        if self.unsent == 0 {
            self.message.clone()
        } else {
            format!(
                "{} · последующие команды не отправлены: {}",
                self.message, self.unsent
            )
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    AwaitingAcknowledgement,
    BlockedUnknown,
}

#[derive(Default)]
pub(crate) struct WorkerGate {
    blocked: HashMap<CommandTarget, (Phase, CommandNotice)>,
    next_token: u64,
    overflow: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Dispatch {
    Executed,
    Dropped,
    RejectedKnown,
    OutcomeUnknown,
    CapacityExceeded,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestFailure {
    NotAdmitted(String),
    OutcomeUnknown(String),
}

impl WorkerGate {
    #[cfg(test)]
    pub fn may_execute(&self, target: &CommandTarget) -> bool {
        !self.overflow && !self.blocked.contains_key(target)
    }

    #[cfg(test)]
    pub fn notice(&self, target: &CommandTarget) -> Option<&CommandNotice> {
        self.blocked.get(target).map(|(_, notice)| notice)
    }

    pub fn notices(&self) -> Vec<CommandNotice> {
        let mut notices = self
            .blocked
            .values()
            .map(|(_, notice)| notice.clone())
            .collect::<Vec<_>>();
        notices.sort_by(|left, right| left.target.cmp(&right.target));
        notices
    }

    #[cfg(test)]
    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    /// Acknowledge only the exact known rejection currently displayed for the
    /// target. Unknown outcomes are permanently locked until the generation ends.
    pub fn acknowledge(&mut self, target: &CommandTarget, expected_token: u64) -> bool {
        let Some((Phase::AwaitingAcknowledgement, notice)) = self.blocked.get(target) else {
            return false;
        };
        if notice.token != expected_token {
            return false;
        }
        self.blocked.remove(target);
        true
    }

    /// Invoke the command closure only while this target is ready. A failure
    /// atomically closes that target before the caller drains another message.
    #[cfg(test)]
    pub fn dispatch_request(
        &mut self,
        target: CommandTarget,
        resize: Option<ResizeKey>,
        execute: impl FnOnce() -> Result<(), RequestFailure>,
    ) -> Dispatch {
        self.dispatch_request_with_id(target, resize, 0, execute)
    }

    pub fn dispatch_request_with_id(
        &mut self,
        target: CommandTarget,
        resize: Option<ResizeKey>,
        request_id: u64,
        execute: impl FnOnce() -> Result<(), RequestFailure>,
    ) -> Dispatch {
        if self.overflow {
            return Dispatch::CapacityExceeded;
        }
        if self.blocked.contains_key(&target) {
            let Some(token) = self.take_token() else {
                self.overflow = true;
                return Dispatch::CapacityExceeded;
            };
            let (_, notice) = self.blocked.get_mut(&target).unwrap();
            notice.unsent = notice.unsent.saturating_add(1);
            notice.last_request_id = notice.last_request_id.max(request_id);
            if let Some(resize) = resize {
                notice
                    .rejected_resizes
                    .retain(|pending| pending.target != target);
                notice.rejected_resizes.push(resize);
            }
            notice.token = token;
            return Dispatch::Dropped;
        }
        match execute() {
            Ok(()) => Dispatch::Executed,
            Err(RequestFailure::NotAdmitted(message)) => self.block(
                target,
                Phase::AwaitingAcknowledgement,
                message,
                resize,
                request_id,
            ),
            Err(RequestFailure::OutcomeUnknown(message)) => {
                self.block(target, Phase::BlockedUnknown, message, None, request_id)
            }
        }
    }

    /// Remove only generations that the host explicitly reports as ended.
    /// Mere absence from a poll snapshot is not evidence that a gate is stale.
    pub fn remove_ended(&mut self, targets: impl Iterator<Item = CommandTarget>) {
        for target in targets {
            self.blocked.remove(&target);
        }
    }

    fn block(
        &mut self,
        target: CommandTarget,
        phase: Phase,
        message: String,
        resize: Option<ResizeKey>,
        request_id: u64,
    ) -> Dispatch {
        if self.blocked.len() >= MAX_GATED_TARGETS {
            self.overflow = true;
            return Dispatch::CapacityExceeded;
        }
        let Some(token) = self.take_token() else {
            self.overflow = true;
            return Dispatch::CapacityExceeded;
        };
        let mut rejected_resizes = Vec::new();
        if let Some(resize) = resize {
            rejected_resizes.push(resize);
        }
        let notice = CommandNotice {
            token,
            last_request_id: request_id,
            target: target.clone(),
            message,
            unsent: 0,
            rejected_resizes,
            acknowledgeable: phase == Phase::AwaitingAcknowledgement,
        };
        self.blocked.insert(target, (phase, notice));
        match phase {
            Phase::AwaitingAcknowledgement => Dispatch::RejectedKnown,
            Phase::BlockedUnknown => Dispatch::OutcomeUnknown,
        }
    }

    fn take_token(&mut self) -> Option<u64> {
        let token = self.next_token;
        self.next_token = self.next_token.checked_add(1)?;
        Some(token)
    }
}

#[derive(Default)]
pub(crate) struct ResizeCache(Vec<ResizeKey>);

impl ResizeCache {
    pub fn contains(&self, key: &ResizeKey) -> bool {
        self.0.contains(key)
    }

    pub fn remember(&mut self, key: ResizeKey) {
        self.0.retain(|pending| pending.target != key.target);
        self.0.push(key);
    }

    pub fn apply_notices(&mut self, notices: &[CommandNotice]) {
        self.0.retain(|pending| {
            !notices
                .iter()
                .any(|notice| notice.rejected_resizes.contains(pending))
        });
    }

    pub fn retain_targets(&mut self, targets: impl Iterator<Item = CommandTarget>) {
        let targets = targets.collect::<std::collections::HashSet<_>>();
        self.0.retain(|pending| targets.contains(&pending.target));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str) -> CommandTarget {
        CommandTarget {
            session: format!("session-{name}"),
            generation: format!("generation-{name}"),
        }
    }

    fn resize(target: CommandTarget, rows: u16, cols: u16) -> ResizeKey {
        ResizeKey { target, rows, cols }
    }

    #[test]
    fn rejection_isolated_to_target_and_other_target_executes_once() {
        let a = target("a");
        let b = target("b");
        let mut gate = WorkerGate::default();
        let mut attempts = Vec::new();
        assert_eq!(
            gate.dispatch_request(a.clone(), None, || {
                attempts.push("a-first");
                Err(RequestFailure::NotAdmitted("full A".into()))
            }),
            Dispatch::RejectedKnown
        );
        assert_eq!(
            gate.dispatch_request(a.clone(), None, || {
                attempts.push("a-later");
                Ok(())
            }),
            Dispatch::Dropped
        );
        assert_eq!(
            gate.dispatch_request(b.clone(), None, || {
                attempts.push("b-input");
                Ok(())
            }),
            Dispatch::Executed
        );
        assert_eq!(attempts, ["a-first", "b-input"]);
        assert!(!gate.may_execute(&a));
        assert!(gate.may_execute(&b));
        assert_eq!(gate.notice(&a).unwrap().unsent, 1);
        assert!(gate.notice(&b).is_none());
    }

    #[test]
    fn unknown_outcome_only_locks_its_target_and_never_retries() {
        let a = target("a");
        let b = target("b");
        let mut gate = WorkerGate::default();
        let mut attempts = 0;
        assert_eq!(
            gate.dispatch_request(a.clone(), None, || {
                attempts += 1;
                Err(RequestFailure::OutcomeUnknown("unknown A".into()))
            }),
            Dispatch::OutcomeUnknown
        );
        let token = gate.notice(&a).unwrap().token;
        assert!(!gate.acknowledge(&a, token));
        assert_eq!(
            gate.dispatch_request(a.clone(), None, || {
                attempts += 1;
                Ok(())
            }),
            Dispatch::Dropped
        );
        assert_eq!(
            gate.dispatch_request(b, None, || Ok(())),
            Dispatch::Executed
        );
        assert_eq!(attempts, 1);
    }

    #[test]
    fn known_rejection_ack_requires_current_notice_and_future_work_is_new() {
        let a = target("a");
        let mut gate = WorkerGate::default();
        assert_eq!(
            gate.dispatch_request(a.clone(), None, || {
                Err(RequestFailure::NotAdmitted("full".into()))
            }),
            Dispatch::RejectedKnown
        );
        let token = gate.notice(&a).unwrap().token;
        assert!(!gate.acknowledge(&a, token.wrapping_sub(1)));
        assert!(gate.acknowledge(&a, token));
        let mut attempts = 0;
        assert_eq!(
            gate.dispatch_request(a, None, || {
                attempts += 1;
                Ok(())
            }),
            Dispatch::Executed
        );
        assert_eq!(attempts, 1);
    }

    #[test]
    fn absent_or_coexisting_generation_does_not_clear_hidden_target_gate() {
        let a = target("a");
        let mut gate = WorkerGate::default();
        gate.dispatch_request(a.clone(), None, || {
            Err(RequestFailure::NotAdmitted("full".into()))
        });
        let old_token = gate.notice(&a).unwrap().token;
        // Absence from a snapshot cannot clear a gate.
        assert!(!gate.may_execute(&a));
        // The host can return multiple concurrent generations for one session.
        let coexisting = CommandTarget {
            session: a.session.clone(),
            generation: "generation-new".into(),
        };
        assert_ne!(a.generation, coexisting.generation);
        assert!(!gate.may_execute(&a));
        // Absence and another generation do not prove this one ended.
        gate.remove_ended(std::iter::empty());
        assert!(!gate.may_execute(&a));
        // A specific terminal lifecycle observation permits cleanup.
        gate.remove_ended([a.clone()].into_iter());
        assert!(gate.may_execute(&a));
        gate.dispatch_request(a.clone(), None, || {
            Err(RequestFailure::NotAdmitted("full again".into()))
        });
        let new_token = gate.notice(&a).unwrap().token;
        assert_ne!(old_token, new_token);
        assert!(!gate.acknowledge(&a, old_token));
    }

    #[test]
    fn stale_ack_cannot_clear_new_dropped_resize_details() {
        let a = target("a");
        let mut gate = WorkerGate::default();
        gate.dispatch_request(a.clone(), None, || {
            Err(RequestFailure::NotAdmitted("full".into()))
        });
        let stale = gate.notice(&a).unwrap().token;
        let dropped = resize(a.clone(), 21, 80);
        assert_eq!(
            gate.dispatch_request(a.clone(), Some(dropped.clone()), || panic!("must drop")),
            Dispatch::Dropped
        );
        assert_ne!(gate.notice(&a).unwrap().token, stale);
        assert!(!gate.acknowledge(&a, stale));
        assert_eq!(gate.notice(&a).unwrap().rejected_resizes, [dropped]);
    }

    #[test]
    fn gate_and_resize_cache_are_bounded_and_exactly_scoped() {
        let mut gate = WorkerGate::default();
        for i in 0..MAX_GATED_TARGETS {
            let t = target(&i.to_string());
            assert_eq!(
                gate.dispatch_request(t, None, || {
                    Err(RequestFailure::NotAdmitted("full".into()))
                }),
                Dispatch::RejectedKnown
            );
        }
        assert_eq!(
            gate.dispatch_request(target("overflow"), None, || {
                Err(RequestFailure::NotAdmitted("full".into()))
            }),
            Dispatch::CapacityExceeded
        );
        assert!(gate.overflowed());
        assert_eq!(gate.notices().len(), MAX_GATED_TARGETS);
        assert_eq!(
            gate.dispatch_request(target("unrelated"), None, || Ok(())),
            Dispatch::CapacityExceeded
        );

        let a = target("cache-a");
        let b = target("cache-b");
        let key = resize(a.clone(), 20, 80);
        let mut cache = ResizeCache::default();
        cache.remember(key.clone());
        cache.apply_notices(&[CommandNotice {
            token: 0,
            last_request_id: 0,
            target: b.clone(),
            message: String::new(),
            unsent: 0,
            rejected_resizes: vec![resize(b, 20, 80)],
            acknowledgeable: true,
        }]);
        assert!(cache.contains(&key));
        cache.retain_targets([a.clone()].into_iter());
        assert!(cache.contains(&key));
        cache.retain_targets(std::iter::empty());
        assert!(!cache.contains(&key));
    }

    #[test]
    fn token_exhaustion_fails_closed_instead_of_reusing_stale_ack_ids() {
        let mut gate = WorkerGate {
            next_token: u64::MAX,
            ..WorkerGate::default()
        };
        assert_eq!(
            gate.dispatch_request(target("a"), None, || {
                Err(RequestFailure::NotAdmitted("full".into()))
            }),
            Dispatch::CapacityExceeded
        );
        assert!(gate.overflowed());
        assert!(gate.notices().is_empty());
    }

    #[test]
    fn only_explicit_terminal_states_release_target_gates() {
        assert!(confirmed_ended_state("exited"));
        assert!(confirmed_ended_state("history"));
        assert!(!confirmed_ended_state("running"));
        assert!(!confirmed_ended_state("degraded"));
        assert!(!confirmed_ended_state("future-state"));
    }
}
