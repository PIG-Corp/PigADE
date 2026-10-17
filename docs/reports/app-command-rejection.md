# UI command rejection recovery — 0.0.1-dev

A proven host queue-full rejection previously became a permanent shared UI
error. The management Escape action only cleared local errors, so it could not
reopen input. Resize geometry was also cached at local queue admission and
never invalidated after rejection. Additionally, the IPC worker continued
sending later queued commands after the first command's unknown outcome.

The worker now closes a tested dispatch gate before handling another queued
request. It drops and counts later unsubmitted requests without retaining or
replaying their input. A known rejection exposes a notice; explicit
Ctrl+Shift+Space then Escape acknowledges the exact observed notice token and
permits new work. Unknown outcomes remain locked after acknowledgement. A stale
acknowledgement cannot hide newer discarded-command/resize details.

Known rejected and unsent resize keys invalidate their local request cache;
geometry may enqueue a fresh resize only after acknowledgement. Previously
admitted resizes are retained, and rendering uses acknowledged host geometry.
Cache entries for vanished generations are removed. No failed input is retried,
no process is restarted and no host output/journal contract changes.

Six pure tests execute the same dispatch closure used around real IPC and cover
known rejection/explicit acknowledgement/new input, unknown locking, unsent
counts, stale notice tokens and resize-cache recovery. Native application98
unit tests and strict Clippy pass; release build/formatting pass. This does not
prove a native mouse/host-saturation UI path. The current failure gate is global
to this prototype's IPC worker; independent per-session UI gating, a mouse
acknowledgement control and rejection-aware drag reconciliation remain required
before full MVP acceptance. Ordinary management Escape already clears local
pre-admission errors; the new path extends it to a proven host rejection.
