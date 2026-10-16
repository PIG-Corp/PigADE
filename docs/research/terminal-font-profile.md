# Terminal font profile research — 2026-10-17

## Result

This research prepared derived, uniquely aliased JetBrains Mono static faces
as the app-level primary font and Noto Sans Mono CJK JP as its fixed
Japanese-regional fallback. The app registers the derived bundled faces and
applies the separate render profile in `crates/app/src/font.rs`; this does not
bind fonts to the terminal core profile, change the active 1 GiB corpus, or
establish visual/platform acceptance. The JP forms are a deliberate regional
default for covered glyphs, not a claim of locale-correct typography for all
CJK languages. `CORE_PROFILE` remains
`font=unfixed-prototype`; retain that identity and the frozen corpus until a
later explicit core-profile migration gate.

JetBrains Mono v2.304 is the smallest complete primary candidate found for the
current renderer: its checked regular, bold, italic, and bold-italic static TTF
files total 1,108,400 bytes. All four faces were inspected with fontTools 4.63.0.
They contain the tested Cyrillic (`Привет, мир! Ёё Іі Її Ґґ €`), box-drawing
(`┌─┬─┐│ ╭─╮ ║ ═`) and combining-mark (`e\u0301и\u0306a\u0328`) samples. Their
`M` and tested single-cell box-drawing advances are both 600/1000 em. This is
coverage evidence from font tables, not a renderer result.

The unmodified upstream primary files, OFL 1.1 text, upstream AUTHORS, redistribution notice,
source-archive SHA-256 and per-file hashes are under
[`crates/app/assets/fonts/jetbrains-mono-v2.304`](../../crates/app/assets/fonts/jetbrains-mono-v2.304/asset-manifest.json).
The official JetBrains GitHub release marks v2.304 as Latest; its tag resolves
to commit `cd5227bd1f61dff3bbd6c814ceaf7ffd95e947d9`. The official project
README states that the font is OFL-1.1 and may be bundled with applications.
The upstream files remain unmodified. Derived copies in
[`derived-terminal-v1`](../../crates/app/assets/fonts/derived-terminal-v1/derived-font-manifest.json)
change OpenType identity strings to the PigADE-only family aliases
`PigADE Terminal Mono v1` and `PigADE Terminal CJK JP v1`. Neither upstream
reserved family name is used as a derived family. The Noto JP OTF also had an
empty, unsigned DSIG table removed; a signature would no longer authenticate
a font after its identity names changed. `scripts/prepare-terminal-fonts.py`
rebuilds the assets offline with developer-installed fontTools 4.63.0, keeps
the applicable OFL and author attribution alongside the derived assets, emits
`PigADE-Terminal-NOTICE.txt` routing readers to those actual filenames, and
records hashes for the unchanged upstream notices. It checks glyph outline,
metric, and cmap fingerprints before and after transformation. Its
`--verify` mode regenerates into a temporary directory and byte-compares every
output, including the manifest. This is source and transformation evidence;
it is not proof of which bytes a platform backend will paint.
The official release archive was downloaded from the URL in the manifest and
measured at 5,622,857 bytes, SHA-256
`6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf`.

## Regional CJK fallback and placement policy

The primary font has no glyphs for the tested CJK sample. An official,
bounded regional candidate is Noto Sans Mono CJK JP v2.004. Its upstream
regular+bold package is 27,839,173 bytes; the two OTF files expand to
33,417,305 bytes. The archive SHA-256 observed locally is
`6c8faf475ce78fa37486dd5d8920e4bb4450b1b0f3c497edf3ba2d25cf52ab78`. The
unmodified Regular OTF is the source for the app profile's explicit fallback;
the derived OTF has its alias metadata and empty DSIG removed. The upstream
OFL 1.1 text and provenance are in
[`crates/app/assets/fonts/noto-sans-mono-cjk-jp-v2.004`](../../crates/app/assets/fonts/noto-sans-mono-cjk-jp-v2.004/asset-manifest.json).
The full JP package is much smaller than the official all-region mono OTC ZIP
(104,680,673 bytes), but JP forms are a regional typography choice. The project
has not chosen a universal CJK locale or a per-run language-tagging policy.
This app-level profile uses Japanese-region forms for glyphs covered by the JP
family; it does not assert that those forms are correct for every Han-language
CLI.

