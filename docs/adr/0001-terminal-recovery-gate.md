# ADR 0001 — Terminal recovery gate remains open

The comparative-application requirement below is superseded by [ADR0003](0003-native-application-direction.md), following the user’s explicit removal of separate control stands. All correctness/recovery/platform budgets remain in force.

Date: 2026-10-17. Status: **proposed; evidence recorded, production stack not accepted**.

## Context

SPEC.md §§7–8 and 10.4 require one authoritative host model, durable ordered events and equality of live state, full replay and bounded-cost checkpoint+tail recovery. Full replay is explicitly permitted for the early prototype but cannot be the sole production path for indefinitely long sessions.

Source inspection and an executed isolated counterexample show that `alacritty_terminal 0.26.0` plus `vte 0.15.0` do not expose a complete durable snapshot. Grid serde skips both cursors. Term holds additional private state. The parser/processor privately retain partial UTF-8/CSI/OSC, REP state, synchronized bytes and timer state. Resetting the parser after a snapshot demonstrably changes subsequent printable output. Sources, exact pins, licenses and reproducible code are in [terminal-core research](../research/terminal-core.md).

The published GPUI 0.2.2 archive names commit `69e2130295c2649963eb639fc70b4f2ee8ea1624` with `dirty:true`. That is not proof of equivalence to a clean commit dependency. The actual clean commit exists and can be selected for a separate build/platform experiment. No platform acceptance is inferred from its backend code.

## Proposed boundary for the prototype

Keep the native candidate under evaluation. Prototype full ordered replay as the correctness oracle; explicitly mark historical output and expose recovery progress. No raw journal compaction, production checkpoint claim, automatic stack approval or release claim follows from that prototype.

Evaluate a small, versioned export/import patch covering both terminal and parser crates, with custom logical synchronized-output timing, before deciding whether the native recovery path is viable. Model DTOs must include both grids and cursors, terminal modes/colors/tabstops/scroll region/title and keyboard stacks, complete parser state, preceding character and sync bytes. Runtime event sinks and render damage are recreated safely. Cold history requires a separately tested immutable index and bounded hot storage.

This is a proposed implementation experiment, not authorization to import Zed's GPL terminal integration or a final dependency/architecture decision. The lead engineer owns adoption and the alternative-selection decision.

## Evidence required before acceptance

- Continuation-sensitive live/full replay/checkpoint+tail equality, including every split byte boundary and explicit timer events.
- Durable-prefix and snapshot-publication fault injection, generation isolation and suppression of replay side effects.
- SPEC.md §12.2's four-session 1 GiB history corpus with 100,000 resizes per session; active frames restored within budget without reading the whole journal.
- Bounded parser, queue, hot-history, checkpoint-tail and archive behavior; the upstream std OSC Vec is an explicit unresolved memory risk.
- A real commit-pinned GPUI build and Wayland/X11 input, IME, Unicode, clipboard, fractional-scale, GPU and accessibility checks.
- Comparable Electron+xterm control measurements on the same release workload/hardware, and actual required CLI integration.
- VM reboot and 24-hour real-time soak evidence. Accelerated replay does not substitute for either.

## Consequences and reconsideration

The repository may remain a runnable stage-0 prototype while this gate is open. Partial screenshots, a successful serde call, or a short replay benchmark cannot mark stage 0 complete. No performance budgets or recovery requirements are relaxed.

If a bounded full-state extension cannot be validated, return to the alternatives explicitly permitted in SPEC.md §10.3; record a separate ADR backed by the control prototype's evidence. The fallback must satisfy the same authoritative-model and recovery contract. Selecting a different UI alone does not repair a missing terminal snapshot.

## Additional stage-0 finding

Unpatched vte 0.15.0 loses a space for UTF-8 `é 界` split into chunks of lengths 1 and 5. A direct Processor-only reproducer and source explanation are recorded in [the research report](../research/terminal-core.md#confirmed-upstream-partial-utf-8-defect). This is a concrete TERM-02 blocker in the unmodified candidate. A narrow consumed-byte-count patch must pass the complete boundary corpus before the candidate proceeds; it does not remove the separate full-checkpoint blocker.

The narrow UTF-8 fix is adopted locally in `vendor/vte`; see its `PIGADE-PATCH.md`. The terminal profile names this patched variant so recovery cannot silently switch between patched and original semantics. This permits continued prototype testing; the production gate remains open.
