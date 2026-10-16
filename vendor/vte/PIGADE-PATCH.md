# PigADE downstream patch, 2026-10-17

Source: crates.io vte 0.15.0, upstream commit `3b3da71c34cc1256c7e20981cf03f8eb95e08ffc`.
Original MIT / Apache-2.0 license files retained. No Zed terminal code is included.

`Parser::advance_partial_utf8` emits only the first scalar when its four-byte temporary buffer contains one complete scalar, ASCII and the beginning of another scalar. Its Err(valid_up_to > 0) branch formerly consumed **all** valid bytes, dropping the ASCII suffix. Consume `c.len_utf8() - old_bytes`, matching the existing Ok branch.

Reproducer: bytes of `é 界`, chunks `[0xc3]`, `[0xa9,0x20,0xe7,0x95,0x8c]`; expected `é 界`, original `é界`. `cargo test -p pigade-terminal` exercises every fixed chunk size of the larger Unicode/ANSI fixture; the minimal case and each cut point are also checked. Upstream source remains in the research report. This patch fixes byte consumption; it does not implement checkpoints or bounded OSC storage.

## Versioned recovery extension

The later `pigade-state.1` profile adds explicit ParserCheckpointV1 and ProcessorCheckpointV1<T> export/import, including private parser state, parameters/intermediates, partial UTF-8, OSC, REP and synchronized-update bytes plus logical timer state. These are maintained downstream APIs, not upstream full-checkpoint claims. Terminal import validates incomplete UTF-8 and parameter group/pending-subparameter consistency before continuation; malformed probes previously caused a panic or nontermination.

OSC parsing is capped at64KiB; oversized OSC is ignored through its terminator, while the host journal retains the original raw bytes. This explicit profile choice bounds parser memory. Imported OSC/synchronized byte vectors check bounds before allocation (sync budget2MiB plus64KiB). Live timers are represented as ordered durable host events. Replay never calls terminal side effects.

Reproduce parser regressions with the standalone vendor test command recorded in docs/reports/checkpoint-review.md and host adapter tests with `cargo test -p pigade-terminal`. Complete terminal recovery combines these APIs with the separately patched alacritty Term/Grid state; no parser-only test certifies the full gate. Keep upstream licenses and identities intact.

## Cloned processor and REP callback

The maintained ANSI Processor and its bounded parser state are cloneable for a
parser-derived census pass. `Handler::input_repeated` is used for REP; its
default implementation calls `input` once per repetition, preserving existing
handler behavior. Counting handlers may override it to aggregate large REP
counts without expanding a callback vector. This API does not add REP limits or
change terminal semantics.
