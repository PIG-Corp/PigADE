# Bundled terminal font verification — 0.0.1-dev

The app embeds five derived OFL faces with unique PigADE aliases. Exact
upstream/derived hashes and unchanged outline/metrics/cmap fingerprints are
recorded in the asset manifests; `prepare-terminal-fonts.py --verify` reproduces
every byte and legal file. The aliases prevent a system copy of the original
font family from silently winning source selection. Unsupported glyphs can
still reach unpinned platform fallback. See [ADR0008](../adr/0008-bundled-terminal-font-identities.md).

Fresh [isolated evidence](../../crates/app/evidence/font-profile-20261017)
uses app SHA-256
`275e47a1b3c1dc68c907c1be794657f2fac36b52ce840bbcd6c9e46c2e66e550`.
Before creating any namespace/window, the harness verifies the exact bytes of
all five expected font assets are present in that executable and match their
manifest. Inside the namespace, successful app startup confirms alias collision
checks and registration passed. The actual app rendered and survived UI
kill/reopen with the same four process/generation identities at both scales:

| Private software output | PNG physical dimensions | Host grid per pane |
| --- | --- | --- |
| 100%, requested 1280×800 logical | 1280×800 | 69×13 |
| 200%, requested 1280×800 logical | 2560×1600 | 73×14 |

Grid differences follow device-pixel rounding of measured font metrics; host
resize acknowledges the resulting dimensions. Root viewed both screenshots:
Cyrillic, combining accent, Han/Hiragana/Hangul with neighboring ASCII and
primary bold/italic samples are readable without obvious adjacent-cell collision.
The same read-only AT-SPI probe passed in both runs. No keyboard/mouse input
was injected. Private namespace/socket/device checks precede startup; no host
display mode, compositor configuration, focus or zoom was changed.

Weston supports a separate output scale; see its
[official output API](https://wayland.pages.freedesktop.org/weston/toc/libweston/output.html).
The pinned Weston15 manual and the actual capture establish this harness's
integer-scale behavior: its headless size arguments are multiplied by scale
for physical capture dimensions. An initial harness attempt multiplied those
arguments twice and obtained 5120×3200, correctly failing the PNG-size assertion.
It is retained outside Git and in the superseded result set.

## Corrected stale-build evidence

An earlier run accidentally selected the old `/tmp` GPUI target executable
`8b24806d…` instead of the newly built app target. Root found the current Noto
font payload absent from that binary. Those screenshots are explicitly marked
**not bundled-font proof** in
[the correction](../../crates/app/evidence/font-profile-stale-build-20261017/CORRECTION.json);
they only show the older renderer. The old record is preserved. The new
preflight rejects this exact class of stale-build error before any window
starts, and Python regressions verify stale binary and stale manifest failures.

The passing runs use `crates/app/target/debug/pigade-app`; alternate build caches
must point at their own freshly built artifact. Tests/builds in one target
directory do not validate a binary in another directory.

These are fixed integer-scale Linux software-rendering fixtures, not native
GPU, fractional scaling, cross-monitor movement, complete ink bounds, all CJK
regional typography, fallback styling, emoji/ZWJ coverage, actual IME, accessible
terminal text or macOS/Windows acceptance. Render-profile persistence alongside
logical session metadata remains open; CORE_PROFILE is unchanged.
