//! Optional bounded numeric tracing of the UI publication path.
//!
//! Records intentionally contain no terminal content, input, paths, or errors.
//! Producers use a bounded nonblocking channel; output and formatting happen
//! only on the owned logger thread.
//!
//! Logger lines are tab-separated numeric fields. `record` contains elapsed
//! nanoseconds, stage (`1` poll-applied, `2` UI-applied, `3` paint), app
//! revision (or command request ID for stages4/5), sequence count, then at most four sequence IDs. `summary`
//! contains elapsed nanoseconds and bounded record/drop counts.

use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::Instant,
};

const QUEUE_CAPACITY: usize = 64;
const RECORD_CAPACITY: usize = 512;
const MAX_SEQUENCES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Stage {
    PollApplied = 1,
    UiApplied = 2,
    Paint = 3,
    CommandDispatch = 4,
    CommandReply = 5,
}

#[derive(Clone, Copy)]
struct Record {
    elapsed_ns: u64,
    stage: Stage,
    revision: u64,
    sequence_count: u8,
    sequences: [u64; MAX_SEQUENCES],
}

struct Counters {
    started: Instant,
    record_capacity: usize,
    reserved: AtomicUsize,
    submitted: AtomicUsize,
    dropped: AtomicU64,
    cap_dropped: AtomicU64,
    writer_failed: AtomicBool,
}

/// Cloneable handle for an optional bounded trace. Dropping the last clone
/// disconnects the logger channel, which then writes its summary and exits.
#[derive(Clone)]
pub(crate) struct Trace {
    sender: Option<SyncSender<Record>>,
    counters: Option<Arc<Counters>>,
}

impl Trace {
    pub(crate) fn disabled() -> Self {
        Self {
            sender: None,
            counters: None,
        }
    }

    /// Start the one owned stderr logger. Startup failure is returned to the
    /// caller so the opt-in diagnostic flag cannot silently claim tracing.
    pub(crate) fn enabled() -> io::Result<Self> {
        Self::start_with_writer(io::stderr(), QUEUE_CAPACITY, RECORD_CAPACITY)
    }

    fn start_with_writer<W: Write + Send + 'static>(
        writer: W,
        queue_capacity: usize,
        record_capacity: usize,
    ) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let counters = Arc::new(Counters {
            started: Instant::now(),
            record_capacity,
            reserved: AtomicUsize::new(0),
            submitted: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            cap_dropped: AtomicU64::new(0),
            writer_failed: AtomicBool::new(false),
        });
        let logger_counters = counters.clone();
        thread::Builder::new()
            .name("pigade-publication-trace".into())
            .spawn(move || logger(receiver, writer, logger_counters))?;
        Ok(Self {
            sender: Some(sender),
            counters: Some(counters),
        })
    }

    /// Add one numeric record without waiting for the logger. Only four
    /// sequence IDs are copied, regardless of caller slice length.
    pub(crate) fn record(&self, stage: Stage, revision: u64, sequences: &[u64]) {
        let (Some(sender), Some(counters)) = (&self.sender, &self.counters) else {
            return;
        };
        if counters.writer_failed.load(Ordering::Relaxed) {
            saturating_increment(&counters.dropped);
            return;
        }
        if !reserve_below(&counters.reserved, counters.record_capacity) {
            saturating_increment(&counters.cap_dropped);
            return;
        }

        let mut record = Record {
            elapsed_ns: counters
                .started
                .elapsed()
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64,
            stage,
            revision,
            sequence_count: sequences.len().min(MAX_SEQUENCES) as u8,
            sequences: [0; MAX_SEQUENCES],
        };
        record.sequences[..usize::from(record.sequence_count)]
            .copy_from_slice(&sequences[..usize::from(record.sequence_count)]);

        match sender.try_send(record) {
            Ok(()) => {
                counters.submitted.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                saturating_increment(&counters.dropped);
            }
        }
    }
}

fn reserve_below(counter: &AtomicUsize, limit: usize) -> bool {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            (value < limit).then_some(value + 1)
        })
        .is_ok()
}

fn saturating_increment(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(1))
    });
}

fn logger<W: Write>(receiver: mpsc::Receiver<Record>, mut writer: W, counters: Arc<Counters>) {
    let mut written = 0usize;
    let mut failed = false;
    while let Ok(record) = receiver.recv() {
        if failed {
            saturating_increment(&counters.dropped);
            continue;
        }
        if write_record(&mut writer, record).is_err() {
            failed = true;
            counters.writer_failed.store(true, Ordering::Relaxed);
            saturating_increment(&counters.dropped);
            continue;
        }
        written = written.saturating_add(1);
    }

    if !failed {
        let elapsed_ns = counters
            .started
            .elapsed()
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        let _ = writeln!(
            writer,
            "summary\t{elapsed_ns}\twritten={written}\tsubmitted={}\tdropped={}\tcap_dropped={}\twriter_failed=0",
            counters.submitted.load(Ordering::Relaxed),
            counters.dropped.load(Ordering::Relaxed),
            counters.cap_dropped.load(Ordering::Relaxed),
        );
    }
}

