//! Bounded numeric diagnostics only; never input bytes or terminal contents.
use serde::{Deserialize, Serialize};
use std::{sync::Mutex, time::Duration};

/// Bounded, content-free observation of the live actor's current pipeline stage.
/// The candidate sequence is an intended next journal sequence, not proof that
/// the event became durable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ActorDiagnostic {
    pub version: u32,
    pub sample_available: bool,
    pub stage: Option<ActorStage>,
    pub candidate_sequence: Option<u64>,
    pub entered_monotonic_ns: Option<u64>,
    pub reader: ReaderDiagnostic,
}

impl Default for ActorDiagnostic {
    fn default() -> Self {
        Self {
            version: 1,
            sample_available: false,
            stage: None,
            candidate_sequence: None,
            entered_monotonic_ns: None,
            reader: ReaderDiagnostic::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorStage {
    Waiting,
    PtyResize,
    JournalAppend,
    HistoryBegin,
    ModelApply,
    HistoryCommit,
    Checkpoint,
    FrameBuild,
    Publish,
    QueryResponses,
    Exited,
    Degraded,
}

/// Cumulative reader progress. These fields never contain PTY bytes or hashes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderDiagnostic {
    pub read_calls: u64,
    pub bytes_read: u64,
    pub last_read_bytes: u64,
    pub last_read_monotonic_ns: Option<u64>,
    pub enqueued_calls: u64,
    pub bytes_enqueued: u64,
    pub last_enqueued_monotonic_ns: Option<u64>,
}

/// Shared probe. Callers hold its mutex only while copying/updating fixed-size
/// counters; never hold it across PTY, parser, history, or filesystem work.
#[derive(Debug, Default)]
pub(crate) struct ActorProbe(Mutex<ActorDiagnostic>);

impl ActorProbe {
    pub(crate) fn enter(&self, stage: ActorStage, candidate_sequence: Option<u64>) {
        let mut sample = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        sample.version = 1;
        sample.sample_available = true;
        sample.stage = Some(stage);
        sample.candidate_sequence = candidate_sequence;
        sample.entered_monotonic_ns = monotonic_ns();
    }

    pub(crate) fn reader_read(&self, bytes: usize) {
        let mut sample = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        sample.sample_available = true;
        sample.reader.read_calls = sample.reader.read_calls.saturating_add(1);
        sample.reader.bytes_read = sample.reader.bytes_read.saturating_add(bytes as u64);
        sample.reader.last_read_bytes = bytes as u64;
        sample.reader.last_read_monotonic_ns = monotonic_ns();
    }

    pub(crate) fn reader_enqueued(&self, bytes: usize) {
        let mut sample = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        sample.sample_available = true;
        sample.reader.enqueued_calls = sample.reader.enqueued_calls.saturating_add(1);
        sample.reader.bytes_enqueued = sample.reader.bytes_enqueued.saturating_add(bytes as u64);
        sample.reader.last_enqueued_monotonic_ns = monotonic_ns();
    }

    pub(crate) fn try_snapshot(&self) -> ActorDiagnostic {
        match self.0.try_lock() {
            Ok(sample) => sample.clone(),
            Err(std::sync::TryLockError::Poisoned(_)) => ActorDiagnostic::default(),
            Err(std::sync::TryLockError::WouldBlock) => ActorDiagnostic::default(),
        }
    }
}

/// Linux CLOCK_MONOTONIC shares the clock domain used by the Python soak's
/// time.monotonic_ns sidechannel. A failed clock read remains explicitly absent.
fn monotonic_ns() -> Option<u64> {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: clock_gettime initializes the provided timespec on success.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, time.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: the successful call above initialized the timespec.
    let time = unsafe { time.assume_init() };
    let sec = u64::try_from(time.tv_sec).ok()?;
    let nsec = u64::try_from(time.tv_nsec).ok()?;
    sec.checked_mul(1_000_000_000)?.checked_add(nsec)
}

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
    }
    pub(crate) fn idle_checkpoint(&mut self, elapsed: Duration) {
        self.checkpoint_ns = self.checkpoint_ns.saturating_add(nanos(elapsed));
    }
}

#[cfg(test)]
mod actor_probe_tests {
    use super::*;

    #[test]
    fn contended_sample_fails_fast_and_identifies_unavailable_data() {
        let probe = ActorProbe::default();
        let _guard = probe.0.lock().unwrap();
        let sample = probe.try_snapshot();
        assert_eq!(sample.version, 1);
        assert!(!sample.sample_available);
        assert!(sample.stage.is_none());
        assert!(sample.entered_monotonic_ns.is_none());
    }

    #[test]
    fn poisoned_probe_is_not_reported_as_a_healthy_sample() {
        let probe = ActorProbe::default();
        probe.enter(ActorStage::JournalAppend, Some(8));
        let result = std::panic::catch_unwind(|| {
            let _guard = probe.0.lock().unwrap();
            panic!("synthetic diagnostic update failure");
        });
        assert!(result.is_err());
        assert!(!probe.try_snapshot().sample_available);
    }

    #[test]
    fn reader_counters_are_numeric_saturating_and_content_free() {
        let probe = ActorProbe::default();
        probe.reader_read(13);
        probe.reader_enqueued(13);
        let sample = probe.try_snapshot();
        assert!(sample.sample_available);
        assert_eq!(sample.reader.read_calls, 1);
        assert_eq!(sample.reader.bytes_read, 13);
        assert_eq!(sample.reader.last_read_bytes, 13);
        assert_eq!(sample.reader.enqueued_calls, 1);
        assert_eq!(sample.reader.bytes_enqueued, 13);
        assert!(sample.reader.last_read_monotonic_ns.is_some());
        assert!(sample.reader.last_enqueued_monotonic_ns.is_some());

        {
            let mut state = probe.0.lock().unwrap();
            state.reader.bytes_read = u64::MAX;
        }
        probe.reader_read(1);
        assert_eq!(probe.try_snapshot().reader.bytes_read, u64::MAX);
    }
}
