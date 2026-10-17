# Local selection and explicit paste — 0.0.1-dev

The native app selects plain text from host-provided cells. Left-drag selects
when reporting is disabled; Shift+left-drag selects locally when a CLI owns
mouse reporting. The highlight includes both halves of a wide glyph and keeps
combining text intact. A soft wrap joins rows; a hard row boundary inserts LF.
Spaces in nonblank selected rows are preserved. Blank selected rows become
empty lines; trailing blank rows are trimmed, retaining at least one row.
Leading wide-character wrap padding contributes no copied text.

Selection belongs to the exact displayed session, generation and event
sequence. A frame revision clears it rather than interpreting old coordinates
against new content. This conservative implementation is suitable for a quiet
visible frame; stable selection during live output, history paging/search and
scrolling selection remain unfinished. No extra terminal parser is involved.

Copy and paste are available through footer buttons and the explicit PigADE
prefix Ctrl+Shift+Space, followed by C or V. Prefix+A selects the visible frame.
Ordinary CLI Ctrl+C and other combinations retain their terminal meaning outside
management mode. Copy writes only selected cell text, excluding hyperlinks,
styles, modes and other host metadata. Replay/output never calls clipboard APIs.

A clipboard read binds the selected session/generation and local input epoch.
Typing, pointer interaction, lost focus/deactivation or generation change cancels
a pending delivery. Only one clipboard read is pending at once. A stale or failed
read reports a nonblocking notice; it does not freeze healthy terminal input.
The eventual input command is checked against the current live generation and
never retried after an unknown IPC outcome.

Paste uses the acknowledged DECSET2004 mode. Bracketed paste sends CSI200~,
normalized text, then CSI201~ in one bounded input command. CRLF/bare CR become
LF in bracketed text; ordinary paste maps newline to CR. Unicode and tabs are
preserved. Embedded control characters other than TAB/CR/LF are explicitly
refused, including ESC and C1 control characters; this prevents a pasted closing
marker from escaping the bracketed body. Empty paste sends nothing. The16KiB
input limit includes the12 wrapper bytes. Larger paste is refused visibly; a
streaming multi-command paste protocol is still pending.

The pure tests cover Unicode, reverse selection, wide/combining cells,
soft/hard breaks, frame invalidation, control refusal and packet limits. Two
real raw-PTY scenarios verify mouse wire delivery, stale-generation rejection,
and bracketed/unbracketed paste bytes after acknowledged mode changes. The
`mouse_input` test target currently runs25 tests:23 shared pure codec tests and
two real PTY tests. The app runs61 tests and passes strict all-target Clippy.

The same-app `--diagnostic-selection` flag stages only a local cell selection.
The private Weston/Pixman/Lavapipe screenshot verifies highlight placement and
footer controls; no native mouse/keyboard injection occurs. This is not clipboard
roundtrip, native gesture, Wayland/X11 or GPU acceptance. Clipboard provider
behavior and full accessibility Text/Action remain unverified.

```sh
crates/app/bootstrap.sh test --locked --bin pigade-app
cargo test -p pigade-session-host --test mouse_input
python3 scripts/isolated-render.py --output /absolute/new/private/run \
  --selection-fixture --recovery-warning
```

Wire reference: [xterm bracketed paste](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#Bracketed_Paste_Mode).
