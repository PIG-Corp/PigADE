//! Storage-only durable append latency. This is not input-to-present latency.
use anyhow::{Result, ensure};
use pigade_storage::{EventKind, Journal, JournalHeader, scan_valid_prefix};
use std::{path::PathBuf, time::Instant};
use uuid::Uuid;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("usage: journal_bench NEW_PATH [events]"));
    let events: usize = args.next().unwrap_or_else(|| "1000".into()).parse()?;
    ensure!((1..=100_000).contains(&events), "events must be 1..100000");
    ensure!(args.next().is_none(), "unexpected argument");
    let header = JournalHeader {
        session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        profile: "storage-benchmark-v1;not-a-terminal-profile".into(),
        initial_rows: 36,
        initial_cols: 120,
    };
    let mut journal = Journal::create(&path, header)?;
    let payload = [b'x'; 16 * 1024];
    let mut latencies = Vec::with_capacity(events);
    let begin = Instant::now();
    for _ in 0..events {
        let start = Instant::now();
        journal.append(EventKind::Output, &payload)?;
        latencies.push(start.elapsed().as_nanos() as u64);
    }
    let elapsed = begin.elapsed();
    drop(journal);
    latencies.sort_unstable();
    let percentile = |p: usize| {
        latencies[((events * p).div_ceil(100))
            .saturating_sub(1)
            .min(events - 1)]
    };
    let scan_begin = Instant::now();
    let report = scan_valid_prefix(&path)?;
    let scan_elapsed = scan_begin.elapsed();
    ensure!(
        report.corruption.is_none() && report.valid_events == events as u64,
        "benchmark replay mismatch"
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "metric": "storage-only durable append; fsync per 16KiB event",
            "events":events,"payload_bytes_per_event":payload.len(),
            "elapsed_ns":elapsed.as_nanos(),"p50_ns":percentile(50),"p95_ns":percentile(95),"p99_ns":percentile(99),
            "scan_elapsed_ns":scan_elapsed.as_nanos(),"valid_events":report.valid_events,"journal_bytes":report.valid_bytes,
            "filesystem_path":path,
            "not_measured":["PTY latency","UI present","GPU","checkpoint recovery","power-loss durability"]
        }))?
    );
    Ok(())
}
