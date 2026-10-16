//! Bounded numeric diagnostics only; never input bytes or terminal contents.
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionMetrics {
    pub events: u64,
    pub output_bytes: u64,
    pub journal_fsync_ns: u64,
    pub model_apply_ns: u64,
    pub archive_commit_ns: u64,
    pub checkpoint_ns: u64,
    pub frame_publish_ns: u64,
    pub checkpoints: u64,
}
pub(crate) struct EventCost {
    pub output_bytes: usize,
    pub journal: Duration,
    pub model: Duration,
    pub archive: Duration,
    pub checkpoint: Duration,
    pub frame: Duration,
    pub checkpointed: bool,
}
fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}
impl SessionMetrics {
    pub(crate) fn event(&mut self, cost: EventCost) {
        self.events = self.events.saturating_add(1);
        self.output_bytes = self.output_bytes.saturating_add(cost.output_bytes as u64);
        self.journal_fsync_ns = self.journal_fsync_ns.saturating_add(nanos(cost.journal));
        self.model_apply_ns = self.model_apply_ns.saturating_add(nanos(cost.model));
        self.archive_commit_ns = self.archive_commit_ns.saturating_add(nanos(cost.archive));
        self.checkpoint_ns = self.checkpoint_ns.saturating_add(nanos(cost.checkpoint));
        self.frame_publish_ns = self.frame_publish_ns.saturating_add(nanos(cost.frame));
        self.checkpoints = self
            .checkpoints
            .saturating_add(u64::from(cost.checkpointed));
    }
    pub(crate) fn idle_checkpoint(&mut self, elapsed: Duration) {
        self.checkpoints = self.checkpoints.saturating_add(1);
        self.checkpoint_ns = self.checkpoint_ns.saturating_add(nanos(elapsed));
    }
}
