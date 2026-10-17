//! Actual-Term diagnostic for parser callback census cost and state preservation.
//!
//! This is a synthetic release-mode diagnostic, not an admission bound, RSS
//! measurement, or production performance claim. It measures parser cloning
//! plus callback counting on one local process with no terminal output retained.
use std::hint::black_box;
use std::time::Instant;

use anyhow::{Result, ensure};
use pigade_terminal::{Geometry, ResourceCensus, Terminal};
use serde_json::{Value, json};

const SAMPLES: usize = 30;

fn parser_usage(terminal: &Terminal) -> Value {
    let usage = terminal.parser_buffer_usage();
    json!({
        "processor_inline_bytes_includes_parser": usage.inline_size_bytes,
        "sync_len": usage.sync_buffer_len,
        "sync_capacity": usage.sync_buffer_capacity,
        "sync_inline_capacity": usage.sync_buffer_inline_capacity,
        "osc_len": usage.parser.pending_osc_len,
        "osc_heap_capacity": usage.parser.pending_osc_heap_capacity,
        "osc_inline_capacity_already_in_processor_size": usage.parser.pending_osc_inline_capacity,
    })
}

fn callback_counts(census: ResourceCensus) -> Value {
    json!({
        "input_chars": census.input_chars,
        "zero_width_chars": census.zero_width_chars,
        "title_set_callbacks": census.title_set_callbacks,
        "title_bytes": census.title_bytes,
        "hyperlink_updates": census.hyperlink_updates,
        "hyperlink_id_bytes": census.hyperlink_id_bytes,
        "hyperlink_uri_bytes": census.hyperlink_uri_bytes,
        "linefeed_callbacks": census.linefeed_callbacks,
        "newline_callbacks": census.newline_callbacks,
        "scroll_up_callbacks": census.scroll_up_callbacks,
        "scroll_up_requested_lines": census.scroll_up_requested_lines,
        "clear_screen_callbacks": census.clear_screen_callbacks,
        "insert_blank_lines_requested": census.insert_blank_lines_requested,
        "bell_callbacks": census.bell_callbacks,
        "cursor_style_callbacks": census.cursor_style_callbacks,
        "private_mode_set_callbacks": census.private_mode_set_callbacks,
        "private_mode_unset_callbacks": census.private_mode_unset_callbacks,
        "clipboard_store_callbacks": census.clipboard_store_callbacks,
        "saturated": census.saturated,
    })
}

fn percentile(samples: &[u64], percentile: usize) -> u64 {
    samples[(samples.len() * percentile).div_ceil(100) - 1]
}

fn measure_output(
    name: &str,
    terminal: &Terminal,
    input_len: usize,
    candidate: &[u8],
) -> Result<Value> {
    ensure!(
        candidate.len() == input_len,
        "fixture input length mismatch"
    );
    let checkpoint = terminal.checkpoint()?;
    let frame = terminal.frame();
    let buffers = parser_usage(terminal);
    let mut samples = Vec::with_capacity(SAMPLES);
    let mut result = ResourceCensus::default();
    for _ in 0..SAMPLES {
        let started = Instant::now();
        result = terminal.output_resource_census(black_box(candidate));
        samples.push(started.elapsed().as_nanos().min(u64::MAX as u128) as u64);
        black_box(result);
    }
    samples.sort_unstable();
    ensure!(
        terminal.checkpoint()? == checkpoint,
        "output census changed checkpoint"
    );
    ensure!(terminal.frame() == frame, "output census changed frame");
    ensure!(
        parser_usage(terminal) == buffers,
        "output census changed parser buffers"
    );
    Ok(json!({
        "name": name,
        "input_bytes": input_len,
        "operation": "output_resource_census",
        "callback_counts": callback_counts(result),
        "parser_buffer_usage_before": buffers,
        "checkpoint_unchanged": true,
        "frame_unchanged": true,
        "parser_buffers_unchanged": true,
        "elapsed_ns": {
            "samples": samples,
            "p50": percentile(&samples, 50),
            "p95": percentile(&samples, 95),
            "p99": percentile(&samples, 99),
            "method": "nearest-rank",
        },
    }))
}

