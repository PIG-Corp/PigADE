# ADR 0008: identify embedded terminal fonts independently of system fonts

Status: implemented in development; Linux software-rendered fixtures verified.
Native platform, coverage and persisted render-profile acceptance remain open.

## Problem

The initial renderer used installed Liberation Mono. Loading upstream
JetBrains Mono and Noto files with `add_fonts` did not by itself fix source
selection: pinned GPUI's Linux cosmic-text backend loads system fonts first,
then appends embedded data. Family lookup collects matching names without a
source-priority contract. An installed face with the same family could win a
style/weight match. `all_font_names` and successful resolution alone do not
identify selected bytes.

## Decision

Retain the exact upstream assets, provenance and OFL notices. Generate derived
faces with unique internal families `PigADE Terminal Mono v1` and
`PigADE Terminal CJK JP v1`. Rename OpenType/CFF identity metadata, remove the
Noto file's empty unsigned DSIG table, and preserve outlines, horizontal and
vertical metrics and cmap. A pinned fontTools generator fingerprints these
properties and byte-compares a fresh regeneration, including legal files.
The derived directory carries its own notice pointing at the actual OFL and
authors filenames.

Before registering fonts, the app rejects either alias if already present.
It then embeds/registers four primary static styles and one JP CJK Regular
face, verifies alias presence and resolves the profile. Primary `liga` and
`calt` features are disabled. Host column/row origins remain authoritative;
each shaped cluster is clipped to its assigned one-/two-cell span and pane.
The JP regional glyph choice is explicit; it is not universal Han typography.
Unsupported scripts/emoji may still reach platform fallback and are not
claimed deterministic.

The app render identity is `pigade-terminal-render-profile-aliases-v1`, with
per-face hashes in the derived manifest. It is separate from the existing
semantic `CORE_PROFILE`; no accepted ANSI semantics, width tables, journal or
checkpoint identity change in this slice. The current historical
`font=unfixed-prototype` core string is retained for compatibility with frozen
corpora. Binding the render identity to persisted generation/configuration
metadata and migrating that placeholder remain required before release.

## Evidence and limits

The app's 21 tests, strict all-target Clippy and locked build pass. Fresh font
regeneration is byte-identical and preserves all three font fingerprints.
Private Weston/Pixman/Lavapipe fixtures verify actual app startup, Cyrillic,
combining and JP CJK samples with adjacent ASCII, primary styles, clipping,
read-only AT-SPI tree and UI kill/reopen with unchanged generations/PIDs.
See the [font research](../research/terminal-font-profile.md) and
[render evidence](../../crates/app/evidence/font-profile-20261017).
The scope is software rendering on Linux; it does not establish fractional
scale, actual IME, GPU behavior, all glyph ink bounds, screen-reader text
operation, unsupported fallback identity or macOS/Windows parity.
