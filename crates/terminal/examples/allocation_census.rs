//! Diagnostic on the actual terminal model, with no PTY/UI or disk workload.
//! Reports scoped owned-payload capacities, not heap/RSS or a release budget.
use std::time::Instant;

use anyhow::{Result, ensure};
use pigade_terminal::{CensusLimits, Geometry, GridAllocationCensus, Terminal};
use serde_json::{Value, json};

const LIMITS: CensusLimits = CensusLimits {
    max_cell_visits: 16_000_000,
    max_unique_allocations: 1_000_000,
};

fn grid_shape(grid: GridAllocationCensus) -> Value {
    json!({
        "screen_lines": grid.screen_lines,
        "columns": grid.columns,
        "history_limit": grid.history_limit,
        "initialized_rows": grid.initialized_rows,
        "outer_row_capacity": grid.outer_row_capacity,
        "cell_capacity": grid.cell_capacity,
        "short_row_column_gap": grid.short_row_column_gap,
    })
}

fn measure(name: &str, terminal: &Terminal) -> Result<Value> {
    let before = terminal.checkpoint()?;
    let mut samples = Vec::new();
    for _ in 0..30 {
        let started = Instant::now();
        let census = terminal.term_allocation_census(LIMITS);
        samples.push(started.elapsed().as_secs_f64() * 1000.);
        ensure!(!census.incomplete, "diagnostic census work limit exceeded");
        std::hint::black_box(census);
    }
    samples.sort_by(f64::total_cmp);
    let census = terminal.term_allocation_census(LIMITS);
    let parser = terminal.parser_buffer_usage();
    ensure!(terminal.checkpoint()? == before, "census mutated model");
    let limited = terminal.term_allocation_census(CensusLimits {
        max_cell_visits: 1,
        max_unique_allocations: 0,
    });
    ensure!(
        limited.incomplete,
        "tiny work limit must not report completion"
    );
    Ok(json!({
        "name": name,
        "geometry": terminal.frame().geometry,
        "hot_history_rows": terminal.frame().history_lines,
        "active_grid": grid_shape(census.active_grid),
        "inactive_grid": grid_shape(census.inactive_grid),
        "cell_visits": census.cell_visits,
        "row_slots_capacity": census.row_slots_capacity,
        "cell_slots_capacity": census.cell_slots_capacity,
        "unique_cell_extras": census.unique_cell_extras,
        "cell_extra_references": census.cell_extra_references,
        "unique_hyperlinks": census.unique_hyperlinks,
        "zero_width_chars_capacity": census.zero_width_chars_capacity,
        "reference_weighted_zero_width_capacity": census.reference_weighted_zero_width_capacity,
        "max_single_extra_zero_width_capacity": census.max_single_extra_zero_width_capacity,
        "unique_zero_width_chars_len": census.unique_zero_width_chars_len,
        "reference_weighted_zero_width_chars_len": census.reference_weighted_zero_width_chars_len,
        "max_single_extra_zero_width_chars_len": census.max_single_extra_zero_width_chars_len,
        "hyperlink_id_bytes_len": census.hyperlink_id_bytes_len,
        "hyperlink_uri_bytes_len": census.hyperlink_uri_bytes_len,
        "hyperlink_id_bytes_capacity": census.hyperlink_id_bytes_capacity,
        "hyperlink_uri_bytes_capacity": census.hyperlink_uri_bytes_capacity,
        "title_stack_slots_capacity": census.title_stack_slots_capacity,
        "current_title_len_bytes": census.current_title_len_bytes,
        "current_title_capacity_bytes": census.current_title_capacity_bytes,
        "title_stack_count": census.title_stack_count,
        "title_stack_len_bytes": census.title_stack_len_bytes,
        "title_stack_strings_capacity_bytes": census.title_stack_strings_capacity_bytes,
        "max_title_len_bytes": census.max_title_len_bytes,
        "max_title_capacity_bytes": census.max_title_capacity_bytes,
        "parser_storage": {
            "processor_inline_bytes_includes_parser": parser.inline_size_bytes,
            "sync_len": parser.sync_buffer_len,
            "sync_capacity": parser.sync_buffer_capacity,
            "osc_len": parser.parser.pending_osc_len,
            "osc_heap_capacity": parser.parser.pending_osc_heap_capacity,
            "osc_inline_capacity_already_in_processor_size": parser.parser.pending_osc_inline_capacity,
            "scope": "Buffer backing and inline handles; no allocator overhead. Do not add parser inline size a second time."
        },
        "estimated_persistent_owned_bytes": census.estimated_persistent_owned_bytes,
        "incomplete": census.incomplete,
        "checkpoint_unchanged": true,
        "tiny_work_limit_incomplete": true,
        "census_ms": {"p50": samples[14], "p95": samples[28], "p99": samples[29]},
        "samples_ms": samples,
    }))
}

fn main() -> Result<()> {
    let mut reports = Vec::new();
    let mut terminal = Terminal::new(Geometry::new(36, 120)?, false);
    reports.push(measure("empty-120x36", &terminal)?);
    let line = format!("{}\r\n", "x".repeat(119));
    let batch = line.repeat(128);
    for _ in 0..79 {
        terminal.output(batch.as_bytes());
    }
    ensure!(
        terminal.frame().history_lines == 10_000,
        "fixture history not full"
    );
    reports.push(measure("10000-hot-rows-120x36", &terminal)?);
    for _ in 0..25 {
        terminal.resize(Geometry::new(40, 128)?);
        terminal.resize(Geometry::new(32, 112)?);
    }
    reports.push(measure("10000-hot-rows-after-50-resizes", &terminal)?);
    drop(terminal);

    let mut terminal = Terminal::new(Geometry::new(36, 120)?, false);
    terminal.output(b"e");
    let marks = "\u{301}".repeat(4096);
    for _ in 0..64 {
        terminal.output(marks.as_bytes());
    }
    reports.push(measure("262144-combining-marks-one-cell", &terminal)?);
    drop(terminal);

    let mut terminal = Terminal::new(Geometry::new(36, 120)?, false);
    let uri_tail = "x".repeat(1024);
    for index in 0..4096 {
        terminal.output(
            format!("\x1b]8;id={index};https://example.test/{uri_tail}\x07L\x1b]8;;\x07\r\n")
                .as_bytes(),
        );
    }
    reports.push(measure("4096-distinct-hyperlinks-1KiB-each", &terminal)?);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema": 1,
            "version": env!("CARGO_PKG_VERSION"),
            "core_profile": pigade_terminal::CORE_PROFILE,
            "scope": "Actual Term payload-capacity census; no PTY, host, disk, UI or GPU. Concurrent machine workload is uncontrolled.",
            "limitations": [
                "Not allocator bytes, RSS, global admission or PERF-01 acceptance.",
                "Parser, wrapper, sinks, observers, allocator overhead and transients excluded.",
                "The census is O(retained cells); not installed in the output/frame loop."
            ],
            "percentile_method": "nearest rank, 30 sorted samples per fixture",
            "results": reports
        }))?
    );
    Ok(())
}