fn measure_sync_timeout(
    name: &str,
    terminal: &Terminal,
    pending_input_len: usize,
) -> Result<Value> {
    ensure!(
        terminal.sync_timeout_pending(),
        "sync timeout fixture is not pending"
    );
    let checkpoint = terminal.checkpoint()?;
    let frame = terminal.frame();
    let buffers = parser_usage(terminal);
    let mut samples = Vec::with_capacity(SAMPLES);
    let mut result = ResourceCensus::default();
    for _ in 0..SAMPLES {
        let started = Instant::now();
        result = terminal.sync_timeout_resource_census();
        samples.push(started.elapsed().as_nanos().min(u64::MAX as u128) as u64);
        black_box(result);
    }
    samples.sort_unstable();
    ensure!(
        terminal.checkpoint()? == checkpoint,
        "timeout census changed checkpoint"
    );
    ensure!(terminal.frame() == frame, "timeout census changed frame");
    ensure!(
        parser_usage(terminal) == buffers,
        "timeout census changed parser buffers"
    );
    ensure!(
        terminal.sync_timeout_pending(),
        "timeout census consumed pending timeout"
    );
    Ok(json!({
        "name": name,
        "input_bytes_to_enter_pending_sync": pending_input_len,
        "operation": "sync_timeout_resource_census",
        "callback_counts": callback_counts(result),
        "parser_buffer_usage_before": buffers,
        "checkpoint_unchanged": true,
        "frame_unchanged": true,
        "parser_buffers_unchanged": true,
        "timeout_still_pending": true,
        "elapsed_ns": {
            "samples": samples,
            "p50": percentile(&samples, 50),
            "p95": percentile(&samples, 95),
            "p99": percentile(&samples, 99),
            "method": "nearest-rank",
        },
    }))
}

fn fresh() -> Result<Terminal> {
    Ok(Terminal::new(Geometry::new(24, 80)?, false))
}

fn run() -> Result<Value> {
    let mut results = Vec::new();

    let terminal = fresh()?;
    let plain = vec![b'P'; 64 * 1024];
    results.push(measure_output(
        "plain-64-KiB",
        &terminal,
        plain.len(),
        &plain,
    )?);

    let terminal = fresh()?;
    let csi = b"\x1b[3S\x1b[2J\x1b[4L\x1b[5M";
    let mut scroll_clear = Vec::with_capacity(csi.len() * 8192);
    for _ in 0..8192 {
        scroll_clear.extend_from_slice(csi);
    }
    results.push(measure_output(
        "many-scroll-clear-no-printable",
        &terminal,
        scroll_clear.len(),
        &scroll_clear,
    )?);

    let effects = b"\x07\x1b[2 q\x1b[?1000h\x1b[?1000l".repeat(2048);
    results.push(measure_output(
        "many-listener-effects-no-printable",
        &terminal,
        effects.len(),
        &effects,
    )?);

    let terminal = fresh()?;
    let mut rep = b"R".to_vec();
    rep.extend_from_slice(b"\x1b[65535b");
    results.push(measure_output("maximal-rep", &terminal, rep.len(), &rep)?);

    let mut terminal = fresh()?;
    terminal.output(&[0xe2]);
    let utf8_tail = [0x82, 0xac, b'X'];
    results.push(measure_output(
        "partial-utf8-continuation",
        &terminal,
        utf8_tail.len(),
        &utf8_tail,
    )?);

    let mut terminal = fresh()?;
    terminal.output(b"\x1b]0;synthetic-title-prefix");
    let osc_tail = b"-suffix\x07\x1b]8;id=fixed;https://example.invalid/synthetic\x07";
    results.push(measure_output(
        "partial-osc-title-and-hyperlink",
        &terminal,
        osc_tail.len(),
        osc_tail,
    )?);

    let mut terminal = fresh()?;
    let sync_prefix = b"\x1b[?2026h";
    let sync_pending = b"synthetic buffered text\x1b[3S\x1b[2J";
    terminal.output(sync_prefix);
    terminal.output(sync_pending);
    results.push(measure_sync_timeout(
        "pending-synchronized-buffer-flush",
        &terminal,
        sync_prefix.len() + sync_pending.len(),
    )?);

    Ok(json!({
        "schema": 1,
        "version": env!("CARGO_PKG_VERSION"),
        "core_profile": pigade_terminal::CORE_PROFILE,
        "sample_count": SAMPLES,
        "scope": "Synthetic actual-Term parser clone plus callback counting; no PTY/UI/disk workload and no plaintext output.",
        "limitations": [
            "Diagnostic timings are not a production latency or throughput guarantee.",
            "Callback counts describe parser requests, not effective grid work or an admission bound.",
            "No heap/RSS bound: clone allocations, allocator overhead, runtime and system effects are not measured.",
            "Fixtures contain only synthetic local bytes and labels; output payload bytes are never printed."
        ],
        "percentile_method": "nearest-rank, 30 release-mode samples per fixture",
        "results": results,
    }))
}

fn main() -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&run()?)?);
    Ok(())
}
