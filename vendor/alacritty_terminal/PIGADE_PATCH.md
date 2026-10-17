# PigADE semantic-state experiment

Upstream: crates.io `alacritty_terminal` **0.26.0**, Apache-2.0 (unaltered
LICENSE-APACHE retained). Crate archive SHA-256:
`bda177466b9524d59f1b12f0dd30b68696788e9992a7e959021c4a0ed96fcf59`.
Registry VCS provenance: Alacritty commit
`94e7c8874e526b1e67b349d9ba30ddf81669119e`, `alacritty_terminal/`.
No Zed terminal implementation or GPL code is included. Product licensing remains
a separate owner decision.

Current profile: `alacritty_terminal/0.26.0+pigade-state.2`, **wire schema 2**.
This incompatibly replaces the first experimental state.1/schema 1 format. The
Rust DTO name `TermCheckpointV1` is retained for source integration; its embedded
schema/core identify actual compatibility. Package version retains upstream's
identity. No migration or raw-history deletion is implied.

## Semantic API

`Term::export_checkpoint()` exports every semantic Term field to a versioned DTO.
The caller MUST call `validate()` before encoding/publication, then enforce an
encoded-byte budget while writing. `Term::from_checkpoint(dto,event_proxy)`
validates before core construction, emits no listener events and recreates full
render damage. Listeners and allocation caches are runtime dependencies.

`TermCheckpointRef::new(&term)` validates a borrowed serialization view with the
same schema2 bytes, canonical row/cursor pool order and logical-column policy.
It streams cells and borrows combining/title/link data rather than allocating
full owned grid/payload DTOs. Identity maps and an encoded output buffer still
allocate; this is not a global workspace budget. Keep source validation aligned
with DTO validation. Byte-parity tests include all45 reference recordings at
three cuts, distinct cursor extras, ring history and grown backing-row tails.

Both grids include active history/visible rows, row occupancy, flags (including
soft wrap), geometry, history/display offsets, current/saved cursor templates,
all character sets and pending wrap. The ring offset is normalized; unused
resize allocation cache is omitted. The DTO also includes tabstops, modes,
scrolling region, all 269 dynamic colors, cursor style, title/title stack, both
keyboard stacks, full Config, focus, vi cursor, selection and the deterministic
per-Term anonymous hyperlink counter. Explicit hyperlink IDs remain intact.
Upstream's standalone `cell::Hyperlink::new(None,...)` behavior is unchanged.

Canonical row export borrows the active ring rows directly: allocation tail at
`zero`, then allocation head, limited to the active length. It no longer clones
the entire row storage (including the inactive resize cache) before truncation.
The generated row DTOs and canonical order are unchanged, so this allocation
repair does not change schema 2 or the state.2 profile. `Row` layout and the
upstream four-word storage swap implementation are untouched. This removes one
intermediate grid/cache clone; it does not establish a whole-process heap bound.

**Schema 2 pools extras and hyperlinks across both grids AND all cursor
templates.** Cells contain extra indices; extras contain hyperlink indices.
Export caches `Arc` identity so a large URI is not cloned or hashed per cell.
A secondary hyperlink content interner merges identical `(id,uri)` values from
separate allocations. Import recreates pooled `Arc`s and preserves sharing;
canonical traversal produces identical serialized bytes on re-export.

The VTE Processor is a separate checkpoint. Its parser, partial UTF-8/CSI/OSC,
REP character, synchronized bytes and logical timeout must be captured at the
same generation/sequence. PigADE's separate VTE patch and terminal envelope
compose them. Upstream Grid serde, a frame or screenshot is not a substitute.

## Bounded decoding and limits

All checkpoint sequence decoders reject excessive known size hints BEFORE
allocation or reading the first element, enforce iteration limits when hints
are absent, and reject an extra element before decoding it. Vectors are not
preallocated from untrusted size hints. In particular:

- At most 10,512 rows per grid; at most 4,096 cells per row and 16,777,216 cells
  per grid, enforced by seeds sharing the grid cell budget while decoding rows.
- A Term shares an18,874,368-cell decode budget across both grids, while keeping
  the per-grid cap. Every valid generic pair fits: at most16,777,216 normal cells
  plus the maximum512×4096 alternate viewport. Each grid validates before the
  next one is decoded. This remains a count bound, not a byte/RSS envelope.
- Validated visible geometry: 1..512 rows, 2..4096 columns; history capacity at
  most 10,000. Normal history capacity must match Config; alternate history is
  zero. The PigADE wrapper has tighter 512x1024 geometry and its own profile.
- Extra pool length must equal the actual canonical referenced indices across
  both grids/cursor templates; hyperlink pool length likewise comes from extra
  references. Pool gaps, unused entries and out-of-range indices are rejected.
- Tab count at most 4096, palette exactly 269, title/keyboard stacks at most
  4096 entries, each core/profile/title/URI/ID/config string has a byte ceiling.
  Core identity is limited to 128 bytes; other text is limited to 64 KiB.
