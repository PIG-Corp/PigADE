//! Native window close is a review/flush protocol, never a process-stop command.
use crate::dirty_documents::DraftNotice;
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Review,
    Flushing,
    Blocked,
}

#[derive(Default)]
pub(crate) struct CloseFlow {
    pub phase: Option<Phase>,
    pub drafts: Vec<DraftNotice>,
    pub pending: Vec<&'static str>,
    pub error: Option<String>,
    pub confirm_selected: bool,
    authorized: BTreeSet<String>,
    started: Option<Instant>,
}

impl CloseFlow {
    pub fn active(&self) -> bool {
        self.phase.is_some()
    }

    /// True means there is nothing to review or flush; native close may proceed.
    pub fn request(
        &mut self,
        drafts: Vec<DraftNotice>,
        pending: Vec<&'static str>,
        now: Instant,
    ) -> bool {
        if self.active() {
            return false;
        }
        if drafts.is_empty() && pending.is_empty() {
            return true;
        }
        self.drafts = drafts;
        self.pending = pending;
        self.confirm_selected = false;
        self.phase = Some(if self.drafts.is_empty() {
            Phase::Flushing
        } else {
            Phase::Review
        });
        self.started = Some(now);
        false
    }

    /// A blocked close needs a fresh, explicit choice acknowledging uncertainty.
    /// Returns true only for that deliberate override, never on a timer.
    pub fn confirm(&mut self, now: Instant) -> bool {
        match self.phase {
            Some(Phase::Blocked) => true,
            Some(Phase::Review) => {
                self.authorized = self.drafts.iter().map(|draft| draft.key.clone()).collect();
                self.phase = Some(Phase::Flushing);
                self.started = Some(now);
                self.confirm_selected = false;
                false
            }
            _ => false,
        }
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        if self.active() {
            self.phase = Some(Phase::Blocked);
            self.error = Some(message.into().chars().take(1024).collect());
            self.confirm_selected = false;
        }
    }

    pub fn cancel(&mut self) {
        *self = Self::default();
    }

    /// Observe actual ACK-driven state. New dirty documents require new consent.
    pub fn service(
        &mut self,
        drafts: Vec<DraftNotice>,
        pending: Vec<&'static str>,
        now: Instant,
    ) -> bool {
        if !self.active() {
            return false;
        }
        self.drafts = drafts;
        if self.phase == Some(Phase::Blocked) {
            for resource in pending {
                if !self.pending.contains(&resource) {
                    self.pending.push(resource);
                }
            }
        } else {
            self.pending = pending;
        }
        if self.phase != Some(Phase::Flushing) {
            return false;
        }
        if self
            .drafts
            .iter()
            .any(|draft| !self.authorized.contains(&draft.key))
        {
            self.phase = Some(Phase::Review);
            self.confirm_selected = false;
            return false;
        }
        if self.pending.is_empty() {
            return true;
        }
        if self
            .started
            .is_some_and(|start| now.saturating_duration_since(start) >= FLUSH_TIMEOUT)
        {
            self.fail("Сохранение перечисленных изменений не подтверждено за 5 секунд. Возврат оставит их в открытом приложении; закрытие не отменяет уже отправленные операции.");
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn draft(key: &str) -> DraftNotice {
        DraftNotice {
            key: key.into(),
            label: "Контекст".into(),
        }
    }
    #[test]
    fn clean_close_is_immediate_but_drafts_default_to_cancel() {
        let now = Instant::now();
        let mut flow = CloseFlow::default();
        assert!(flow.request(vec![], vec![], now));
        assert!(!flow.request(vec![draft("a")], vec![], now));
        assert_eq!(flow.phase, Some(Phase::Review));
        assert!(!flow.confirm_selected);
        assert!(!flow.request(vec![], vec![], now));
        assert!(!flow.service(vec![draft("a")], vec![], now));
        flow.cancel();
        assert!(!flow.active());
    }
    #[test]
    fn confirmation_waits_for_writes_and_new_drafts_need_new_review() {
        let now = Instant::now();
        let mut flow = CloseFlow::default();
        flow.request(vec![draft("a")], vec!["Canvas"], now);
        assert!(!flow.confirm(now));
        assert!(!flow.service(vec![draft("a")], vec!["Canvas"], now));
        assert!(!flow.service(vec![draft("a"), draft("b")], vec![], now));
        assert_eq!(flow.phase, Some(Phase::Review));
        flow.confirm(now);
        assert!(flow.service(vec![draft("a"), draft("b")], vec![], now));
    }
    #[test]
    fn timeout_never_closes_or_retries_and_error_requires_fresh_confirmation() {
        let now = Instant::now();
        let mut flow = CloseFlow::default();
        flow.request(vec![], vec!["Настройки"], now);
        assert!(!flow.service(vec![], vec!["Настройки"], now + FLUSH_TIMEOUT));
        assert_eq!(flow.phase, Some(Phase::Blocked));
        assert!(!flow.service(vec![], vec![], now + FLUSH_TIMEOUT));
        assert!(flow.confirm(now));
        assert_eq!(flow.pending, vec!["Настройки"]);
        flow.cancel();
        flow.request(vec![], vec!["Canvas"], now);
        flow.fail("disk error");
        assert!(!flow.service(vec![], vec![], now));
        assert_eq!(flow.error.as_deref(), Some("disk error"));
        assert!(flow.confirm(now));
    }
}
