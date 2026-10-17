# Terminal resource census — 0.0.1-dev

The terminal now has two bounded diagnostic primitives for a future admission
policy. Neither limits the running model or establishes the total memory gate.

The parser callback census advances a clone of the existing VTE processor. It
counts input, zero-width characters, title and hyperlink payload bytes and
title pushes, including buffered synchronized output and REP. REP counts are
aggregated without calling the census handler 65,535 times. The original
processor and terminal stay unchanged; bounded parser buffers can still be
allocated while cloning. It introduces no second ANSI implementation.

The allocation census inspects both Term grids and every retained cache row,
Vec capacities, four cursor templates, separately deduplicated CellExtra and
hyperlink Arc allocations, combining Vec capacities, title strings/stacks,
tab stops, keyboard stacks and damage lines. Identical contents in distinct
Arcs count as separate allocations. Caller-supplied cell-visit and unique-Arc
limits stop work and mark the result incomplete. The estimate includes payload
capacities and an estimated Arc header; allocator overhead, fragmentation,
parser/wrapper/listener/observer storage and transients are excluded. Even a
complete result is not an upper bound on process memory.

The release example uses the actual terminal model and verifies the complete
checkpoint stays byte-identical before/after each census. Its 30-sample nearest
rank timings and exact source/binary hashes are in
[`terminal-allocation-census.json`](../../benchmarks/results/terminal-allocation-census.json).
Concurrent corpus, soak and build work were present, so these are diagnostic
cost samples, not isolated PERF-01 results.

| Model fixture | Estimated Term payload bytes | Visited cells | Census p95 |
| --- | ---: | ---: | ---: |
| Empty, 120×36 | 212,443 | 8,644 | 0.018 ms |
| 10,000 hot rows, 120×36 | 29,541,723 | 1,208,644 | 3.486 ms |
| Same history after 50 resizes | 27,377,843 | 1,127,172 | 6.537 ms |
| 262,144 combining marks on one cell | 1,261,075 | 8,644 | 0.023 ms |
| 4,096 distinct 1 KiB hyperlinks | 19,663,621 | 608,644 | 2.009 ms |

The full census is O(retained cells); it is deliberately absent from the
output/frame loop. These measurements support admission-triggered accounting,
not a full walk on every output chunk. Required next work is a conservative
whole-event reservation covering grid growth, CellExtra copy-on-write,
metadata, resize/reflow and checkpoint/frame/import/export transients, plus a
shared host budget. Rejection must retain the durable raw tail and last valid
frame, leave parser/model unchanged and recover that same valid prefix with an
explicit resource error. Cumulative lifetime counters that never decrease
under overwrite are unsuitable. The existing 128 MiB encoded checkpoint limit
does not solve this problem.

```sh
cargo run --release -p pigade-terminal --example allocation_census
```

The census is tested for shared/distinct Arc ownership, copy-on-write extras,
inactive row caches, all cursor roots, metadata-capacity deltas, work-limit
exhaustion and unchanged checkpoints. Global heap admission, storage quotas and
the SPEC 700 MiB product budget remain open release blockers.

The follow-on census exposes total CellExtra references, reference-weighted
combining capacity and the largest single extra capacity. The weighted field
intentionally counts shared storage more than once: it describes potential
copy-on-write fanout, not additional currently allocated memory. Unique payload
estimates remain unchanged. A defensive regression imports a valid checkpoint
with four cells sharing 4,096 marks, then sends two combining characters through
the actual VTE parser to different cells. Those two marks create two large
unique Vec allocations; source and restored checkpoints still match. This is
caller-constructed/imported state, not evidence of normal ANSI reachability.

Fresh release samples with these fields are preserved separately in
[`terminal-allocation-census-references.json`](../../benchmarks/results/terminal-allocation-census-references.json).
The five payload estimates match the original diagnostic; p95 traversal costs
are 0.018/2.766/6.179/0.018/1.742 ms in fixture order. Concurrent long runs remain
active, so neither set proves isolated performance. A policy based only on
incoming mark bytes would miss the demonstrated fanout and is not installed.

The pinned-parser census now counts title set/pop and payload maxima,
hyperlink updates/maxima and each Term query callback kind, including OSC52
queries which the configured live model disables. Partial and synchronized OSC,
title pushes, long OSC8 updates and CSI-only query storms preserve original
parser checkpoint bytes. Counts do not execute handlers or establish response
byte bounds. See [admission plan](../research/terminal-admission-plan.md).

Checkpoint export now borrows grids and payloads; shared two-grid import bounds
and early envelope validation are integrated. The measured component RSS
comparison and exact compatibility evidence are in
[ADR0009](../adr/0009-borrowed-checkpoint-export.md). These repairs do not close
the global budget, live growth or transient-reservation gaps above.

