# ADR 0002 — Maintained full-state extension experiment

Date: 2026-10-17. Status: experimental, production gate open.

Unmodified native core and the Electron serialize control both fail complete recovery requirements (ADR 0001 and control evidence). Continue the SPEC §10.3 permitted small-fork experiment before choosing an alternative. No budget or acceptance requirement changes.

The owned modifications are explicit versioned DTOs and validation around existing mature parsing/model code, not a new ANSI parser. VTE exports its finite-state machine, params/intermediates, partial UTF-8/OSC, REP character and synchronized buffer/logical timeout. The terminal export must include both grids with both cursors, all terminal state and a deterministic anonymous hyperlink counter. Event listeners and render damage are runtime dependencies reconstructed safely; replay never executes external effects.

The VTE profile also caps collected OSC at 64 KiB. An overlong OSC is ignored **as a whole** until its terminator, never executed as a truncated command; the original raw bytes remain in the journal. This is an explicit protocol limit and part of the pinned core profile. It avoids unbounded private-parser allocation; queue limits alone could not do so. The cap does not permit output/journal loss. A continuation test must prove return to ground and unchanged subsequent text.

Binary checkpoint encoding uses [Postcard 1.1.3](https://docs.rs/postcard/1.1.3/postcard/), Apache-2.0 OR MIT, with its documented stable 1.x format, explicit schema/core profile and input bounds. This dependency has a concrete purpose: avoid enormous per-cell JSON checkpoint overhead. IPC cell snapshots remain a separate prototype representation and are not checkpoints. Publication needs checksum, generation/sequence/offset, fsync temporary file, atomic rename and directory sync; old valid checkpoints and raw journal remain recovery fallbacks.

The experiment is accepted only as implementation work. Required evidence before stack approval still includes complete DTO continuation equality, actual fault boundaries, cold-history index, four 1-GiB corpora/100,000 resizes, 24h real soak, real CLI matrix, VM reboot, GPUI platform/accessibility and fair performance comparison. No compaction/deletion of raw history is authorized by a successful short test.
