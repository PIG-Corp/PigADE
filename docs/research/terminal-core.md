# Terminal core research — stage 0

Research date: 2026-10-17. Status: source inspection and isolated negative experiment complete; **stack gate not passed**. SPEC.md and DESIGN.md were read in full. This report covers upstream evidence, not product acceptance or a dependency adoption decision.

## Version and license evidence

The crates.io API and release archives were fetched on the research date. These were the latest stable published versions returned then; that statement is time-bound. Published `.cargo_vcs_info.json` supplies the source revisions below.

| Component | Exact published version | Upstream revision | Declared license | Rust version declaration |
| --- | --- | --- | --- | --- |
| alacritty_terminal | 0.26.0 | `94e7c8874e526b1e67b349d9ba30ddf81669119e` | Apache-2.0 | 1.85.0 |
| vte | 0.15.0 | `3b3da71c34cc1256c7e20981cf03f8eb95e08ffc` | Apache-2.0 OR MIT | 1.62.1 |
| portable-pty | 0.9.0 | `f8921727a11b9f8b073e8c24821d72fd41283500` | MIT | not declared |
| gpui | 0.2.2 | `69e2130295c2649963eb639fc70b4f2ee8ea1624`, **dirty publication** | Apache-2.0 | not declared; edition 2024 |

