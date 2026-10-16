# Cold history plan — 0.0.1-dev

Read-only architecture review by `cold_history_design`, persisted by root on2026-10-17. Source analysis, not executed acceptance measurements. Stage0 remains blocked on archive semantics/implementation, long-history and aggregate memory proof.

## Semantic decision required

The current pinned profile has10,000 hot rows. Add an immutable archive of retired normal-screen cell fragments alongside that bounded authoritative state. An archive is not an unlimited Alacritty Grid: future resizes cannot silently pull archived rows into the live grid. Full replay and checkpoint+tail must execute the same explicit retirement policy and reproduce both hot state and archive. If unlimited live reflow is required, a persistent backing store inside Grid is necessary instead. Record this distinction in an ADR before acceptance; do not silently weaken SPEC7.3/8.3/11.2.

Keep original physical cells, widths, occupied/wrap metadata, attributes, extras/hyperlinks and palette provenance. Logical-line reading may join only recorded continuations; never infer joins by trimming text or comparing rows. A single soft-wrapped line can exceed the hot limit, so persist bounded fragments and a continuation cursor instead of accumulating the whole line. Clear/reset operations create retained historical epochs; alternate-screen redraws stay in their event journal, not ordinary scrollback. Search/copy use stable fragment/codepoint positions and cross-page overlap.

## Exact core integration points

- Grid::scroll_up in vendor/alacritty_terminal/src/grid/mod.rs resets rotated rows. Borrow rows before reuse when oldest history exceeds max_scroll_limit. Retirement count is P−min(P,M−H) when positive; preserve chronological order.
- Width shrink in grid/resize.rs reflows retained rows then truncates oldest overflow at reversed.truncate(max_scroll_limit+lines). A scroll-only hook loses these rows.
- Grid::clear_history and update_history remove history; emit explicit epoch/retirement records before shrink.
- Normal and alternate grids both resize; only normal reflows, including while inactive. Route by normal-grid identity, not current/inactive slot.
- Audit grow_columns and height-shrink removals separately: a generic Storage::shrink_lines hook wrongly classifies bottom-screen/reflow bookkeeping as old scrollback.
- Do not accumulate a Vec of retired rows; synchronously pass borrowed rows into a bounded page encoder.

**Unsafe constraint:** Storage::swap copies exactly four usize values and debug-asserts Row size. Do not add provenance fields to Row without deliberately replacing that operation and testing release behavior. A synchronized sidecar is the alternative.

Extend the terminal checkpoint with archive schema/profile/root/watermark, deterministic next retirement ordinal, history epoch, bounded unfinished-line state and archive/hot continuation provenance. Writers/handles are runtime-only. Derive identities from generation+event sequence+ordinal; random replay IDs would destroy equality.

## Durable storage protocol

Proposed starting bounds requiring measurement:64MiB raw journal segments,8MiB archive segments,256KiB decoded pages, bounded encoding/metadata queues. Large rows/extras must fragment without silent truncation. Index epoch/fragment ranges to segment/page offsets. One existing SQLite writer must remain authoritative; actors must not open competing writers. Checkpoints refer to a stable root and watermark, never an ever-growing list of every segment.

For eventN: raw journal append+fsync → apply sole model/generate bounded provisional pages → archive data+commit-marker fsync → index/reference transaction → publish frame/archive watermark. A checkpoint may reference only committed archive data. If derived storage fails after raw durability, enter explicit degradation/backpressure and reconstruct later from the previous committed root and tail; do not publish an incomplete archive or accumulate unbounded RAM backlog.

Crash cases: raw-only reconstructs tail; durable pages without metadata register idempotently using deterministic IDs; invalid references reject that root and preserve originals; partial pages/events use valid prefix; orphan regeneration cannot duplicate history. Fast startup loads bounded root dependencies and lazy pages, never scans the whole archive. Raw data remains intact until separate removal/compaction proof. Segment receipts should bind exact sequence/offset/checksum and profile/generation.

## Immediate findings and repairs

The review found that only Output payload bytes advanced checkpoint cadence, and a separate timer branch bypassed common post-apply handling. Root has unified significant events and added all-event bytes/count/model-work thresholds plus five-second idle publication. These are scheduling repairs, not proof of the final2s long-history budget. Regression tests cover quiet synchronized output and resize-only tails.

The review found Storage::checkpoint_rows cloned the complete ring/cache before exporting another DTO vector. The storage lane replaced that with borrowed canonical active-row iteration and verified old-algorithm byte equality. This removes one duplication without changing schema or claiming a global heap bound.

## Remaining aggregate memory risks

The128MiB encoded cap does not bound total live/decoded heap. Export builds a full cell DTO; import overlaps encoded input, DTOs and reconstructed grids. Per-grid16,777,216-cell and aggregate33,554,432-combining-char admission limits are not a whole-product budget. Live combining marks grow through Vec::push, and width shrink builds reflowed rows before truncating overflow. Twelve120-column/10k-row sessions contain roughly14.45million base cells before extras, alternate grids, frames and temporary recovery allocations.

Required: measure actual Cell/row capacities; shared allocation/admission budget across both grids/pools/buffers; a bounded global checkpoint/recovery scheduler; streaming shrink-overflow; budget live variable extras before allocation with explicit degradation/raw retention instead of truncation. Cold history alone does not solve these peaks.

## Next implementation and acceptance

1. Adopt an explicit archive/reflow ADR; add deterministic borrowed retirement hooks and in-memory streaming oracle while proving hot semantics unchanged.
2. Add bounded archive segments/index/commit watermarks, crash recovery and checkpoint roots; compare canonical archives plus complete hot/parser checkpoints.
3. Expose host history pages, copy and cancellable cross-page search.
4. Test >10k rows, shrink/grow/height, long soft-wrap line, wide/combining boundaries, inactive normal grid, clear/reset epochs, dynamic palette/OSC8, partial escapes and timer flush.
5. Inject failures at every raw/archive/index/checkpoint boundary; preserve corrupt/missing/orphan files; record bytes actually read by fast recovery.
6. Execute four1GiB histories with100k resizes each, VM reboot,12-session RSS/GPU budgets and a separate24h real-time soak. None has been completed by this review.
