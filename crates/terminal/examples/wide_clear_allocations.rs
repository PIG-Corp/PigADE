//! Requested allocation bytes for actual shared-cell clearing, not RSS/heap.
//! The comparison explicitly emulates the former make_mut-before-clear path;
//! it is not a measurement of a separately built historical binary.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::vte::ansi::Color;
use anyhow::{Result, ensure};
use serde_json::json;

struct CountedSystem;
static ENABLED: AtomicBool = AtomicBool::new(false);
static REQUESTED: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);

fn record(bytes: usize) {
    if ENABLED.load(Ordering::Relaxed) {
        REQUESTED.fetch_add(bytes, Ordering::Relaxed);
        CALLS.fetch_add(1, Ordering::Relaxed);
    }
}

// This standalone, single-threaded diagnostic uses the normal System allocator.
unsafe impl GlobalAlloc for CountedSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountedSystem = CountedSystem;

fn measure(fixture: &Cell, emulate_old_copy: bool) -> Result<(usize, usize)> {
    let mut cell = fixture.clone();
    REQUESTED.store(0, Ordering::Relaxed);
    CALLS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    if emulate_old_copy {
        // Previous clear_wide cloned the shared extra (including all marks)
        // before discarding the cloned vector. Force that same discarded copy.
        std::hint::black_box(Arc::make_mut(cell.extra.as_mut().unwrap()));
    }
    cell.clear_wide();
    ENABLED.store(false, Ordering::Relaxed);
    let result = (
        REQUESTED.load(Ordering::Relaxed),
        CALLS.load(Ordering::Relaxed),
    );
    ensure!(cell.c == ' ' && !cell.flags.contains(Flags::WIDE_CHAR));
    ensure!(cell.zerowidth().is_some_and(<[char]>::is_empty));
    ensure!(cell.underline_color() == fixture.underline_color());
    ensure!(cell.hyperlink() == fixture.hyperlink());
    Ok(result)
}

fn main() -> Result<()> {
    let marks = 262_144;
    let mut fixture = Cell {
        c: '界',
        flags: Flags::WIDE_CHAR | Flags::BOLD,
        ..Cell::default()
    };
    for _ in 0..marks {
        fixture.push_zerowidth('\u{301}');
    }
    fixture.set_underline_color(Some(Color::Indexed(17)));
    fixture.set_hyperlink(Some(Hyperlink::new(
        Some("synthetic"),
        "https://example.invalid/".into(),
    )));
    let expected = fixture.clone();
    let mut samples = Vec::new();
    for _ in 0..30 {
        let repaired = measure(&fixture, false)?;
        let former_copy_emulation = measure(&fixture, true)?;
        ensure!(fixture == expected, "shared sibling changed");
        ensure!(
            former_copy_emulation.0 >= repaired.0 + marks * size_of::<char>(),
            "fixture did not expose the discarded mark copy"
        );
        samples.push(json!({
            "current_requested_bytes": repaired.0,
            "current_allocation_calls": repaired.1,
            "former_copy_emulation_requested_bytes": former_copy_emulation.0,
            "former_copy_emulation_allocation_calls": former_copy_emulation.1,
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema": 1,
            "version": env!("CARGO_PKG_VERSION"),
            "fixture_marks": marks,
            "shared_sibling_unchanged": true,
            "scope": "Requested allocator bytes only during Cell::clear_wide; fixture and JSON allocations excluded. Former path is explicitly emulated in the same binary.",
            "limitations": "Not RSS, live heap, allocator overhead, whole-event admission, host/UI workload or a separately built historical binary.",
            "samples": samples,
        }))?
    );
    Ok(())
}