- Combining characters have an **aggregate** 33,554,432-character budget per
  Term, shared while decoding the extra pool. There is no 4096-character limit
  per cell; a valid cell containing 4097 combining marks roundtrips unchanged.
  The same limits are checked by `validate()` on export. Over-budget exports
  fail explicitly; no data is truncated. Raw journal fallback remains required.

Canonical binary structure order is fixed. The diagnostic map/JSON form requires
grids before extras and extras before hyperlinks so dependent pool lengths are
known before allocation. Generic reordering of such diagnostic maps is not a
supported wire transformation. Borrowed binary strings are checked before owned
allocation; a JSON decoder can itself allocate escaped-string scratch before
calling a serde visitor. The outer decoder must still bound encoded bytes.

**These are field/count bounds, not a proven global process heap budget.**
Two large valid-shape grids, cell restoration, index bitmaps, vector capacities
and existing models can still create high transient memory. No shared aggregate
byte allocator budget is implemented here. The upstream live model also permits
long combining sequences; bounding live collection growth without silent Unicode
loss remains a gate. Malformed semantic cell-flag combinations need more fuzzing.
Do not infer the product's 700 MiB/long-history budgets from these fixes.

## Verification

From the repository root:

```sh
cargo test --manifest-path vendor/alacritty_terminal/Cargo.toml --lib \
  --config "patch.crates-io.vte.path=\"$PWD/vendor/vte\""
cargo check --manifest-path vendor/alacritty_terminal/Cargo.toml \
  --no-default-features \
  --config "patch.crates-io.vte.path=\"$PWD/vendor/vte\""
```

State.2: **146 unit tests passed**, including 132 unchanged upstream tests and
14 checkpoint regressions. All 45 upstream captures are checked at 1/3, 2/3 and
end cuts (135 comparisons): continuous replay equals composed parser+Term
checkpoint/tail, with additional continuation beyond capture endpoints. The
suite also covers both screens/cursors, resizing/history wrapping, tabstops,
margins/attrs/modes, stacks/colors/charsets, selection/focus, links and invalid
state. Import emits no listener effects. No-serde check passed.

Specific new regressions: a 37,001-byte long-OSC8/4200-character fixture produces
75,149 checkpoint bytes at 40x12, roundtrips exactly and continues identically;
4097 combining marks roundtrip; repeated explicit hyperlink IDs re-export
canonically. A malicious five-million-row count is rejected before the first
row is requested; a small Postcard count header is rejected without constructing
its advertised body. These are measured component tests, not product budgets.
Borrowed row export is checked against the previous clone/truncate oracle using
complete serialized rows (including occupancy) across every ring offset and
active length in a small ring, plus real cache growth/reuse, both rotations,
shrink/truncation and empty storage. A non-Clone cell regression also checks that
yielded rows refer to the original allocation.

An additional run of the unmodified upstream `--test ref` target passes 44/45.
Its `hyperlinks` fixture expects upstream anonymous IDs `0_alacritty` and
`1_alacritty`, while this existing deterministic per-Term experiment emits
`0_pigade` and `1_pigade`. The borrowed-row repair does not change those IDs or
alter upstream fixtures. The composed checkpoint corpus above includes that
capture and checks live/checkpoint/tail equality under the experimental profile;
this is separate from passing an unchanged upstream anonymous-ID snapshot.

Strict crate-local Clippy on Rust 1.98.1 identifies three unchanged upstream
`clippy::question_mark` cases in `src/tty/unix.rs:135/143/151`. Running with
`--force-warn clippy::question_mark -D warnings` keeps that precise baseline
visible and checks other diagnostics strictly. No unrelated upstream cleanup is
included merely to silence a new lint. Workspace dependency lint caps apply.

Composed publication/crash/fault recovery, cold-history indexing/compaction,
4x1GiB/100,000-resize recovery, VM reboot, 24-hour real-time soak, provider matrix,
platform/renderer/accessibility and performance gates remain independent required
proof. This experiment neither approves a production stack nor authorizes
removing raw history.

## Profile-aware upstream reference fixture

The first full `--test ref` run passed44/45: the hyperlink capture expected process-global anonymous IDs `0_alacritty`/`1_alacritty`, while this maintained profile deliberately uses checkpointed per-Term IDs `0_pigade`/`1_pigade`. Root adapted only those six generated-ID occurrences in tests/ref/hyperlinks/grid.json. Explicit user IDs, URI strings, ordinal distinction, cells, attrs and every other golden field remain unchanged; no comparison was skipped or weakened. The original JSON is reconstructed by reversing those two exact substitutions. SHA-256 original:06c611d988bc376b05af48bbe4a83244ef62202a6ae71676b75a9759e8e90214; profile fixture:442de602c7cc1021397027225e60d5e3c32e4756244a95f4fe6711cd9f4924b8. Fresh full reference run:45/45. This is a documented downstream fixture adaptation, not a claim that unmodified upstream snapshots use PigADE IDs.

## Experimental borrowed history notifications

