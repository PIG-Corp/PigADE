//! Bounded receipts for optimistic mouse state sent through the IPC worker.
//!
//! An Accepted receipt means the host admitted the request into its ordered
//! input-writer path. It does not claim that the child process consumed bytes.
use crate::{command_worker::CommandTarget, mouse};
use std::collections::{HashMap, HashSet, VecDeque};

pub(crate) const MAX_PENDING: usize = 32;
const MAX_TRACKED_TARGETS: usize = 32;
const MAX_UNKNOWN_TARGETS: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MouseSnapshot {
    pub encoder: mouse::Encoder,
    pub position: mouse::Position,
    pub wheel: mouse::WheelAccumulator,
    pub rows: u16,
    pub cols: u16,
    pub modes: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Accepted,
    NotAdmitted,
    Unknown,
    Dropped,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RequestReceipt {
    pub id: u64,
    pub target: CommandTarget,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq)]
struct Pending {
    id: u64,
    target: CommandTarget,
    mouse_after: Option<MouseSnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ReceiptEffect {
    None,
    Rollback {
        target: CommandTarget,
        confirmed: Option<MouseSnapshot>,
        unknown: bool,
    },
}

#[derive(Default)]
pub(crate) struct MouseDelivery {
    pending: VecDeque<Pending>,
    confirmed: HashMap<CommandTarget, MouseSnapshot>,
    cleanup_waiting: HashMap<CommandTarget, (u64, u64)>,
    unknown: HashSet<CommandTarget>,
    unknown_overflow: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cleanup {
    pub id: u64,
    pub bytes: Vec<u8>,
    pub mouse_after: MouseSnapshot,
}

impl MouseDelivery {
    pub fn stage(
        &mut self,
        id: u64,
        target: CommandTarget,
        mouse_after: Option<MouseSnapshot>,
    ) -> bool {
        if self.pending.len() == MAX_PENDING || self.pending.iter().any(|pending| pending.id == id)
        {
            return false;
        }
        if mouse_after
            .as_ref()
            .is_some_and(|snapshot| snapshot.encoder.has_pressed())
            && !self.confirmed.contains_key(&target)
            && !self.pending.iter().any(|pending| {
                pending.target == target
                    && pending
                        .mouse_after
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.encoder.has_pressed())
            })
        {
            let pending_targets = self
                .pending
                .iter()
                .filter(|pending| {
                    pending
                        .mouse_after
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.encoder.has_pressed())
                })
                .map(|pending| pending.target.clone())
                .collect::<HashSet<_>>();
            let reserved = self.confirmed.len()
                + pending_targets
                    .iter()
                    .filter(|pending_target| !self.confirmed.contains_key(*pending_target))
                    .count();
            if reserved >= MAX_TRACKED_TARGETS {
                return false;
            }
        }
        self.pending.push_back(Pending {
            id,
            target,
            mouse_after,
        });
        true
    }

    pub fn cancel_stage(&mut self, id: u64) {
        self.pending.retain(|pending| pending.id != id);
    }
    pub fn has_capacity(&self) -> bool {
        self.pending.len() < MAX_PENDING
    }
    pub fn has_motion_capacity(&self) -> bool {
        // Intermediate pointer positions need not consume the slots reserved
        // for keys, resize and button transitions. Admitted events stay FIFO.
        self.pending.len() < MAX_PENDING - 8
    }
    pub fn awaiting_cleanup(&self, target: &CommandTarget) -> bool {
        self.cleanup_waiting.contains_key(target)
    }
    pub fn confirmed_pressed(&self, target: &CommandTarget) -> bool {
        self.confirmed
            .get(target)
            .is_some_and(|snapshot| snapshot.encoder.has_pressed())
    }
    pub fn settled_through(&self, target: &CommandTarget, request_id: u64) -> bool {
        !self
            .pending
            .iter()
            .any(|pending| pending.target == *target && pending.id <= request_id)
    }