The next snapshot adds grid geometry, initialized rows versus outer capacity,
logical combining/link/title lengths, and no-allocation parser buffer usage:
[`terminal-allocation-census-shapes.json`](../../benchmarks/results/terminal-allocation-census-shapes.json).
Its source hashes capture the fixed 1,000-row cache **before** the width-sensitive
cache repair. The five original unique payload estimates remain equal. The
processor's 488 inline bytes include its parser/timeout handles; parser inline
storage must not be added again. Sync storage now starts with zero capacity,
grows geometrically below the existing flush threshold, and retains capacity
for reuse. This is not evidence of a 2 MiB RSS reduction: the former eager
allocation need not have committed all its pages.

The grid cache now targets 256 KiB of newly appended spare cell payload,
clamped to 1–1,000 rows. Required active rows are never refused or truncated.
For one 1,024-column Cell row, the regression changes 999 spare rows to 9:
216 KiB rather than about 24 MiB of spare cell payload. A separate 48-scroll
oracle compares canonical rows and cursor against an explicit legacy-sized
cache. Owned/borrowed checkpoint and continuation tests still pass. This target
does not bound heterogeneous retained row capacities, outer Vec capacity,
allocator overhead, total hot history or full-process RAM.

Fresh post-change model samples are in
[`terminal-allocation-census-cache-target.json`](../../benchmarks/results/terminal-allocation-census-cache-target.json).
The hyperlink fixture drops from 19,663,621 to 17,052,101 estimated payload bytes;
the full hot-history fixture slightly increases from 29,541,723 to 29,560,283
because growth/cache alignment differs. The empty and resized fixture estimates
remain unchanged. Both source snapshots and all 30 timing samples are retained;
neither proves isolated throughput or the global product budget.

Frame interning also replaces duplicate owned `(id,uri)` map keys with shallow
Hyperlink handles. Equal contents in distinct model allocations still produce
one wire-pool entry, verified before and after the repair. No numeric RSS claim
is attached to that local copy removal.

## Read-only whole-event driver preflight

The terminal wrapper now exposes the cloned pinned parser census for output
and ordered sync timeout. It includes cursor/scroll/insert/erase/reset/grid-mode
and keyboard-stack drivers. Review found omitted listener events: BEL,
cursor-style and mouse/private-mode updates are now counted too, along with
OSC52 store requests (disabled by the live profile). The counters saturate
explicitly. They describe requested callbacks, not geometry-clamped operations,
allocated bytes or an event reservation. No second ANSI parser is introduced.

The two actual-wrapper regressions preserve exact checkpoints, frames, pending
responses and timeout/parser state. Continuation after the dry run still matches
the untouched checkpoint, including split UTF8, partial CSI and buffered sync.
The vendor suite additionally covers CSI-only operations, maximal REP and
listener callback storms. ESC E dispatches linefeed plus carriage return in
this pinned VTE, while its Handler::newline counter remains zero.

Thirty release samples per synthetic fixture are retained with source and binary
hashes in [resource-preflight-20261017](../../benchmarks/results/resource-preflight-20261017/source.json).
These measure clone-plus-callback work, with concurrent builds and two soaks.

| Candidate | Input bytes | p95 µs |
| --- | ---: | ---: |
| Plain ASCII | 65,536 | 131.580 |
| CSI scroll/clear/insert/delete, no printable input | 131,072 | 762.297 |
| BEL/cursor-style/mouse modes, no printable input | 45,056 | 189.309 |
| Maximal REP | 9 | 0.121 |
| Split UTF8 continuation | 3 | 0.100 |
| Partial OSC title and hyperlink | 55 | 0.621 |
| Ordered timeout flushing buffered sync | 39 including sync start | 0.250 |

Exact candidate lengths are authoritative in the raw results. These are local
diagnostic costs, without PTY, host durability, UI or GPU, and do not pass PERF.
The [payload-copy analysis](../research/terminal-payload-copy-bound.md) describes
why these counters can bound logical copies but cannot alone reserve allocator
capacity, intermediate frames or aggregate host memory.

## Discarded shared-mark copy repair

`Cell::clear_wide` used to clone a shared combining vector before discarding it.
It now creates only the retained small extra with empty marks; underline and
hyperlink sharing remain intact, and the unique-Arc path drops marks in place.
The [plan](../research/wide-cell-clear-plan.md) and two regression tests preceded
the repair. Shared siblings, colors, other flags and unique Arc identity retain
their earlier behavior; checkpoint schema2 is unchanged.

The single-threaded actual-cell allocation diagnostic with262,144 shared marks
requests56 bytes in one allocation during clearing. Explicitly emulating the
former make_mut-before-clear algorithm in the **same binary** requests1,048,632
bytes in two allocations, in all30 samples. The comparison is not an old
binary run. The saved samples count allocator requests during that operation,
excluding fixture/JSON creation; they are not peak heap, RSS, total memory or
a whole-event bound. This removes one known transient amplification without
closing the global memory gate.

```sh
cargo run --offline --locked --release -p pigade-terminal --example resource_preflight
cargo run --offline --locked --release -p pigade-terminal --example wide_clear_allocations
```
