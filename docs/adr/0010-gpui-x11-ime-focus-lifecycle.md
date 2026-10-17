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