    pub fn request_receipt(&mut self, receipt: RequestReceipt) -> ReceiptEffect {
        let pending = self
            .pending
            .iter()
            .position(|item| item.id == receipt.id && item.target == receipt.target)
            .map(|position| self.pending.remove(position).expect("position exists"));
        if self
            .cleanup_waiting
            .get(&receipt.target)
            .is_some_and(|(_, id)| *id == receipt.id)
        {
            self.cleanup_waiting.remove(&receipt.target);
        }
        match receipt.outcome {
            Outcome::Accepted => {
                if let Some(mouse_after) = pending.and_then(|item| item.mouse_after) {
                    if mouse_after.encoder.has_pressed() {
                        self.confirmed.insert(receipt.target, mouse_after);
                    } else {
                        self.confirmed.remove(&receipt.target);
                    }
                }
                ReceiptEffect::None
            }
            Outcome::NotAdmitted | Outcome::Unknown => {
                if receipt.outcome == Outcome::Unknown {
                    let has_mouse_state = self.confirmed.contains_key(&receipt.target)
                        || pending.as_ref().is_some_and(|item| {
                            item.mouse_after
                                .as_ref()
                                .is_some_and(|snapshot| snapshot.encoder.has_pressed())
                        })
                        || self.pending.iter().any(|item| {
                            item.target == receipt.target
                                && item
                                    .mouse_after
                                    .as_ref()
                                    .is_some_and(|snapshot| snapshot.encoder.has_pressed())
                        });
                    if has_mouse_state {
                        if self.unknown.len() < MAX_UNKNOWN_TARGETS
                            || self.unknown.contains(&receipt.target)
                        {
                            self.unknown.insert(receipt.target.clone());
                        } else {
                            self.unknown_overflow = true;
                        }
                    }
                }
                ReceiptEffect::Rollback {
                    target: receipt.target.clone(),
                    confirmed: self.confirmed.get(&receipt.target).cloned(),
                    unknown: receipt.outcome == Outcome::Unknown
                        || self.unknown.contains(&receipt.target)
                        || self.unknown_overflow,
                }
            }
            Outcome::Dropped => {
                // Keep later requests staged until their individual FIFO
                // receipts arrive; a failure does not prove the UI consumed them.
                self.pending.retain(|item| item.id != receipt.id);
                ReceiptEffect::Rollback {
                    target: receipt.target.clone(),
                    confirmed: self.confirmed.get(&receipt.target).cloned(),
                    unknown: self.unknown.contains(&receipt.target) || self.unknown_overflow,
                }
            }
        }
    }

    /// Prepare a release from the last worker-confirmed encoder, using only the
    /// current acknowledged geometry/modes for this exact target.
    pub fn prepare_cleanup(
        &self,
        target: &CommandTarget,
        id: u64,
        rows: u16,
        cols: u16,
        modes: u32,
    ) -> Result<Option<Cleanup>, ()> {
        let Some(confirmed) = self.confirmed.get(target) else {
            return Ok(None);
        };
        if self.unknown.contains(target) || self.unknown_overflow {
            return Err(());
        }
        if !confirmed.encoder.has_pressed() {
            return Ok(None);
        }
        if rows == 0 || cols == 0 {
            return Err(());
        }
        let mut after = confirmed.clone();
        after.rows = rows;
        after.cols = cols;
        after.modes = modes;
        after.position.row = after.position.row.min(usize::from(rows - 1));
        after.position.column = after.position.column.min(usize::from(cols - 1));
        let Some(bytes) =
            after
                .encoder
                .release_all(after.position, mouse::Modifiers::default(), modes)
        else {
            return Err(());
        };
        Ok(Some(Cleanup {
            id,
            bytes,
            mouse_after: after,
        }))
    }

    pub fn reserve_cleanup(
        &mut self,
        target: CommandTarget,
        token: u64,
        id: u64,
        mouse_after: MouseSnapshot,
    ) -> bool {
        if self.cleanup_waiting.contains_key(&target) || !self.has_capacity() {
            return false;
        }
        self.cleanup_waiting.insert(target.clone(), (token, id));
        if !self.stage(id, target.clone(), Some(mouse_after)) {
            self.cleanup_waiting.remove(&target);
            return false;
        }
        true
    }

