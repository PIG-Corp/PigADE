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
- Fresh root verification passed 120 workspace tests, 152 vendor library tests plus 45 reference cases, and the native app's 17 tests/all-target strict Clippy/build. Isolated software-rendered UI reopen preserved four views and their PIDs/generations. No GPU or platform acceptance is claimed.
- The live-model recovery oracle now compares complete state, frame and every cold record against both replay paths; a mismatched-history regression fails explicitly.
- The latest frozen corpus smoke verified four 1 MiB sessions with 100 resizes, common-root recovery, SHA-256 and logical 24-hour metadata. It is not the 1 GiB corpus or a real-time soak.
- User-selectedgpt-6-luna model for all new subagents.
- Mandatory desktop isolation after a live input automation incident; unsafe smoke tooling removed.

This section describes development work; it is not a release announcement.
