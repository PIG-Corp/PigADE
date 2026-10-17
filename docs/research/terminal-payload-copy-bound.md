# Terminal payload-copy bound (research)

This note derives coarse per-event **logical payload-copy** envelopes for
combining marks, OSC 8 hyperlinks, and title metadata from the pinned parser and
terminal model. It is groundwork only: the counters do not authorize a
production resource limit. See the broader [admission plan](terminal-admission-plan.md)
and [measured allocation census](../reports/terminal-memory-census.md).

## Combining marks

Require an unsaturated `ResourceCensus` and a complete pre-event
`AllocationCensus`; otherwise no bound may be used. Define:

- `Z`: `ResourceCensus.zero_width_chars`, including REP repetitions.
- `I`: `ResourceCensus.input_chars`, also including REP repetitions.
- `A`: `terminal_attribute_callbacks`; counting every SGR attribute is
  conservative because only underline-color/reset cases mutate `CellExtra`.
- `H`: `hyperlink_updates`, counting close callbacks too.
- `L`: pre-event `max_single_extra_zero_width_chars_len`.
- `C = size_of::<CellExtra>() + 2 * size_of::<usize>()`; the latter is the
  census's approximate Arc header, not a guaranteed allocator layout.

`push_zerowidth`, underline-color mutation, and hyperlink mutation can each
call `Arc::make_mut`; each such call can copy a shared extra once. The event may
first add marks and then mutate the extra, so any source vector has at most
`L + Z` logical chars. A conservative ordinary COW count is
`K = Z + A + H`, with logical copied payload at most
`K * (C + 4 * (L + Z))`. Add `4 * Z` for appended mark payload. This is a gross
copy/allocation-traffic envelope: some calls are no-ops or unique-Arc mutations,
and copied extras may later be dropped. `write_at_cursor` shares the cursor
template extra into cells without copying it; later mutations of those aliases
are covered by the per-mutation `K` bound. See [`Cell::push_zerowidth` and
CellExtra](../../vendor/alacritty_terminal/src/term/cell.rs#L137-L207),
[`Term::input`](../../vendor/alacritty_terminal/src/term/mod.rs#L1007-L1110),
and the callback counters in [`resources.rs`](../../vendor/alacritty_terminal/src/term/resources.rs#L20-L120).

Example: a valid imported state has a shared 4,096-mark extra (`L=4096`). Four
combining callbacks can, in the deliberately pessimistic distinct-target case,
copy up to `4 * 4,100 * 4 = 65,600` logical mark bytes, plus four `CellExtra` / Arc
payloads and 16 appended bytes. Four marks in one unique cell usually cost far
less; parser census cannot identify target cells or alias counts at callback
time. This illustrates checkpoint-sharing amplification, not a claim that the
same state is reachable from ordinary ANSI output.

### `clear_wide`

Before the repair, `clear_wide` used `Arc::make_mut` and then replaced the
cloned mark vector with an empty vector. Up to two `write_at_cursor` calls occur
for one input callback (wide glyph plus spacer), so before-repair transient
mark-copy traffic is conservatively bounded by `8 * I * (L + Z)` bytes. The
current [`Cell::clear_wide`](../../vendor/alacritty_terminal/src/term/cell.rs#L209-L228)
keeps the unique-Arc fast path and, for shared extras, creates an empty-marks
extra while preserving underline color and the shared hyperlink Arc. Its mark
copy term is therefore zero; at most `2 * I` small `CellExtra`/Arc replacements
remain if shared attributes must survive. This repair removes the discarded
mark-vector copy, not all input-driven COW or other model allocations.

## OSC 8 links

Let `U=hyperlink_uri_bytes`, `J=hyperlink_id_bytes`, and `H` be hyperlink
callbacks. Every nonempty set creates a fresh `HyperlinkInner`; cells and cursor
templates share its Arc, while each retained inner owns its strings. A coarse
gross retained logical payload bound is
`U + J + 27 * H + H * (size_of::<HyperlinkInner>() + 2*sizeof::<usize>())`.
The 27-byte term allows the terminal-generated anonymous ID (20 decimal digits
plus `_pigade`) for every callback; `H` also includes closes, so this
overcounts inner objects. The parser URI is moved into the inner. Explicit IDs
are converted with `to_string`, causing an additional temporary logical copy
of at most the largest ID for one callback at a time. See
[`Term::set_hyperlink`](../../vendor/alacritty_terminal/src/term/mod.rs#L1908-L1920),
[`Hyperlink`](../../vendor/alacritty_terminal/src/term/cell.rs#L44-L130), and
the deduplicating existing-state census in
[`resources.rs`](../../vendor/alacritty_terminal/src/term/resources.rs#L774-L881).

The existing counters do not distinguish absent/invalid IDs from explicit IDs,
or nonempty sets from close callbacks. Add those callback distinctions and the
terminal's next anonymous-ID value if tighter accounting is needed. VTE caps
each OSC capture at 64 KiB, but aggregate output may contain many OSCs
([`MAX_OSC_CAPTURE`](../../vendor/vte/src/lib.rs#L44-L50)).

## Titles

Let `M=max(pre-event AllocationCensus.max_title_len_bytes,
ResourceCensus.max_title_bytes)`, `P=title_pushes`, `Q=title_pops`, `S=title_set_callbacks`,
and `n=pre-event title_stack_count`. A successful pop can occur at most
`Q'=min(Q,n+P)` times (reset can only reduce that). Push clones the current
title; a successful pop clones the popped title into current state; each set
callback clones its title into current state. Thus gross logical string-copy
traffic is conservatively bounded by `(P + Q' + S) * M`. Reset/empty-title
callbacks and capacity reuse make this an overestimate. Current title length
and each stack entry are at most `M`; stack depth is capped at 4,096. Baseline
title capacities and stack count are reported by `AllocationCensus`, but
callback ordering and successful-pop count are not known. See
[`Term::set_title/push_title/pop_title`](../../vendor/alacritty_terminal/src/term/mod.rs#L2264-L2298).

VTE's 64 KiB per-OSC cap bounds each incoming title, not titles restored from a
checkpoint. The formula covers terminal-model string copies only. Parser OSC
scratch, incoming strings, title listener events, queued event retention,
`String` capacity growth, and allocator rounding are additional/transient
costs. A complete post-event census can measure resulting owned capacities;
it cannot predict them before mutation.

## Admission limits

These formulas concern logical payload bytes and gross copies, not exact
retained delta, `Vec`/`String` capacities, heap requests, allocator metadata, or
RSS. Rust collection capacity growth and allocator rounding need a separately
proven bound or an explicit growth policy with checked reservations. Counters
also lack callback ordering, target cell identity, exact successful title
pops, OSC ID absence, and alias state at mutation time. Resize/reflow, grid
storage, parser/checkpoint transients, event queues, and other terminal fields
are outside this note. Do not enable a production profile or claim a whole-event
memory limit from these formulas alone.
