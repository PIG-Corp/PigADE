//! Stage-0 component benchmark, NOT input-to-present or full MVP acceptance.
use anyhow::{Context, Result};
use pigade_storage::{EventKind, Journal, JournalHeader, JournalReader};
use pigade_terminal::{CORE_PROFILE, Geometry, Terminal};
use serde_json::json;
use std::time::Instant;
use uuid::Uuid;

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).ceil() as usize]
}
fn stats(mut values: Vec<f64>) -> serde_json::Value {
    json!({"samples":values.len(),"p50_ms":percentile(&mut values,0.5),"p95_ms":percentile(&mut values,0.95),"p99_ms":percentile(&mut values,0.99)})
}
fn main() -> Result<()> {
    let directory = std::env::var_os("PIGADE_BENCH_DIR")
        .context("set PIGADE_BENCH_DIR to a durable filesystem directory, not tmpfs")?;
    let root = tempfile::tempdir_in(directory)?;
    let g = Geometry::new(36, 120)?;
    let line = "terminal benchmark: кириллица e\u{301} 界 \x1b[38;2;100;150;210mcolor\x1b[0m\r\n";
    let payload = line.repeat(128).into_bytes();
    let mut commit = Vec::new();
    let mut parse = Vec::new();
    let mut replay = Vec::new();
    let mut total = 0;
    let mut checkpoint_restore = Vec::new();
    let mut checkpoint_sizes = Vec::new();
    let started = Instant::now();
    for session in 0..4 {
        let path = root.path().join(format!("{session}.journal"));
        let mut journal = Journal::create(
            &path,
            JournalHeader {
                session_id: Uuid::new_v4(),
                generation: Uuid::new_v4(),
                profile: CORE_PROFILE.into(),
                initial_rows: g.rows,
                initial_cols: g.cols,
            },
        )?;
        let mut terminal = Terminal::new(g, true);
        // At least 10,000 lines per session, plus 1,000 ordered resizes.
        for index in 0..1000 {
            if index < 80 {
                let now = Instant::now();
                journal.append(EventKind::Output, &payload)?;
                commit.push(now.elapsed().as_secs_f64() * 1000.);
                let now = Instant::now();
                terminal.output(&payload);
                parse.push(now.elapsed().as_secs_f64() * 1000.);
                total += payload.len();
            }
            let size = Geometry::new(36, if index % 2 == 0 { 121 } else { 120 })?;
            journal.append(EventKind::Resize, &serde_json::to_vec(&size)?)?;
            terminal.resize(size);
        }
        let frame = terminal.frame();
        let grid = terminal.checkpoint()?;
        checkpoint_sizes.push(grid.len());
        for _ in 0..10 {
            let now = Instant::now();
            let restored = Terminal::from_checkpoint(&grid, false)?;
            anyhow::ensure!(restored.frame() == frame, "checkpoint frame mismatch");
            checkpoint_restore.push(now.elapsed().as_secs_f64() * 1000.);
            anyhow::ensure!(
                restored.checkpoint()? == grid,
                "complete checkpoint mismatch"
            );
        }
        drop(journal);
        for _ in 0..10 {
            let now = Instant::now();
            let mut reader = JournalReader::open(&path)?;
            let mut recovered = Terminal::new(g, false);
            while let Some(event) = reader.next_event()? {
                match event.kind {
                    EventKind::Output => recovered.output(&event.payload),
                    EventKind::Resize => recovered.resize(serde_json::from_slice(&event.payload)?),
                    EventKind::SyncTimeout => recovered.sync_timeout(),
                    EventKind::Exit => (),
                    EventKind::OutputRejectedResource => {
                        anyhow::bail!("benchmark cannot apply a resource-refused suffix")
                    }
                }
            }
            let same = frame == recovered.frame() && grid == recovered.checkpoint()?;
            anyhow::ensure!(same, "model replay mismatch");
            replay.push(now.elapsed().as_secs_f64() * 1000.);
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "kind":"stage-0 component benchmark; release; warm filesystem cache; no UI/GPU",
            "core_profile":CORE_PROFILE,"sessions":4,"lines_per_session":10240,"resizes_per_session":1000,
            "output_bytes":total,"checkpoint_bytes_per_session":checkpoint_sizes,"single_session_checkpoint_restore_plus_frame":stats(checkpoint_restore),"durable_output_commit":stats(commit),"parse_output_chunk":stats(parse),
            "single_session_full_replay_with_frame_and_grid_comparison":stats(replay),
            "elapsed_seconds":started.elapsed().as_secs_f64(),
            "not_measured":["input-to-present","frame_time","cold_start","long_history_checkpoint","24h_soak","GPU_memory","VM_reboot"]
        }))?
    );
    Ok(())
}
