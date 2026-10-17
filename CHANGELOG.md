# Changelog

## 0.0.1-dev — in development

- Initial SPEC.md and DESIGN.md product contracts preserved.
- Git repository, mandatory shared agent history, project map, handoff and version policy introduced.
- Native terminal/PTY/storage foundation, complete-state checkpoint experiments and GPUI cell-replica application under verification. Separate Electron control application removed by user request.
- VTE UTF-8 fix, bounded checkpoint decoding, launch recovery and saturated-input regressions added; stack/MVP gates remain open.
- Pure tiling with stable UUIDs, atomic cross-workspace transfers and persistent100undo/redo; schema3 catalog CAS and backed-up legacy migrations.
- Allocation-aware checkpoint export/decode, all-event/idle checkpoint cadence, bounded output batching and chunked query replies.
- Verified private software-Wayland rendering/reopen and measured4-stream throughput; complete MVP/performance gates remain open.
- Host-integrated cold normal history: PIGROW02 rows, generation segments, fixed-slot index and PIGCP002 committed roots; complete hot/parser and every-record recovery equality. The index-batch run meets the 60-second producer body rate but misses host-exit deadline and has no input-latency samples; see the cold-history integration report.
- Bounded conditional polling avoids repeating unchanged terminal frames. The 64 KiB index buffer preserves data fsync before index fsync and uses logical archive offsets to avoid per-row seeks. Synthetic isolated VM reboot verifies four sessions with 2,467 cold rows each; provider resume/layout acceptance and the 24h soak remain open.
- Fresh root verification passed 128 workspace tests, 162 vendor library tests plus 45 reference cases, 57 VTE tests and the native app's 21 tests/all-target strict Clippy/build. Isolated software-rendered UI reopen preserved four views and their PIDs/generations. No GPU or platform acceptance is claimed.
- The live-model recovery oracle now compares complete state, frame and every cold record against both replay paths; a mismatched-history regression fails explicitly.
- The latest frozen corpus smoke verified four 1 MiB sessions with 100 resizes, common-root recovery, SHA-256 and logical 24-hour metadata. It is not the 1 GiB corpus or a real-time soak.
- Separate locked runtime endpoints support long durable state paths. Leased replay caches have bounded background orphan maintenance that preserves unknown/source files.
- Canonical cold-history records can be read in bounded chunks by two asynchronous workers; a blocked archive read does not block commands to another live PTY. The product history viewer is still pending.
- Queue-full replies distinguish proven rejection before admission from unknown outcomes. The soak retries only explicitly rejected resize commands; the UI does not retry input or launches. The failed 104-second soak is retained and the new actual 24-hour attempt remains in progress.
- The same app builds on pinned GPUI platform/AccessKit. A private Wayland AT-SPI probe sees named application, heading, terminal and status nodes; screen-reader text/history operation remains unverified. The pinned Linux Rust-dependency license inventory covers 481 reachable packages.
- Bounded VTE callback and Term allocation censuses cover REP, partial parser state, both grids/cached rows, shared extras, links and metadata; release diagnostics measure traversal cost without running it per output event. Global memory admission is still pending.
- Bundled terminal font aliases avoid ambiguous system-family selection; an offline reproducible generator preserves outlines/metrics/cmap and includes OFL attribution. The painter clips clusters to host-assigned spans. JP CJK forms are explicit; uncovered platform fallback and cross-platform parity remain unverified.
- Invalid typed journal tails retain the last valid generation/model/history with a sequence-specific error instead of dropping the session. Full/fast recovery and hidden-state continuation regressions cover this path; raw sources are unchanged.
- Unreadable/incompatible journal artifacts produce bounded nonfatal recovery warnings without fake sessions or blocking healthy terminals. Poll validation is atomic; notice-only updates preserve cached cells. Unix filename controls and extreme omission counts are covered.
- Resource census now distinguishes shared allocation size from reference-weighted combining capacity; a checkpoint-import/VTE continuation regression demonstrates copy-on-write fanout. Whole-event/global admission is still open.
- Checkpoint export borrows the validated grid and payloads while preserving exact schema2 bytes; import checks the borrowed envelope before owned decoding and shares a cell budget across both grids. Model-process RSS measurements and their scope are recorded in ADR0009.
- The second real-time soak failed after64minutes. A producer bug discarding the next PING after a coalesced RELEASE is reproduced and repaired; failed evidence is retained and the24h gate remains open.
- User-selectedgpt-6-luna model for all new subagents.
- Mandatory desktop isolation after a live input automation incident; unsafe smoke tooling removed.

This section describes development work; it is not a release announcement.