The font-table advances are informative, but they do not by themselves fail a
two-cell terminal policy. The host owns cell spans: a wide glyph is assigned a
two-cell span, and the next host cell is positioned from its own column index.
The app painter positions clusters at host cell origins, left-aligns them, and
clips each cluster to its host-assigned one-/two-cell span as well as the pane
viewport. The span mask bounds overhang at adjacent host columns, but it does
not prove glyph ink or baseline fits visibly inside the span. A glyph narrower
than two cells can leave a gutter; natural advance need not equal the allocated
span. The profile uses GPUI's vertically centered shaped-line baseline and a
line height with four logical pixels of padding. Pixel fixtures must still
verify the ink and baseline under that policy.

Measured table advances for this placement investigation:

| Font / glyph | Advance (1000 UPM) | Relative to JetBrains Mono `M` (600 UPM) | Required for two cells |
| --- | ---: | ---: | ---: |
| JetBrains Mono `M` | 600 | 1.00 cell | 1.00 cell |
| Noto Sans Mono CJK JP `漢`, `あ`, `中` | 1000 | 1.67 cells | host assigns 2 cells |
| Noto Sans Mono CJK JP `한` | 920 | 1.53 cells | host assigns 2 cells |

The source Noto mono font's Latin `M` is 500 units and selected CJK JP glyphs have
1000 units; source JetBrains Mono's `M` is 600 units. This shows a natural advance of
about 1.67 cells for those ideographs at equal nominal font size, not a
collision or a pass/fail verdict. The Hangul sample has a still narrower 920
unit advance. The following cell starts at the host's next column, two cells after a wide
glyph, and the glyph paint is clipped to that two-cell span. The pinned JP
fallback supplies regional forms for covered glyphs; this is not a claim of
locale-correct Han forms for every language. Bold/italic fallback selection,
unsupported CJK codepoints, emoji, variation selectors and ZWJ clusters still
need renderer fixtures and may use platform fallback.

## GPUI integration points at the pinned source

The application pin is `zed-industries/zed@322cd509dcccaee1f6bc44725d270598ae8171ad`.
At that revision:

- `App::text_system().add_fonts(Vec<Cow<'static, [u8]>>)` registers in-memory
  font bytes and invalidates cached resolution/layouts. The platform trait has
  implementations in `gpui_linux`/`gpui_wgpu` (Linux), `gpui_apple` (Apple), and
  `gpui_windows` (Windows). These are available APIs, not platform acceptance.
- `Font.fallbacks` accepts `FontFallbacks::from_fonts(Vec<String>)`; Linux’s
  pinned `CosmicTextSystem` turns that ordered list into explicit family
  fallbacks. A typo/missing family is dropped by this path, so startup must
  verify the bundled family resolves rather than rely on an OS font.
- `FontFeatures::disable_ligatures()` in this pin sets `calt=0` (contextual
  alternates). It does not itself set `liga=0`; the app profile constructs
  explicit `liga=0` and `calt=0` feature values.
- `crates/app/src/font.rs` registers four embedded static faces under the
  unique primary alias and the derived Noto JP Regular face under its unique
  fallback alias. Before registration it fails if either alias is already
  present in the app font database; after registration it verifies both names
  and resolves the four primary styles plus the fallback family. This makes
  the alias-to-bundled-face mapping unambiguous in the process font database
  when the precheck succeeds. It does not establish the identity of
  GPUI/platform fallback for codepoints missing from the JP face. The profile
  pins 14 logical px. The fallback has only a Regular face; bold/italic fallback
  styling remains unverified.