    pub fn cancel_cleanup(&mut self, target: &CommandTarget, id: u64) {
        if self
            .cleanup_waiting
            .get(target)
            .is_some_and(|(_, pending_id)| *pending_id == id)
        {
            self.cleanup_waiting.remove(target);
        }
        self.cancel_stage(id);
    }

    pub fn finish_ack(
        &mut self,
        target: &CommandTarget,
        token: u64,
        id: Option<u64>,
        accepted: bool,
    ) {
        if let Some(id) = id
            && self.cleanup_waiting.get(target) == Some(&(token, id))
            && !accepted
        {
            self.cancel_cleanup(target, id);
        }
    }

    pub fn remove_ended(&mut self, target: &CommandTarget) {
        self.confirmed.remove(target);
        self.unknown.remove(target);
        self.cleanup_waiting.remove(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_worker::{Dispatch, RequestFailure, WorkerGate};

    fn target(name: &str) -> CommandTarget {
        CommandTarget {
            session: name.into(),
            generation: format!("{name}-gen"),
        }
    }
    fn snapshot(pressed: bool) -> MouseSnapshot {
        let mut encoder = mouse::Encoder::default();
        if pressed {
            encoder.press(
                mouse::Button::Left,
                mouse::Position { row: 9, column: 19 },
                mouse::Modifiers::default(),
                mouse::MOUSE_REPORT_CLICK | mouse::SGR_MOUSE,
            );
        }
        MouseSnapshot {
            encoder,
            position: mouse::Position { row: 9, column: 19 },
            wheel: mouse::WheelAccumulator::default(),
            rows: 10,
            cols: 20,
            modes: mouse::MOUSE_REPORT_CLICK | mouse::SGR_MOUSE,
        }
    }
    fn receipt(id: u64, target: CommandTarget, outcome: Outcome) -> RequestReceipt {
        RequestReceipt {
            id,
            target,
            outcome,
        }
    }

    #[test]
    fn known_rejected_release_rolls_back_then_prepares_clamped_cleanup() {
        let a = target("a");
        let mut delivery = MouseDelivery::default();
        assert!(delivery.stage(1, a.clone(), Some(snapshot(true))));
        delivery.request_receipt(receipt(1, a.clone(), Outcome::Accepted));
        assert!(delivery.stage(2, a.clone(), Some(snapshot(false))));
        let effect = delivery.request_receipt(receipt(2, a.clone(), Outcome::NotAdmitted));
        assert!(
            matches!(effect, ReceiptEffect::Rollback { confirmed: Some(ref s), .. } if s.encoder.has_pressed())
        );
        let cleanup = delivery
            .prepare_cleanup(&a, 3, 4, 5, mouse::MOUSE_REPORT_CLICK | mouse::SGR_MOUSE)
            .unwrap()
            .unwrap();
        assert_eq!(cleanup.bytes, b"\x1b[<0;5;4m");
        assert_eq!(
            cleanup.mouse_after.position,
            mouse::Position { row: 3, column: 4 }
        );
        assert!(!cleanup.mouse_after.encoder.has_pressed());
    }

    #[test]
    fn failed_press_does_not_invent_release_and_unknown_release_is_distinct() {
        let a = target("a");
        let mut delivery = MouseDelivery::default();
        assert!(delivery.stage(1, a.clone(), Some(snapshot(true))));
        let effect = delivery.request_receipt(receipt(1, a.clone(), Outcome::NotAdmitted));
        assert!(matches!(
            effect,
            ReceiptEffect::Rollback {
                confirmed: None,
                ..
            }
        ));
        assert!(
            delivery
                .prepare_cleanup(&a, 2, 10, 20, 40)
                .unwrap()
                .is_none()
        );
        assert!(delivery.stage(3, a.clone(), Some(snapshot(true))));
        delivery.request_receipt(receipt(3, a.clone(), Outcome::Accepted));
        assert!(delivery.stage(4, a.clone(), Some(snapshot(false))));
        let effect = delivery.request_receipt(receipt(4, a.clone(), Outcome::Unknown));
        assert!(matches!(
            effect,
            ReceiptEffect::Rollback { unknown: true, .. }
        ));
        assert!(delivery.stage(5, a.clone(), Some(snapshot(false))));
        let effect = delivery.request_receipt(receipt(5, a.clone(), Outcome::Dropped));
        assert!(matches!(
            effect,
            ReceiptEffect::Rollback { unknown: true, .. }
        ));
        assert!(delivery.prepare_cleanup(&a, 5, 10, 20, 40).is_err());
    }

    #[test]
    fn known_failure_waits_for_receipts_of_later_dropped_requests() {
        let a = target("barrier");
        let mut delivery = MouseDelivery::default();
        assert!(delivery.stage(2, a.clone(), Some(snapshot(true))));
        assert!(delivery.stage(3, a.clone(), Some(snapshot(false))));
        delivery.request_receipt(receipt(2, a.clone(), Outcome::NotAdmitted));
        assert!(!delivery.settled_through(&a, 3));
        delivery.request_receipt(receipt(3, a.clone(), Outcome::Dropped));
        assert!(delivery.settled_through(&a, 3));
    }

    #[test]
    fn unknown_bookkeeping_is_bounded_and_ignores_keyboard_only_targets() {
        let mut delivery = MouseDelivery::default();
        let protected = target("protected-press");
        assert!(delivery.stage(1_000, protected.clone(), Some(snapshot(true))));
        delivery.request_receipt(receipt(1_000, protected.clone(), Outcome::Accepted));
        for index in 0..MAX_UNKNOWN_TARGETS {
            let target = target(&format!("unknown-{index}"));
            let id = index as u64 + 1;
            assert!(delivery.stage(id, target.clone(), Some(snapshot(true))));
            delivery.request_receipt(receipt(id, target, Outcome::Unknown));
        }
        assert_eq!(delivery.unknown.len(), MAX_UNKNOWN_TARGETS);
        assert!(!delivery.unknown_overflow);

        let keyboard_only = target("keyboard-only");
        assert!(delivery.stage(2_000, keyboard_only.clone(), None));
        delivery.request_receipt(receipt(2_000, keyboard_only.clone(), Outcome::Unknown));
        assert!(!delivery.unknown.contains(&keyboard_only));

        let overflow = target("overflow");
        assert!(delivery.stage(2_001, overflow.clone(), Some(snapshot(true))));
        let effect = delivery.request_receipt(receipt(2_001, overflow.clone(), Outcome::Unknown));
        assert!(matches!(
            effect,
            ReceiptEffect::Rollback { unknown: true, .. }
        ));
        assert!(delivery.unknown_overflow);
        assert!(delivery.unknown.len() <= MAX_UNKNOWN_TARGETS);
        assert!(
            delivery
                .prepare_cleanup(&protected, 2_002, 4, 5, 40)
                .is_err()
        );
    }

    #[test]
    fn receipts_are_bounded_and_other_targets_keep_confirmed_state() {
        let a = target("a");
        let b = target("b");
        let mut delivery = MouseDelivery::default();
        for id in 1..=MAX_PENDING as u64 {
            assert!(delivery.stage(id, a.clone(), None));
        }
        assert!(!delivery.stage(33, b.clone(), None));
        delivery.request_receipt(receipt(1, a.clone(), Outcome::Accepted));
        assert!(delivery.stage(33, b.clone(), Some(snapshot(true))));
        delivery.request_receipt(receipt(33, b.clone(), Outcome::Accepted));
        let effect = delivery.request_receipt(receipt(2, a.clone(), Outcome::NotAdmitted));
        assert!(matches!(effect, ReceiptEffect::Rollback { target, .. } if target == a));
        assert!(delivery.confirmed.get(&b).unwrap().encoder.has_pressed());
    }

    #[test]
    fn confirmed_pressed_targets_are_bounded() {
        let mut delivery = MouseDelivery::default();
        for index in 0..MAX_TRACKED_TARGETS as u64 {
            let target = target(&index.to_string());
            assert!(delivery.stage(index + 1, target.clone(), Some(snapshot(true))));
            delivery.request_receipt(receipt(index + 1, target, Outcome::Accepted));
        }
        assert_eq!(delivery.confirmed.len(), MAX_TRACKED_TARGETS);
        let overflow = target("overflow");
        assert!(!delivery.stage(100, overflow, Some(snapshot(true))));
    }

    #[test]
    fn stale_ack_cannot_keep_cleanup_reservation() {
        let a = target("a");
        let mut delivery = MouseDelivery::default();
        assert!(delivery.reserve_cleanup(a.clone(), 7, 12, snapshot(false)));
        delivery.finish_ack(&a, 7, Some(12), false);
        assert!(!delivery.awaiting_cleanup(&a));
        assert!(delivery.stage(12, a, None));
    }

    #[test]
    fn worker_dispatch_and_receipt_ledger_reconcile_release_before_ack_cleanup() {
        let a = target("a");
        let mut gate = WorkerGate::default();
        let mut delivery = MouseDelivery::default();
        let mut sent_cleanup = Vec::new();

        assert!(delivery.stage(1, a.clone(), Some(snapshot(true))));
        assert_eq!(
            gate.dispatch_request_with_id(a.clone(), None, 1, || Ok(())),
            Dispatch::Executed
        );
        delivery.request_receipt(receipt(1, a.clone(), Outcome::Accepted));

        assert!(delivery.stage(2, a.clone(), Some(snapshot(false))));
        assert_eq!(
            gate.dispatch_request_with_id(a.clone(), None, 2, || {
                Err(RequestFailure::NotAdmitted("full".into()))
            }),
            Dispatch::RejectedKnown
        );
        let effect = delivery.request_receipt(receipt(2, a.clone(), Outcome::NotAdmitted));
        assert!(
            matches!(effect, ReceiptEffect::Rollback { confirmed: Some(ref state), .. } if state.encoder.has_pressed())
        );

        // The latest notice watermark is settled only after its request receipt.
        let notice = gate.notice(&a).unwrap().clone();
        assert_eq!(notice.last_request_id, 2);
        assert!(delivery.settled_through(&a, notice.last_request_id));
        let cleanup = delivery
            .prepare_cleanup(&a, 3, 4, 5, mouse::MOUSE_REPORT_CLICK | mouse::SGR_MOUSE)
            .unwrap()
            .unwrap();
        assert!(delivery.reserve_cleanup(
            a.clone(),
            notice.token,
            cleanup.id,
            cleanup.mouse_after.clone(),
        ));

        // Stale ACKs must not run a release closure.
        assert!(!gate.acknowledge(&a, notice.token + 1));
        assert!(sent_cleanup.is_empty());
        assert!(gate.acknowledge(&a, notice.token));
        let outcome = gate.dispatch_request_with_id(a.clone(), None, cleanup.id, || {
            sent_cleanup.push(cleanup.bytes.clone());
            Ok(())
        });
        assert_eq!(outcome, Dispatch::Executed);
        delivery.request_receipt(RequestReceipt {
            id: cleanup.id,
            target: a.clone(),
            outcome: Outcome::Accepted,
        });
        delivery.finish_ack(&a, notice.token, Some(cleanup.id), true);

        assert_eq!(sent_cleanup, [b"\x1b[<0;5;4m".to_vec()]);
        assert!(!delivery.confirmed_pressed(&a));
        assert!(delivery.stage(4, a.clone(), None));
        assert_eq!(
            gate.dispatch_request_with_id(a.clone(), None, 4, || Ok(())),
            Dispatch::Executed
        );
    }
}
