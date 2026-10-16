# Handoff — 0.0.1-dev

## Instructions and actual status

Implement the full requested Linux MVP against SPEC/DESIGN. The project is still a runnable foundation under stage0 evaluation; **MVP and stack/platform gates are not complete**. The user removed separate control stands; maintain the actual Rust+GPUI app only. Preserve tests/fixtures/measurements. All NEW subagents must explicitly use gpt-6-luna with a model-overridable role, fork_turns=none and self-contained bounded tasks; do not reactivate old-model children.

No child owns production paths at this handoff. Root integrates the current foundation. Read current git status and latest WORKLOG; older agent/path descriptions are historical, not current ownership.

## Implemented contracts

Host owns one alacritty/VTE model. All output/resize/significant timers are CRC/sequence/generation journaled+fsynced before publication. Adjacent queued output can merge up to16KiB without waiting or crossing control events. Bounded input worker, priorityStop, bounded private IPC and durable launch helper/OS identity/generation locks exist. PTY EOF is not process exit; Stop remains serviceable. Synchronized query replies stream in input-sized chunks. Unknown outcomes never respawn automatically.

Complete schema2 terminal checkpoints cover both grids/history/cursors/config/modes/colors/parser/REP/UTF8/OSC/sync logical state. Shared extras/hyperlinks and bounded decoding fix reproduced amplification/panic/hang cases. Borrowed row export removes a ring/cache clone. Checkpoint publication is fsync→rename→directoryfsync; keep3valid and preserve bad originals. Cadence counts every event (16MiB/1024events/125ms measured model work/5s wall, whichever first, plusExit); quiet dirty state also publishes. Work threshold is a heuristic, not a final budget proof. Raw journals are never pruned.

SQLite schema3 has project/workspace/tab/window metadata, CAS and consistent pre-migration backup. Global LayoutHistory100undo/redo and per-tab projections commit atomically. Explicit valid legacyv2 layout import preserves placement and reports missing prior undo; corrupt legacy state stays intact/read-only. Encoded catalog is counted beforecommit (7MiBwire plus4MiBraw cap) to prevent durable unservable metadata. RootResponse carries migrationnotice. Pure layout supports docking/move/swap/transfer/resize/equalize/rotate/focusmode; windowresize is projection only.

Actual native app renders host cells only; at most4 sessions, current temporary projection, Ctrl+Shift+Space prefix, EN/RU commit encoder/headless tests. Negative-descent and background-over-wide-glyph clipping repaired and visually rechecked in isolatedsoftwareWayland. UI close hook quits the UI process without stopping independently managed host. Full hierarchy/tiling/drag UI is pending.

## Fresh evidence

- Root workspace72 tests pass (13layout,17hostunit,8realPTY/recovery,1saturatedinput,23storage,10terminal), strictworkspaceclippy, fmt and release build.
- App13 headless tests and strictclippy/build passed; two upstream GPUI float future warnings remain. No Windows/macOS/X11 runtime claim.
- Vendor VTE56 tests; Alacritty146lib plus45reference fixtures. Hyperlink golden adapts only generated anonymous-ID suffixes to the explicit per-Term profile; provenance/hash documented, no comparison removed.
- Actualapp isolated bwrap/Weston15.0.1/Pixman+Lavapipe26.2.1 render+UIkill/reopen passed with same4shellPIDs/generations and69×15acknowledgedgeometry. Private namespaces/socketpeer/FD/device checks recorded. No host desktop socket/input/GPU access or keyboard injection.
- 60s four1MiB/s streams: every byteCRC and complete checkpoint-tail/fullreplay state matches; producer-rate target passes aftercoalescing. Host sampledRSS30,334,976bytes;112ACK observations p50=30.414,p95=67.648,p99=113.127ms. This is input-to-durable-snapshot observation, **not input-to-present**; full performance gate remains open. Failed prior throughput run retained.
- Independent reviewer re-executed v2migration and oversized-wire defects and approved their repairs. Latest query-burst addition separately passes realPTY+complete-state equality.

## Next critical work

1. Adopt explicit cold-archive versus unlimited-grid reflow ADR and implement deterministic retirement hooks+bounded immutable archive/index. See research/cold-history-plan.md. Do not add fields to Row without repairing Storage::swap's unsafe fixed-size assumption.
2. Global heap/admission budget and checkpoint/recovery scheduler: current128MiBencoded cap is not total heap; combining marks and shrink reflow still have transient/allocation risks. Fix before long-history/resource acceptance.
3. Execute4×1GiB+100kresizes, record bytes-read bounded recovery, fault boundaries, VM reboot and separate24hsoak. None completed.
4. Isolated native input/IME/clipboard/accessibility/X11/scale/GPU checks; current software screenshot proves rendering/reopen only. A compatible Weston input driver is not yet verified. Never fall back to the active desktop.
5. Then finish Git/worktree preparation, full hierarchy+tiling interaction, versioned context/memory/capabilities/exactresume, packaging and end-to-end MVP. AI CLIs only version/help probed; no automaticresume certified. No --last or blind keyboard prompt delivery.

Known host limits: Linux-specific identity/IPC,32generation registry cap currently counts historical entries, eager historical frame loading, no-frame unresolved intents not yet exposed as UI entries, no coldindex, JSONfullframepolling50ms. Preserve originals and explicit degradation; do not label history live or hide errors to pass gates.