Authoritative release metadata: [alacritty_terminal](https://crates.io/api/v1/crates/alacritty_terminal/0.26.0), [vte](https://crates.io/api/v1/crates/vte/0.15.0), [portable-pty](https://crates.io/api/v1/crates/portable-pty/0.9.0), [gpui](https://crates.io/api/v1/crates/gpui/0.2.2). Archive SHA-256 checksums, suitable for verifying downloaded source artifacts:

```text
alacritty_terminal 0.26.0 bda177466b9524d59f1b12f0dd30b68696788e9992a7e959021c4a0ed96fcf59
vte               0.15.0 a5924018406ce0063cd67f8e008104968b74b563ee1b85dde3ed1f7cb87d3dbd
portable-pty      0.9.0  b4a596a2b3d2752d94f51fac2d4a96737b8705dddd311a32b9af47211f08671e
gpui              0.2.2 979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707
```

`alacritty_terminal` explicitly depends on `vte ^0.15.0` with `std,ansi`, and `unicode-width ^0.2.0`; those are ranges, so reproducibility also requires Cargo.lock and a recorded width profile. The source manifest does not prove identical behavior under a future resolution. [Pinned manifest](https://github.com/alacritty/alacritty/blob/94e7c8874e526b1e67b349d9ba30ddf81669119e/alacritty_terminal/Cargo.toml#L14-L33).

The GPUI release archive is not proven byte-equivalent to its advertised commit: its VCS metadata contains `dirty: true`. The clean commit exists, its GPUI manifest says 0.2.2/Apache-2.0, and it can be named by an immutable Git `rev`. A source pin candidate is therefore:

```toml
# Candidate for a separate native spike; not an approved production pin.
gpui = { git = "https://github.com/zed-industries/zed", rev = "69e2130295c2649963eb639fc70b4f2ee8ea1624" }
```

This was verified by fetching the clean commit's manifests, not by compiling that Git dependency. It uses workspace dependencies, so switching from the published archive to Git must resolve and check the entire actual dependency graph. [Clean GPUI manifest](https://github.com/zed-industries/zed/blob/69e2130295c2649963eb639fc70b4f2ee8ea1624/crates/gpui/Cargo.toml#L1-L99), [published VCS metadata](https://docs.rs/crate/gpui/0.2.2/source/.cargo_vcs_info.json).

The GPUI README warns of pre-1.0 breaking changes. Linux backend feature declarations include Wayland and X11; this is implementation availability, not PigADE platform certification. The pinned Wayland source imports fractional-scale and text-input-v3, and the X11 source has XIM and clipboard handling. Their presence does not establish correct IME composition, EN/RU mapping, multi-monitor transitions, accessibility or device-pixel geometry in PigADE. [README](https://github.com/zed-industries/zed/blob/69e2130295c2649963eb639fc70b4f2ee8ea1624/crates/gpui/README.md#L8-L17), [Wayland](https://github.com/zed-industries/zed/blob/69e2130295c2649963eb639fc70b4f2ee8ea1624/crates/gpui/src/platform/linux/wayland/client.rs#L42-L56), [X11 IME](https://github.com/zed-industries/zed/blob/69e2130295c2649963eb639fc70b4f2ee8ea1624/crates/gpui/src/platform/linux/x11/client.rs#L1268-L1339).

Zed's terminal integration is a separate GPL-3.0-or-later crate. Reading it does not license copying it under GPUI's Apache license. No Zed terminal implementation was copied during this research. This is a manifest inventory, not a complete distribution-license audit of all transitive dependencies/assets. [Pinned terminal manifest](https://github.com/zed-industries/zed/blob/69e2130295c2649963eb639fc70b4f2ee8ea1624/crates/terminal/Cargo.toml#L1-L6).

## What serialization actually preserves

`Term<T>` has no full Serialize/Deserialize implementation. Its private state includes both grids, active charset, tab stops, modes, scroll region, dynamic colors, cursor style, title/title stack, both keyboard stacks and config. `event_proxy` is a runtime dependency and must be rebound; damage is renderer bookkeeping and must be invalidated on restore. A visible-cell dump is insufficient. [Term definition](https://github.com/alacritty/alacritty/blob/94e7c8874e526b1e67b349d9ba30ddf81669119e/alacritty_terminal/src/term/mod.rs#L268-L363).

Even `Grid<Cell>`'s existing serde deliberately **skips current and saved cursor**. A cursor also carries its template cell, charset mapping and `input_needs_wrap`. Restoring cells while silently defaulting this state changes subsequent output. [Grid and Cursor](https://github.com/alacritty/alacritty/blob/94e7c8874e526b1e67b349d9ba30ddf81669119e/alacritty_terminal/src/grid/mod.rs#L33-L138).

`vte::Parser` has private state for the state-machine state, CSI intermediates/parameters, partial OSC, ignored-sequence state, and partial UTF-8 bytes/length. It derives Default, not Serialize. `ansi::Processor` additionally holds `preceding_char` for REP and synchronized-update state. The terminal and parser together are the continuation state. [Parser](https://github.com/alacritty/vte/blob/3b3da71c34cc1256c7e20981cf03f8eb95e08ffc/src/lib.rs#L49-L70), [Processor](https://github.com/alacritty/vte/blob/3b3da71c34cc1256c7e20981cf03f8eb95e08ffc/src/ansi.rs#L242-L283).

Synchronized output buffers bytes outside the terminal grid. Upstream has a 2 MiB sync buffer limit and a 150 ms timeout; `StdSyncHandler` stores `Option<Instant>`. Persisting an absolute process-local Instant across reboot is not a valid timer model. `Processor<T: Timeout>` exposes a custom timeout type and `stop_sync()`, allowing an event-stream-controlled timer without implementing a new ANSI parser. Host ordering must durably record the timeout firing before applying it, and replay must apply that recorded event rather than elapsed wall time. [Sync constants](https://github.com/alacritty/vte/blob/3b3da71c34cc1256c7e20981cf03f8eb95e08ffc/src/ansi.rs#L34-L40), [stop_sync](https://github.com/alacritty/vte/blob/3b3da71c34cc1256c7e20981cf03f8eb95e08ffc/src/ansi.rs#L314-L359), [Timeout contract](https://github.com/alacritty/vte/blob/3b3da71c34cc1256c7e20981cf03f8eb95e08ffc/src/ansi.rs#L440-L490).

Two extra hazards need explicit coverage:

- OSC 8 links without supplied IDs allocate a process-global atomic suffix. Replaying in the same process or interleaving sessions can change generated IDs. The comparison must preserve hyperlink equivalence/grouping without mistaking allocator IDs for semantic identity, or a fork must use deterministic generation-local allocation. Restoring a global counter per session is unsafe. [Hyperlink allocation](https://github.com/alacritty/alacritty/blob/94e7c8874e526b1e67b349d9ba30ddf81669119e/alacritty_terminal/src/term/cell.rs#L39-L101).
- With `std`, partial OSC uses an unbounded Vec; the capacity check exists only for `not(feature="std")`. Bounded host channels do not alone bound terminal-core memory for a never-terminated OSC. A supported-profile limit or spill strategy needs explicit behavior and tests; preserve raw bytes in the journal. [OSC storage](https://github.com/alacritty/vte/blob/3b3da71c34cc1256c7e20981cf03f8eb95e08ffc/src/lib.rs#L55-L69), [OSC accumulation](https://github.com/alacritty/vte/blob/3b3da71c34cc1256c7e20981cf03f8eb95e08ffc/src/lib.rs#L542-L552).

## Executed negative experiment

An isolated Cargo executable was built and run on this host using exact `alacritty_terminal=0.26.0` and `vte=0.15.0`. No GUI, PTY, network provider or fork was involved. It demonstrates that the tempting serialization path is incorrect; it does not validate a replacement checkpoint.

Observed output:

```text
cursor (2,7) -> (0,0); saved (1,3) -> (0,0)
utf8: live "€"; reset-parser "�"
REP: live "AAA"; reset-parser "A"
CSI: live "R"; reset-parser "mR"
sync: live "ABC"; reset-parser ""
```

Reproduce in an empty temporary Cargo project with this manifest:

```toml
[package]
name = "checkpoint-counterexample"
version = "0.0.0"
edition = "2021"
[dependencies]
alacritty_terminal = "=0.26.0"
vte = { version = "=0.15.0", features = ["ansi"] }
serde_json = "1"
```

Use the following `src/main.rs`, then `cargo run --quiet`:

```rust
use alacritty_terminal::{grid::Grid, term::cell::Cell, index::{Point, Line, Column}};
use vte::ansi::{Processor, Handler};
#[derive(Default)] struct Sink(String);
impl Handler for Sink { fn input(&mut self, c: char) { self.0.push(c); } }
fn main() {
    let mut grid = Grid::<Cell>::new(4, 20, 10);
    grid.cursor.point = Point::new(Line(2), Column(7));
    grid.saved_cursor.point = Point::new(Line(1), Column(3));
    let json = serde_json::to_string(&grid).unwrap();
    let restored: Grid<Cell> = serde_json::from_str(&json).unwrap();
    assert_ne!(grid.cursor.point, restored.cursor.point);
    assert_ne!(grid.saved_cursor.point, restored.saved_cursor.point);
    for (name, prefix, tail) in [
        ("utf8", &[0xe2, 0x82][..], &[0xac][..]),
        ("REP", &b"A"[..], &b"\x1b[2b"[..]),
        ("CSI", &b"\x1b[31"[..], &b"mR"[..]),
        ("sync", &b"\x1b[?2026hABC"[..], &b"\x1b[?2026l"[..]),
    ] {
        let mut live: Processor = Processor::new();
        let mut output = Sink::default();
        live.advance(&mut output, prefix);
        let at_checkpoint = output.0.clone();
        live.advance(&mut output, tail);
        let mut reset: Processor = Processor::new();
        let mut lost = Sink(at_checkpoint);
        reset.advance(&mut lost, tail);
        println!("{name}: live {:?}; reset-parser {:?}", output.0, lost.0);
        assert_ne!(output.0, lost.0);
    }
}
```

The test's Handler records printable output only. It proves parser loss independently of grid loss; it is not a comprehensive terminal-model comparator. The first two assertions separately demonstrate Grid serde loss.

## Smallest plausible recovery extension — unverified proposal

The minimum plausible patch spans **both alacritty_terminal and vte**. Patching only Term or adding serde to Grid is insufficient. An implementation owner must choose this path and verify its maintenance/license implications; research does not approve the fork.

1. Add explicit versioned export/import DTOs inside the upstream crates, where private state is accessible. Export both grids with complete cursor states, all non-derived Term state, VTE parser state, Processor preceding character, sync bytes and logical timeout state. Rebind event proxy and force full render damage on import. Validate all lengths, indices, geometry, enum values and profile/core/schema identifiers before constructing runtime objects.
2. Use a host-owned logical Timeout. Keep pending/remaining scheduling metadata for live recovery; replay only recorded timer decisions. A historical generation is not a running timer. Resize and timer boundaries belong to the same sequence as Output; preserve output chunk semantics used by upstream sync handling.
3. Export immutable cold-history chunks and bounded hot grids. Existing Grid serde includes in-memory storage, not a lazy cold-history index. A checkpoint API alone does not solve archival history or the long-history recovery budget.
4. Differential test live/full replay/checkpoint+tail at every byte boundary, not just at printable-text boundaries. Exercise both buffers, saved cursors, scroll regions, tabs, palette changes, title/keyboard stacks, pending wrap, partial UTF-8/OSC/CSI, REP, sync timeouts, resize and anonymous hyperlinks. Compare future continuations as well as the initial restored frame.
5. Publish via temporary file + fsync + rename + directory sync, then commit the DB reference; inject crashes on each boundary. Retain full replay as a correctness oracle. Do not delete raw events until compaction equivalence and recovery fallback have passed.

Waiting for parser ground state is not a sufficient bounded-recovery strategy: ground state does not remove REP/cursor/mode state, OSC can remain incomplete, and a client can hold sync state. Artificially flushing partial data to create a checkpoint changes terminal behavior. Replay of an unbounded prefix is likewise not a bounded-cost checkpoint.

## PTY integration facts

portable-pty provides `openpty`, cloneable reader, single writer, `resize`, size retrieval, and slave-side `spawn_command`; on Unix resize goes through the PTY size ioctl. These primitives do not implement durable spawn intent, helper registration, generation fencing, boot/start identity, crash reconciliation or output ordering. Those remain host responsibilities. [Trait API](https://github.com/wezterm/wezterm/blob/f8921727a11b9f8b073e8c24821d72fd41283500/pty/src/lib.rs#L88-L165), [Unix resize](https://github.com/wezterm/wezterm/blob/f8921727a11b9f8b073e8c24821d72fd41283500/pty/src/unix.rs#L180-L206).

## Acceptance status and handoff

Verified: release metadata/licenses above, pinned source structure, missing complete serialization, Grid cursor loss, parser-reset counterexamples, existence of candidate GPUI commit. Not verified by this report: native GPUI build, GUI accessibility, Wayland/X11/IME/scaling/clipboard behavior, GPU context loss, complete checkpoint round trip, cold-history implementation, real CLI compatibility, input-to-present latency, long-history recovery, VM reboot or 24-hour soak.

The stage-0 gate therefore remains open. Next implementation experiment: a versioned two-crate snapshot patch plus timer injection and differential continuation corpus, in parallel with a genuine commit-pinned GPUI platform spike and the designated Electron control. Any adoption/alternative decision belongs to the lead engineer's ADR after these results. Do not infer success from screenshots or serialize API names.

## Confirmed upstream partial-UTF-8 defect

A separate stage-0 fixture exposed a defect **without parser replacement or checkpointing**. Independent direct `vte::ansi::Processor` testing reproduces it, eliminating the PigADE cell adapter as the cause:

```text
bytes       c3 a9 20 e7 95 8c  (é 界)
whole       "é 界"
chunks 1,5  "é界"
```

Observed on unmodified published vte 0.15.0. The existing larger terminal test first differs at cell index 8 for chunk size 3: expected a space, observed wide `界`; final cursor can still match after subsequent control sequences. This illustrates why final cursor-only comparison is insufficient.

In `advance_partial_utf8`, the `Err` branch with `valid_up_to() > 0` prints only the first decoded character, then reports consuming `valid_bytes - old_bytes`. If the temporary four-byte buffer contains a completed two-byte character, one ASCII byte, and the start of another multibyte character, the ASCII byte is counted as consumed but never dispatched. The symmetric `Ok` branch already uses the first character's actual UTF-8 length. The proposed narrow fix is `return c.len_utf8() - old_bytes`, followed by exhaustive chunk-boundary regression and invalid-UTF-8 tests. [Published source](https://github.com/alacritty/vte/blob/3b3da71c34cc1256c7e20981cf03f8eb95e08ffc/src/lib.rs#L670-L714).

The fetched upstream master resolved to immutable commit `abeae765dd546dfff60b278f0757dcc71beb8ab1`, dated 2026-02-28, and still contains the same expression; simply switching to that upstream commit does not resolve it. [Inspected upstream](https://github.com/alacritty/vte/blob/abeae765dd546dfff60b278f0757dcc71beb8ab1/src/lib.rs#L694-L703).

Reproducer: [`utf8-regression.rs`](../../tests/fixtures/terminal/utf8-regression.rs), data: [`utf8-regression.json`](../../tests/fixtures/terminal/utf8-regression.json). Copy the Rust file into an isolated Cargo executable using the vte dependency shown in its header and run `cargo run`: upstream must fail the equality assertion. The implementation owner is responsible for the vendored fix and its test evidence; this finding does not establish checkpoint completeness or full Unicode conformance.

## Native source baseline follow-up

The clean GPUI commit's untouched Hello World example subsequently **built successfully** using a standalone lock seeded from upstream Cargo.lock, and mapped a native Wayland surface during a 40-second smoke run. Fresh transitive resolution initially failed in xattr/libc. [Reproduction, exact locks and logs](gpui-source-check/README.md) preserve both outcomes. This updates the earlier build-status checklist only; the native window's pixels/input/accessibility and PigADE's four-terminal gate remain unverified. No release-performance conclusion is drawn from the debug build.
