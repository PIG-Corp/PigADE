# Unchanged terminal redraw reduction — bounded plan

Status: implemented and checked at bounded scope, 0.0.1-dev. Notification trace on the actual app
matches50 frames but finds Poll→UI p95≈41.5ms. This does not prove a GPU/fence
cause. Ordinary unmarked text commit and every accepted worker receipt currently
notify the whole canvas even when no displayed state changed.

Preserve input bytes, host admission/durability, preedit lifecycle, receipt
processing and ACK cleanup. First expose/reuse the actual acknowledgement-button
presentation rule and test its pending→settled, pressed-release and unrelated
receipt transitions with the real receipt ledger. Then notify on receipt drains
only when that visible presentation changes; state/recovery/notice updates keep
their own invalidations. Ordinary text commits redraw for visible preedit
removal or protocol errors, while local admission errors retain their existing
notification. Cursor blink reset remains independently responsible for showing
a hidden cursor. No speculative terminal echo or event loss is introduced.

Run app tests/Clippy/release, the identical private50-key fixture with finite
trace, and a native Pinyin commit regression for cleared preedit. Retain failures,
old/new binary/source hashes and all samples; do not infer formal PERF acceptance
or solve the initial1s outlier merely from a faster complete run.

Result:127 app tests/strictClippy/fmt/release pass; both private native Pinyin
paths preserve exact commit/clear. One traced50-key run improves observed p95
to66.165ms and Poll→UI p95to0.466ms; formal budgets and the earlier1s outlier
remain open. See reports/host-notification-20261017.md for all evidence.