A runtime-only HistoryObserver follows the normal grid, including while alternate screen is active. Borrowed rows are reported before full-history scroll reuse, width-reflow overflow, history-limit reduction, clear and reset. Reset reports its retained history+viewport once and suppresses the internal clear's duplicate epoch. Observer fields are skipped by serde and rebuilt unattached by checkpoint import; Row memory layout and Storage::swap are unchanged. No production archive is enabled by this API and no existing hot-state semantics/profile change is intended. Grid regressions compare chronological retirement plus retained cells to a larger-history reference at the retirement boundary; composed tests compare complete hot/parser checkpoints with/without an observer. See ADR0005 for the bounded-hot/archive distinction and remaining acceptance work.

Retirement slice verification (2026-10-17): five Grid regressions additionally
cover height growth and width-growth cursor overflow without spurious archival;
the composed wrapper crosses the10k hot-row boundary and compares complete hot
Term/parser bytes with an observer, including inactive normal resize, clear and
reset. This adds five core tests to the previous146, without changing schema2.
Production sinks/encoding/roots are still absent.

## Parser-derived resource census primitives

`term::resources` advances a clone of the pinned VTE Processor with a counting
Handler and reports input callbacks, zero-width callbacks, delivered title and
hyperlink byte counts, and title-stack pushes. REP is counted through the
Processor's `input_repeated` callback, and synchronized timeout census drains
only the cloned pending buffer. The original parser and terminal are
untouched. This is census data, not a heap measurement or admission policy.
Cloning allocates for existing pending parser buffers (bounded by the VTE
2 MiB synchronized-update and 64 KiB OSC limits); it does not clone the Term
grid. No CORE_PROFILE, checkpoint schema, or live terminal behavior changes.

## Host archive integration (2026-10-17)

The statement above describes the earlier retirement-only slice. The host now
attaches a PIGROW02/PIGARC01 sink with a generation-bound fixed-slot index and
PIGCP002 checkpoint roots (ADR0006). The vendor API additionally exposes borrowed
row occupancy without changing Row's memory layout, and forwards dynamic palette
updates to the normal-grid observer, including while alternate screen is active.
Observer attachment provides the current palette before any retired row. These
runtime notifications remain outside the schema2 hot checkpoint wire format.
Archive durability and complete hot/parser/cold-record comparisons live in the
host tests; full resource, platform and release gates remain open.

## Bounded width-shrink scratch (2026-10-17)

Width shrink previously materialized every reflowed row, then retained only
history-limit plus visible rows. Dense 1024-column rows narrowed to two columns
could therefore create millions of intermediate rows. Shrink now keeps a rolling
deque of at most `max_scroll_limit + lines` completed rows and retires displaced
rows oldest first. Completed rows release excess cell capacity; Row's fields and
the four-word Storage::swap ABI assumption remain unchanged. Both grids, saved
cursor, occupancy, wrap flags and archive order keep their previous semantics.

A dense Unicode/wide-cell regression compares streamed retirement plus retained
rows with an independently untruncated reference, then trims that reference and
compares hot grid/cursors/display offset and occupancy. Per-row capacity is also
checked. Root ran 152 library tests, 45 reference cases and the no-serde check.
The original grid, one row being split, parser state and checkpoint transients
still consume memory: this is a local scratch bound, not total heap admission.
Live combining marks and aggregate session memory remain open resource gates.

## Bounded persistent allocation census

`term::resources::census_terminal_allocations` reports capacity-based owned
storage estimates for both screens, including inactive cached rows, row and
cell Vec capacities, the four cursor templates, deduplicated `CellExtra` and
hyperlink Arc payloads, titles/title-stack storage, tab stops, both keyboard
mode stacks, damage-line storage, and semantic escape chars.
Callers set cell-visit and unique-Arc limits; reaching either limit or a checked
arithmetic/allocation failure marks the result incomplete. The census does not
clone or mutate the model and is not wired into output processing or admission.
Its scoped payload-capacity estimate includes inline Term size and a two-`usize`
Arc-header estimate, but excludes allocator metadata, alignment, fragmentation,
generic listeners, history observers, parser state and transient copies. A
complete scan is not an upper bound on terminal or process memory and makes no
claim about physical allocator overhead or heap/RSS usage.
It additionally reports CellExtra reference count, per-reference-weighted mark
capacity (which intentionally counts shared storage once per reference), and
the largest single CellExtra mark capacity. A bounded test constructs a shared
marks Arc in a caller-built Term, round-trips it through a valid checkpoint,
then uses the pinned VTE Processor to mutate two cells and exercise copy-on-
write. This demonstrates a conservative accounting case, not that ordinary
ANSI output from a fresh terminal creates that sharing pattern.

The parser-derived census also counts title-set and title-pop callbacks,
maximum individual title payload, hyperlink callback count and maximum
individual identifier/URI payloads, and the callback counts for device
attributes/status, public/private mode reports, dynamic colors, text-area
sizes, keyboard mode, modify-other-keys, and OSC 52 clipboard-load queries.
Dynamic-color prefix/terminator and clipboard-load terminator bytes are
reported as payload counts. These callbacks are observed by advancing the
same cloned pinned VTE Processor; the census does not run Term handlers,
enqueue or answer effects, or invent response-byte bounds. Fixtures cover
partial OSC continuation, synchronized-timeout drain, title push/pop
amplification, repeated long OSC 8 updates, query-only CSI/OSC storms, and
unchanged original processor checkpoints after census.
