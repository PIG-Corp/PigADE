//! Fail-closed state for the bounded UI-to-host command worker.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResizeKey {
    pub generation: String,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandNotice {
    /// Changes whenever the notice changes, so an acknowledgement based on an
    /// older UI snapshot cannot clear newer dropped-command details.
    pub token: u64,
    pub message: String,
    pub unsent: u32,
    /// These resizes are known not to have reached the host and may be retried
    /// after explicit acknowledgement. The ambiguous failed command is omitted.
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Ready,
    AwaitingAcknowledgement,
    BlockedUnknown,
}

#[derive(Default)]
pub(crate) struct WorkerGate {
    phase: Phase,
    notice: Option<CommandNotice>,
    next_token: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Dispatch {
    Executed,
    Dropped,
    RejectedKnown,
    OutcomeUnknown,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestFailure {
    NotAdmitted(String),
    OutcomeUnknown(String),
}

impl WorkerGate {
    pub fn may_execute(&self) -> bool {
        self.phase == Phase::Ready
    }

    pub fn notice(&self) -> Option<&CommandNotice> {
        self.notice.as_ref()
    }

    pub fn reject_before_admission(&mut self, message: String, rejected_resize: Option<ResizeKey>) {
        let mut rejected_resizes = Vec::new();
        if let Some(resize) = rejected_resize {
            rejected_resizes.push(resize);
        }
        self.phase = Phase::AwaitingAcknowledgement;
        self.set_notice(CommandNotice {
            token: 0,
            message,
            unsent: 0,
            rejected_resizes,
            acknowledgeable: true,
        });
    }

    pub fn block_unknown(&mut self, message: String) {
        self.phase = Phase::BlockedUnknown;
        self.set_notice(CommandNotice {
            token: 0,
            message,
            unsent: 0,
            rejected_resizes: Vec::new(),
            acknowledgeable: false,
        });
    }

    /// Discard a later queued request while either failure gate is active.
    pub fn record_unsent(&mut self, resize: Option<ResizeKey>) {
        let Some(notice) = &mut self.notice else {
            return;
        };
        notice.unsent = notice.unsent.saturating_add(1);
        if let Some(resize) = resize {
            // The canvas admits one pending size per visible generation.
            // Replace older dropped sizes for that generation instead of
            // retaining every intermediate drag geometry.
            notice
                .rejected_resizes
                .retain(|pending| pending.generation != resize.generation);
            notice.rejected_resizes.push(resize);
        }
        notice.token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
    }

    /// Only an explicit acknowledgement after a known pre-admission rejection
    /// reopens the worker. Unknown outcomes remain permanently blocked.
    pub fn acknowledge(&mut self, expected_token: u64) -> bool {
        if self.phase != Phase::AwaitingAcknowledgement
            || self.notice.as_ref().map(|notice| notice.token) != Some(expected_token)
        {
            return false;
        }
        self.phase = Phase::Ready;
        self.notice = None;
        true
    }

    /// Invoke the command closure only while ready. Failures atomically close
    /// the gate before the caller drains another queued message.
    pub fn dispatch_request(
        &mut self,
        resize: Option<ResizeKey>,
        execute: impl FnOnce() -> Result<(), RequestFailure>,
    ) -> Dispatch {
        if !self.may_execute() {
            self.record_unsent(resize);
            return Dispatch::Dropped;
        }
        match execute() {
            Ok(()) => Dispatch::Executed,
            Err(RequestFailure::NotAdmitted(message)) => {
                self.reject_before_admission(message, resize);
                Dispatch::RejectedKnown
            }
            Err(RequestFailure::OutcomeUnknown(message)) => {
                self.block_unknown(message);
                Dispatch::OutcomeUnknown
            }
        }
    }

    fn set_notice(&mut self, mut notice: CommandNotice) {
        notice.token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        self.notice = Some(notice);
    }
}

#[derive(Default)]
pub(crate) struct ResizeCache(Vec<ResizeKey>);

impl ResizeCache {
    pub fn contains(&self, key: &ResizeKey) -> bool {
        self.0.contains(key)
    }

    pub fn remember(&mut self, key: ResizeKey) {
        self.0
            .retain(|pending| pending.generation != key.generation);
        self.0.push(key);
    }

    pub fn apply_notice(&mut self, notice: &CommandNotice) {
        self.0
            .retain(|pending| !notice.rejected_resizes.contains(pending));
    }

    pub fn retain_generations<'a>(&mut self, generations: impl Iterator<Item = &'a str>) {
        let generations = generations.collect::<Vec<_>>();
        self.0
            .retain(|pending| generations.contains(&pending.generation.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resize(generation: &str, rows: u16, cols: u16) -> ResizeKey {
        ResizeKey {
            generation: generation.into(),
            rows,
            cols,
        }
    }

    #[test]
    fn known_rejection_reopens_only_after_ack_and_future_input_is_new_work() {
        let mut gate = WorkerGate::default();
        assert!(gate.may_execute());
        gate.reject_before_admission("queue full".into(), None);
        assert!(!gate.may_execute());
        assert!(gate.acknowledge(gate.notice().unwrap().token));
        assert!(gate.may_execute());
        // The gate never retains or replays the rejected command.
        assert!(gate.notice().is_none());
    }

    #[test]
    fn unknown_outcome_cannot_be_unlocked_by_ack() {
        let mut gate = WorkerGate::default();
        gate.block_unknown("input outcome unknown".into());
        assert!(!gate.acknowledge(gate.notice().unwrap().token));
        assert!(!gate.may_execute());
        assert_eq!(gate.notice().unwrap().message, "input outcome unknown");
    }

    #[test]
    fn later_queued_commands_are_discarded_and_reported_until_ack() {
        let mut gate = WorkerGate::default();
        let mut attempts = Vec::new();
        assert_eq!(
            gate.dispatch_request(None, || {
                attempts.push("first");
                Err(RequestFailure::NotAdmitted("resize not admitted".into()))
            }),
            Dispatch::RejectedKnown
        );
        let mut executed = Vec::new();
        for command in ["queued-input", "queued-resize"] {
            let outcome = gate.dispatch_request(
                (command == "queued-resize").then(|| resize("g", 20, 80)),
                || {
                    attempts.push(command);
                    Ok(())
                },
            );
            if outcome == Dispatch::Executed {
                executed.push(command);
            }
        }
        assert!(executed.is_empty());
        assert_eq!(attempts, ["first"]);
        let notice = gate.notice().unwrap();
        assert_eq!(notice.unsent, 2);
        assert_eq!(notice.rejected_resizes, [resize("g", 20, 80)]);
        assert!(notice.display_message().contains("не отправлены: 2"));
        let token = notice.token;
        assert!(gate.acknowledge(token));
        assert!(gate.may_execute());
        assert_eq!(
            gate.dispatch_request(None, || {
                attempts.push("fresh-input");
                Ok(())
            }),
            Dispatch::Executed
        );
        assert_eq!(attempts, ["first", "fresh-input"]);
    }

    #[test]
    fn explicit_ack_clears_failed_and_unsent_resize_cache_for_retry() {
        let mut cache = ResizeCache::default();
        let accepted_but_unacknowledged = resize("g1", 25, 80);
        let rejected = resize("g2", 20, 60);
        cache.remember(accepted_but_unacknowledged.clone());
        cache.remember(rejected.clone());

        let mut gate = WorkerGate::default();
        gate.reject_before_admission("resize queue full".into(), Some(rejected.clone()));
        gate.record_unsent(Some(resize("g3", 18, 50)));
        cache.apply_notice(gate.notice().unwrap());
        assert!(cache.contains(&accepted_but_unacknowledged));
        assert!(!cache.contains(&rejected));
        assert!(!cache.contains(&resize("g3", 18, 50)));
        assert!(!gate.may_execute());

        assert!(gate.acknowledge(gate.notice().unwrap().token));
        assert!(gate.may_execute());
        assert!(!cache.contains(&rejected)); // geometry may enqueue a fresh resize now.
    }

    #[test]
    fn stale_ack_does_not_clear_newly_reported_unsent_resize() {
        let mut gate = WorkerGate::default();
        gate.reject_before_admission("full".into(), None);
        let stale_token = gate.notice().unwrap().token;
        assert_eq!(
            gate.dispatch_request(Some(resize("g2", 21, 80)), || panic!("must be dropped")),
            Dispatch::Dropped
        );
        let current = gate.notice().unwrap().clone();
        assert_ne!(stale_token, current.token);
        assert!(!gate.acknowledge(stale_token));
        assert_eq!(gate.notice(), Some(&current));
        assert!(gate.acknowledge(current.token));
    }

    #[test]
    fn unknown_dispatch_stays_blocked_and_never_retries() {
        let mut gate = WorkerGate::default();
        let mut attempts = 0;
        assert_eq!(
            gate.dispatch_request(None, || {
                attempts += 1;
                Err(RequestFailure::OutcomeUnknown("unknown".into()))
            }),
            Dispatch::OutcomeUnknown
        );
        let token = gate.notice().unwrap().token;
        assert!(!gate.acknowledge(token));
        assert_eq!(
            gate.dispatch_request(None, || {
                attempts += 1;
                Ok(())
            }),
            Dispatch::Dropped
        );
        assert_eq!(attempts, 1);
    }
}
