# UI command rejection recovery — 0.0.1-dev

A proven host rejection closes the exact `(session, generation)` command gate
before the worker takes another request. Later requests for that target are
counted and discarded without resending input. Other targets remain usable.
Only explicit `exited`/`history` host states retire a gate; absence from a poll,
an unknown state or a different UUID does not establish that a process ended.

A known rejection exposes a mouse action and management Escape
(Ctrl+Shift+Space, then Escape). Acknowledgement requires the exact displayed
target and globally increasing notice token. Stale acknowledgements do nothing.
Unknown outcomes remain locked. Gates are capped at32, with fail-closed overflow
and counter exhaustion. Rejected/unsent resize keys invalidate the exact target's
cache; rendering still uses acknowledged host geometry.

The bounded32-entry request ledger waits for individual FIFO worker receipts.
An Accepted receipt means admission to the host's ordered input-writer path,
not child consumption. Mouse state is speculative until that receipt. Known
rejections restore the last admitted pressed-button state; later dropped
receipts cannot clear uncertainty. A failed press never invents a release.

If an admitted button remains pressed, the explicit acknowledgement action
prepares one fresh release using that target's current host geometry and modes,
clamping the last position. The worker validates the acknowledgement and sends
that cleanup in the same ordered message. It does not retry the rejected input.
An unencodable release stays visible and retains held state; unknown outcomes
prohibit cleanup. Pending receipts, held targets, cleanup reservations and
mouse-uncertainty tracking are bounded. Receipt send may backpressure the worker
without holding the shared state lock; the UI drains receipts independently of
state revisions.

Fresh root application tests pass110/110. Tests cover actual gate-dispatch
closures plus receipt reconciliation, exact/stale ACK cleanup, unknown followed
by dropped requests, individual receipt barriers, independent targets, bounds,
resize-cache invalidation and explicit ended-state pruning. Strict Clippy and
release validation are recorded in WORKLOG. No real saturated-host/native mouse
injection path is claimed. Local pre-admission `input_error` is still global to
this prototype and remains a separate fairness/usability gap. No host protocol,
output journal, launch or replay contract changed.
