# ADR0010 — Pin a GPUI X11 XIM focus lifecycle correction

Date:2026-10-17. Status:integrated in development; one isolated native Pinyin
commit passes in debug/release. The complete Linux platform gate remains open.

## Evidence and decision

On clean upstream GPUI322cd509dcccaee1f6bc44725d270598ae8171ad, the actual PigADE
window renders on private Xvfb and has verified X focus, but Fcitx5.1.23 reports
an empty current input method after startup settles. The pinned X11 backend
creates ICs before a valid window/IM is ready, creates another IC on each focus,
and never sends SetICFocus/UnsetICFocus. The pinned xim-rs client does not send
these automatically on CreateIC reply. Fcitx activation requires a current input
context. Earlier SDK/profile/harness mistakes were corrected separately and
retained as failed evidence, not attributed to this product defect.

Keep Rust/GPUI and apply the exact two-file downstream profile
`pigade-x11-ime-focus-v1`. The [manifest and patch](../../crates/app/patches/gpui-x11-ime/)
pin the upstream archive, before/after source hashes and patch hash. The source
archive remains SHA27e6a724515cb6e6fa8a456098d075bfa2a50b4f6145491105555e56de2d256d.
The modified gpui_linux component declares Apache-2.0; preserve its upstream
license/notices. No Zed GPL terminal integration or new runtime dependency is
introduced. Product version remains0.0.1-dev.

An explicit state machine separates IM readiness, desired focused window,
pending create/destroy, bound IC and submitted focus request. It creates only
for a ready IM and nonzero focused window, serializes replacement behind
DestroyIC reply, destroys stale create replies, reuses same-window ICs and
orders UnsetICFocus before ResetIC. It forwards keys/commit/preedit callbacks
only for the current locally focused IC. Failed focus submission invalidates
local focus; unknown destroy outcomes are not automatically retried.

Bootstrap applies the profile before Cargo. The applier locks the source scope,
checks the archive and listed preimages, validates all patch paths and exact
source hashes, dry-runs with fuzz0, revalidates, applies and verifies postimages.
All-after is idempotent; mixed/unexpected states fail closed and are retained.
The source is an archive extraction inside PigADE's Git tree, so archive mode
must be used; an ancestor Git HEAD cannot identify the GPUI extraction. The
guard verifies the modified files, not every cached upstream file.

## Verification and limits

Nine tests execute the exact embedded state helper extracted from the patched
source. Native app90 tests, strict Clippy and debug/release builds pass. The
[isolated report](../reports/x11-ime-20261017.md) records a matched unpatched
baseline failure and patched actual-app debug/release native XIM commit passes:
no precommit PTY bytes, exactly six UTF8 bytes for你好, no duplicate/trailing input
and a distinct child ACK. No active desktop was accessed.

Set/UnsetICFocus are one-way requests; local “focused” is not server-acknowledged
state. This correction does not prove same-window PigADE session-generation
composition barriers, all reset/error sequences, other IMEs, EN/RU shortcuts,
Wayland text-input-v3, hardware GPU or cross-platform parity. The observed native
preedit is in Fcitx's popup; inline preedit/caret positioning remains unverified.
No platform/MVP acceptance criterion is waived.

