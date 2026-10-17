# Retain only observable terminal listener work — 0.0.1-dev

The wrapper currently queues every Term Event until the ordered event ends,
then ignores BEL/cursor/mouse/clipboard events and all replay responses. Every
title update is also retained although only the last one becomes visible.
This can allocate a large transient queue without changing the final frame.

First lock exact live response FIFO and title/reset/push/pop behavior using
mixed effects, split event boundaries, sync timeout and checkpoint continuation.
Then discard already-ignored effects at the listener boundary, discard replay
response events there, and retain a single last-title update. Keep every live
response in order; do not drop bytes, change ANSI/model processing, issue
external effects or impose a queue-full policy as part of this repair. Preserve
the existing post-event dynamic-color resolution semantics.

After the repair, test actual parser callbacks before drain: ignored storms
retain no queue, title storms retain one last update, historical queries retain
no responses, and live mixed queries retain FIFO. Run workspace tests, strict
Clippy, format and release; the existing real PTY query-backpressure regression
must still pass. Source-bound diagnostics may compare retained callback count,
but must not advertise the change as global admission or an RSS budget. Live
response payload/closure storage remains part of that pending admission work.
