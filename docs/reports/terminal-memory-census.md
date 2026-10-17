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