- The pinned GPUI source constructs cosmic-text font databases by loading system
  fonts before later `add_fonts` calls append embedded fonts. Linux `load_family`
  gathers all matching family faces without source preference. For upstream family
  names, mere registration, name presence, or resolution cannot prove which bytes
  win when a system font has the same family. PigADE-only aliases plus the
  pre-registration collision check address that ambiguity for the two named
  families. Unsupported glyphs can still use platform fallback and remain
  unpinned.
- In `crates/app/src/main.rs`, `paint_panel` positions each glyph cluster at
  `pane.origin + (cell_width × host_column, cell_height × host_row)` and shapes
  the host-owned cluster as one GPUI text run. Its content mask clips at the
  pane viewport; `TextAlign::Left` is used. The painter now also clips each
  glyph to the host-assigned one-cell span, or two-cell span when the leading
  cell carries Alacritty's `WIDE_CHAR` flag. This bounds overhang at adjacent
  cell origins in painter geometry, but does not prove glyph ink or baseline
  fits visibly within that span. `render-profile.json` records the app-level
  policy separately from semantic core identity.

The primary has static italic faces matching the current renderer flags
(`bold` and `italic` can both be set). The pinned Noto JP asset contains only
Regular, copied out of the upstream Regular+Bold package. Bold/italic fallback
resolution is not certified; fixture it before claiming styled CJK parity.

## Required fixtures before render-profile acceptance or core-profile binding

1. Run the app in the isolated Linux renderer and assert that registration
   succeeds from embedded bytes, the `PigADE Terminal Mono v1` alias is used for
   regular, bold, italic, and bold-italic requests, and the explicit
   `PigADE Terminal CJK JP v1` fallback alias resolves. Include `漢あ中한` directly between
   ASCII sentinels (for example `A漢B` and `xあ中한y`) to check both glyph coverage and
   adjacent-cell preservation. Inspect uncovered-glyph behavior
   as platform behavior only.
2. Compare `M`, single-cell box drawing, combining clusters, and representative
   CJK glyph advances/ascent/descent at 14 logical px and 10/13/24 px settings.
   Host geometry assigns the span (one or two cells); require the glyph ink and
   baseline to stay within that span without collision with adjacent columns.
   Exercise the painter's one-cell and `WIDE_CHAR` two-cell masks on ordinary,
   combining, and wide clusters.
   Natural advances need not equal the allocated span. Check 100%, 125%, 150%,
   and 200% device scaling, including device-pixel rounding.
3. Render adjacency fixtures with text immediately before and after wide glyphs,
   ANSI bold/italic/bold-italic, Cyrillic, box drawing, combining sequences,
   locale-sensitive Han, Hangul, emoji, variation selectors, and ZWJ sequences.
   Placement is currently left-aligned with per-span clipping. The baseline is
   GPUI's vertical centering of shaped ascent/descent, with four logical pixels
   of line padding. Measure glyph ink under that policy and check that glyphs do
   not collide with or shift the next host column. Compare on the same
   OS/font/renderer profile; cross-platform pixel identity is not a requirement.
4. Run those fixtures on isolated Linux MVP and then separate macOS/Windows
   environments. Until each environment is actually exercised, mark its result
   unknown. Do not use the active desktop for UI automation.
5. Only after the advance/placement fixtures pass should the lead create a
   versioned core font profile and update checkpoint/recovery identity policy.
   Existing corpus state belongs to the prior `font=unfixed-prototype` profile.

## Primary upstream references

- [JetBrains Mono official v2.304 release](https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304)
- [JetBrains Mono official source README and license](https://github.com/JetBrains/JetBrainsMono)
- [Noto Sans CJK official package and download table](https://github.com/notofonts/noto-cjk/blob/main/Sans/README.md)
- [Noto Sans CJK v2.004 release](https://github.com/notofonts/noto-cjk/releases/tag/Sans2.004)