Primary pinned sources: [GPUI XIM handler](https://github.com/zed-industries/zed/blob/322cd509dcccaee1f6bc44725d270598ae8171ad/crates/gpui_linux/src/linux/x11/xim_handler.rs),
[GPUI X11 client](https://github.com/zed-industries/zed/blob/322cd509dcccaee1f6bc44725d270598ae8171ad/crates/gpui_linux/src/linux/x11/client.rs),
[xim-rs request/dispatch](https://github.com/zed-industries/xim-rs/blob/16f35a2c881b815a2b6cdfd6687988e84f8447d8/src/client.rs),
[Fcitx current-context activation](https://github.com/fcitx/fcitx5/blob/27cca6e0239938b2d53937061d0980c087856b22/src/lib/fcitx/instance.cpp).

## Update18.10.2026 — complete XIM style for hover safety

Profile `pigade-x11-ime-focus-v2` retains the lifecycle correction and pairs
`PREEDIT_CALLBACKS` with `STATUS_NOTHING` at IC creation and both XIC attribute
updates. The [Xlib specification](https://www.x.org/releases/X11R7.6/doc/libX11/specs/libX11/libX11.pdf)
defines separate preedit and status interaction categories. Pointer fixtures02/03
showed Fcitx composing `nihao` in its popup with no application preedit: a local
empty-string check could therefore allow hover to change the pane during IME.
The style correction must pass the actual private native fixture; source tests
alone are insufficient evidence of callbacks or safe hover.

Bootstrap accepts a complete exact v1 file set through the manifest's
`previous_files_sha256`. It verifies the upstream archive and successor patch in
a private temporary tree before replacing those exact known cache files.
Unknown or mixed file sets remain untouched and fail closed. An interrupted
multi-file replacement can leave a mixed cache requiring deliberate rebuild;
this migration operates on derived build sources, never user/session data.
The broader native composition/generation barrier limitations above remain.

Follow-up evidence: release pointer04 still showed external preedit after the
style pair correction. [Fcitx5.1.23 XIM config](https://raw.githubusercontent.com/fcitx/fcitx5/5.1.23/src/frontend/xim/xim.h)
defaults `UseOnTheSpot` to false; its [style selection](https://raw.githubusercontent.com/fcitx/fcitx5/5.1.23/src/frontend/xim/xim.cpp)
then omits callback preedit. The backend must validate the advertised style
before creating an IC. Unsupported callback styles use the native keyboard
fallback; silently accepting an opaque popup would leave hover unable to see
composition ownership. Private acceptance configures only its own server HOME
with callback preedit enabled. This does not certify the user's IM configuration
or resolve the delayed target/generation barrier.

The pinned xim-rs dispatcher also omitted the synchronous
`XIM_PREEDIT_START_REPLY`. The [XIM protocol](https://x.org/releases/X11R7.6/doc/libX11/specs/XIM/xim.pdf)
requires that reply. The small [transport profile](../../crates/app/patches/xim-preedit/README.md)
sends it once after a successful handler callback with unlimited preedit length;
handler errors propagate. The existing MIT dependency version and revision stay
fixed, with one hash-checked source patch in a private build checkout and a Cargo
path override. `bootstrap.sh test-xim` executes the patched crate's actual
mock-transport tests; it uses its upstream workspace because Cargo cannot test a
nonmember dependency's dev-dependencies from the app workspace.

## Update18.10.2026 — native file-drop transport

Profile `pigade-x11-ime-focus-v3` preserves v2 XIM behavior and fixes the
`XdndAware` property type. The pinned atom manager interns the literal name
`XA_ATOM`, which is a custom atom, while the [XDND specification](https://www.freedesktop.org/wiki/Specifications/XDND/)
requires the predefined `ATOM`. Two isolated file-drop attempts failed before
data transfer on this mismatch; the native fixture retains its standard type
requirement. The patch now uses XCB's `AtomEnum::ATOM` directly.

Selection reads are bounded to64KiB, sufficient for percent-encoded application
path lists. Nonzero `bytes_after`, invalid UTF-8 and any malformed or non-file
URI reject the entire list. Empty and comment lines are ignored. Invalid data
produces an empty external-path event so the UI can clear the drag and report
failure; no partial valid prefix reaches terminal insertion review. The exact
v2 postimages are the only recognized migration predecessor. Existing hash and
source checks do not replace native file-drop acceptance.

`XdndFinished` reports success only after a complete nonempty path list was
decoded; invalid, truncated, empty or missing payloads return failure with no
action. Success acknowledges the native transfer only. Terminal insertion
still requires the application's explicit review confirmation and ordinary
generation-bound input admission.

Native03 opened the first review but lost a second drop after Escape. Native04
added a visible-modal gate before any confirmation and reproduced the loss
without sending Ctrl+Enter. The pinned GPUI window classifies input modality
before translating file events; `FileDrop` left keyboard modality unchanged,
so `HitboxId::is_hovered` suppressed the drop listener. The third patched file,
`gpui/src/window.rs`, classifies only native Entered/Pending/Submit as mouse
input. Exited/Ended and ordinary paints do not change modality, preserving
keyboard focus under a stationary cursor. The pristine window file hash joins
the exact v2 predecessor set; unknown cache edits still fail closed.

Final18.10 evidence: source-bound12 capability/lifecycle tests and the actual
patched xim-rs2 mock-transport tests pass. Native release pointer07, Pinyin04,
keyboard03 and controls01 pass. Inline preedit and final drop/undo captures were
inspected. Pointer05/Pinyin02 exposed an overly strict requirement that protocol
padding be zero; the typed decoder correctly skips it. The preflight now checks
count/length while accepting unspecified padding. Unsupported external-style
Fcitx has no IC and still delivers the exact31 EN/RU/control bytes through the
ordinary keyboard path. On-the-spot mode commits exactly6 bytes once with no
precommit input. These are private software X11 results, not platform parity or
resolution of delayed target/generation callbacks. The dependency's five existing
warnings remain visible; first-party strict Clippy passes.
