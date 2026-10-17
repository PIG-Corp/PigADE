# Host publication notification — 0.0.1-dev

The actual app now waits for a bounded host publication token independently of
its command worker. It then obtains validated cells through conditional Poll;
there is no second ANSI parser or raw output dropping. See
[ADR0012](../adr/0012-host-publication-notification.md) for ordering, limits and
legacy fallback. Normal operation has no diagnostic logging of terminal data.

Root verification passed206 workspace tests (81 host library plus real PTY/IPC,
recovery, storage, layout and terminal cases),125 app tests, strict Clippy in
both workspaces, formatting and release builds. The real host notification test
covers output, independent commands, idle timeout and historical restart with a
new host epoch. One-shot notification failure cannot change a committed launch
outcome. Pure/UDS tests are not native platform acceptance.

## Retained actual-app measurements

All rows use private Xvfb/Lavapipe, the same fonts and durable Btrfs journals.
There are50 fixed ordered keys per complete run, unique causal RGB cells and
independent no-echo child ACK, with no retry and the unchanged1s sample limit.
Other builds/soaks overlap. These are software diagnostics, not physical input,
scanout, the formal echo harness or isolated PERF acceptance.

| Run | App | Result | p50 ms | p95 ms | p99/max ms |
| --- | --- | --- | ---: | ---: | ---: |
| Initial notification |3eba79d2…|sample deadline, partial distribution unavailable|—|—|>1000 deadline|
| Diagnostic helper |3eba79d2…|50 exact keys +ACK|95.577|96.876|97.287|
| Finite publication trace |22b762db…|50 exact keys +ACK|95.582|97.208|99.322|

The frozen host is477d5f81… throughout. Exact hashes/source bindings, every
sample, the initial failure, recovery oracle and captures are retained under
[notification-20261017](../../benchmarks/results/notification-20261017/).
The initial failure's saved model has sequence10/background45,239,27, consistent
with sample index7 having reached the terminal model; full replay and
checkpoint+tail agree. It has no contemporaneous capture, so its precise cause
is unresolved. It remains a failed run rather than being replaced by later
passes. Cold-history equality is vacuous for this tiny journal.

The first diagnostic helper briefly included XKeysymToKeycode in its start
interval. Its exact source snapshot is retained; the current helper restores
the original start immediately before XTEST. No speedup is attributed to those
runs. Root visually reviewed the diagnostic pass capture: final causal color,
child ACK and exited state are visible. Timing gates remain open.

## Numeric path trace

`--diagnostic-publication-trace` opts into one logger thread, a64-record
nonblocking queue and512 total reserved record attempts shared across clones.
Producers copy at most four numeric sequence IDs; no keys, terminal text,
titles, paths or error strings enter the trace. Queue-full diagnostic records
are counted/dropped, while raw terminal output is unaffected. Tests block the
writer, exhaust the cap and inject a writer failure without blocking/retrying
producer work. The normal path uses a disabled handle.

TSV `record` fields are elapsed monotonic ns, stage, revision/request ID,
sequence count and up to four sequences. Stages1/2/3 mean Poll-applied,
UI-applied and paint callback entry;4/5 mean command dispatch/reply. Paint entry
is not GPU submission/presentation. The harness terminates its owned app after
the capture, so the final logger summary was not observed; do not infer zero
dropped records from its absence.362 numeric records match all50 causal frames.

For those frames, Poll→UI is40.178ms p50/41.455ms p95/42.618ms p99; UI→paint
entry is6.864/7.727/9.030ms. This localizes an observable delay after the replica
arrives. Source review also finds redraws of unchanged cells on ordinary text
commit and accepted command receipts. Reducing those redundant invalidations
is the next hypothesis to test; these traces do not prove a renderer/fence
root cause or solve the initial1s outlier.

## Reproduction

Use fresh private output paths and frozen binaries with verified hashes:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
crates/app/bootstrap.sh test
crates/app/bootstrap.sh clippy --all-targets -- -D warnings
python3 scripts/test_isolated_ime_x11.py
python3 scripts/isolated-ime-x11.py --fixture latency
python3 scripts/isolated-ime-x11.py --fixture latency --publication-trace --run \
  --app /absolute/path/to/frozen/pigade-app \
  --host /absolute/path/to/frozen/pigade-session-host \
  --output /absolute/new/private/path/outside/repository
```

The script refuses unverified display/server/window identity. Never substitute
an active-desktop endpoint. Full platform, memory, native IME target generation,
formal performance and MVP hierarchy/tiling/context gates remain open.

## Follow-up: redraw only changed presentation

The plan and receipt-presentation regressions were written before the bounded
redraw change. Normal unmarked commit/unmark no longer invalidates an unchanged
terminal canvas. Visible preedit removal, protocol/local admission errors,
cleared notices and cursor blink reset retain their notifications. Receipt
processing still drains the bounded channel every wake; only a changed visible
ACK button/release label requests its own redraw. Host frame/notice/recovery
updates continue to invalidate independently. The actual ACK renderer reuses
that same tested rule, including individually settled requests, other targets,
unknown outcomes and confirmed mouse presses after a rejected release.

Frozen app1f7a858a… with the same477d host and same cec087fd… helper completed
all50 ordered keys and independent ACK. Observed pixel latency was63.647ms p50,
66.165ms p95 and96.425ms p99/max. The prior traced run had p95=97.208ms. For
50 matched frames the new Poll→UI figures are0.411/0.466/35.835ms; UI→paint entry
is14.955/15.790/15.981ms (p50/p95/p99).267 finite trace records are retained,
versus362 previously. These are two individual software runs with concurrent
work, not a stable hardware speedup distribution or the50/80ms acceptance gate.
The earlier1s outlier remains unresolved. No durability, queue/input delivery or
host parser behavior changed.

Exact source hashes, samples and TSV are in
[redraw-source.json](../../benchmarks/results/notification-20261017/redraw-source.json),
[latency-redraw-pass.json](../../benchmarks/results/notification-20261017/latency-redraw-pass.json)
and [redraw-publication-trace.tsv](../../benchmarks/results/notification-20261017/redraw-publication-trace.tsv).
All127 app tests, strict Clippy, formatting and release build pass. An independent
read-only review found no stale-presentation regression.

The same frozen app then passed native Pinyin again on both private X11/Fcitx
and Sway/Pixman/Fcitx Wayland-v3: zero bytes before Space, exactly six UTF8 bytes
你好 with no extras, independent child ACK. Root viewed preedit and committed
captures for both: X11's external candidate popup remains at bottom left;
Wayland has marked inline preedit/candidate near the cursor; both clear after
commit and show ACK/exited state. This preserves commit/clear behavior without
claiming X11 candidate placement, generation-switch barriers or general IME
support. Captures and exact run metadata are in
[redraw-ime-20261017](../../crates/app/evidence/redraw-ime-20261017/).