fn write_record(writer: &mut impl Write, record: Record) -> io::Result<()> {
    write!(
        writer,
        "record\t{}\t{}\t{}\t{}",
        record.elapsed_ns, record.stage as u8, record.revision, record.sequence_count,
    )?;
    for sequence in &record.sequences[..usize::from(record.sequence_count)] {
        write!(writer, "\t{sequence}")?;
    }
    writeln!(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Condvar, Mutex, mpsc::RecvTimeoutError},
        time::Duration,
    };

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<(Mutex<WriterState>, Condvar)>);

    #[derive(Default)]
    struct WriterState {
        blocked: bool,
        started: bool,
        bytes: Vec<u8>,
    }

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let (state, changed) = &*self.0;
            let mut state = state.lock().unwrap();
            if !state.started {
                state.started = true;
                changed.notify_all();
            }
            while state.blocked {
                state = changed.wait(state).unwrap();
            }
            state.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SharedWriter {
        fn block_until_first_write(&self) {
            self.0.0.lock().unwrap().blocked = true;
        }

        fn wait_for_first_write(&self) {
            let (state, changed) = &*self.0;
            let mut state = state.lock().unwrap();
            while !state.started {
                let (next, timeout) = changed.wait_timeout(state, Duration::from_secs(1)).unwrap();
                state = next;
                assert!(!timeout.timed_out(), "logger never reached the writer");
            }
        }

        fn release(&self) {
            self.0.0.lock().unwrap().blocked = false;
            self.0.1.notify_all();
        }

        fn contents(&self) -> Vec<u8> {
            self.0.0.lock().unwrap().bytes.clone()
        }
    }

    struct ReleaseOnDrop(SharedWriter);

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    #[test]
    fn disabled_trace_is_a_noop() {
        Trace::disabled().record(Stage::Paint, 7, &[1, 2]);
    }

    #[test]
    fn producer_does_not_wait_when_bounded_queue_is_full_and_records_are_capped() {
        let writer = SharedWriter::default();
        writer.block_until_first_write();
        let trace = Trace::start_with_writer(writer.clone(), 2, 4).unwrap();
        let release = ReleaseOnDrop(writer.clone());
        trace.record(Stage::PollApplied, 1, &[11]);
        writer.wait_for_first_write();
        // The first record is in the blocked writer and these two fill the
        // queue. Further record calls must remain nonblocking.
        trace.record(Stage::UiApplied, 2, &[12]);
        trace.record(Stage::Paint, 3, &[13]);
        let (done_tx, done_rx) = mpsc::channel();
        let producer = trace.clone();
        let join = thread::spawn(move || {
            producer.record(Stage::Paint, 4, &[14]);
            done_tx.send(()).unwrap();
        });
        match done_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {
                writer.release();
                join.join().unwrap();
                panic!("record waited for the logger");
            }
            Err(RecvTimeoutError::Disconnected) => panic!("producer exited unexpectedly"),
        }
        // The total acquired-record cap includes attempts that hit a full
        // queue, so a permanently slow sink cannot cause unbounded formatting.
        trace.record(Stage::PollApplied, 5, &[15]);
        drop(trace);
        drop(release);
        join.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !writer
            .contents()
            .windows(b"summary\t".len())
            .any(|window| window == b"summary\t")
        {
            assert!(Instant::now() < deadline, "logger summary never arrived");
            thread::yield_now();
        }
        let output = String::from_utf8(writer.contents()).unwrap();
        assert!(output.contains("cap_dropped=1"), "{output}");
        assert!(output.contains("dropped=1"), "{output}");
        assert!(output.contains("written=3"), "{output}");
        assert!(output.contains("\t11\n"), "{output}");
        assert!(output.contains("\t12\n"), "{output}");
        assert!(output.contains("\t13\n"), "{output}");
        assert!(!output.contains("\t14\n"), "{output}");
        assert!(!output.contains("\t15\n"), "{output}");
    }

    #[test]
    fn record_copies_at_most_four_numeric_sequences() {
        let writer = SharedWriter::default();
        let trace = Trace::start_with_writer(writer.clone(), 8, 8).unwrap();
        trace.record(Stage::Paint, 9, &[1, 2, 3, 4, 5, 6]);
        drop(trace);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !writer
            .contents()
            .windows(b"summary\t".len())
            .any(|window| window == b"summary\t")
        {
            assert!(Instant::now() < deadline, "logger summary never arrived");
            thread::yield_now();
        }
        let output = String::from_utf8(writer.contents()).unwrap();
        assert!(output.contains("record\t"));
        assert!(output.contains("\t9\t4\t1\t2\t3\t4\n"), "{output}");
        assert!(!output.contains("\t5\n"), "{output}");
    }

    struct BrokenWriter {
        attempts: Arc<AtomicUsize>,
        failed: mpsc::SyncSender<()>,
        finished: Arc<AtomicBool>,
    }

    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            let _ = self.failed.try_send(());
            Err(io::Error::other("fixture writer failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("fixture writer failure"))
        }
    }

    impl Drop for BrokenWriter {
        fn drop(&mut self) {
            self.finished.store(true, Ordering::Release);
        }
    }

    #[test]
    fn writer_failure_is_not_retried_or_propagated_to_producer() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicBool::new(false));
        let (failed_tx, failed_rx) = mpsc::sync_channel(1);
        let trace = Trace::start_with_writer(
            BrokenWriter {
                attempts: attempts.clone(),
                failed: failed_tx,
                finished: finished.clone(),
            },
            1,
            4,
        )
        .unwrap();
        trace.record(Stage::PollApplied, 1, &[1]);
        failed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("logger never attempted its first write");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !trace
            .counters
            .as_ref()
            .unwrap()
            .writer_failed
            .load(Ordering::Acquire)
        {
            assert!(
                Instant::now() < deadline,
                "logger did not record writer failure"
            );
            thread::yield_now();
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        // Once the sink has failed, producers only update the numeric drop
        // count. They cannot retry or perform writer I/O themselves.
        trace.record(Stage::UiApplied, 2, &[2]);
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert_eq!(
            trace
                .counters
                .as_ref()
                .unwrap()
                .dropped
                .load(Ordering::Relaxed),
            2
        );
        drop(trace);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !finished.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "logger did not exit after trace drop"
            );
            thread::yield_now();
        }
    }
}
