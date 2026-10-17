# ADR0012 — bounded notifications for host cell publication

Status: accepted bounded implementation; formal performance remains open, 0.0.1-dev, 2026-10-17.
This does not approve the stack, stage0, or formal latency budgets.

## Problem and decision

The actual app's independent command wake still left output waiting for a
50ms conditional Poll interval. A long wait in the existing command worker or
host dispatcher would block keyboard/resize/launch work. Use a separate,
read-only notification connection and app thread, followed by the existing
validated conditional Poll. Notifications carry no cells or raw ANSI.

One host-local ChangeHub owns a random incarnation UUID and a checked u64
revision. Poll samples that token **before** locking/reading views. Actors
publish a complete durable model/frame, release its lock, then advance the
revision. A publication racing the snapshot therefore either appears in it or
makes its token stale; compare-and-wait uses the same mutex as notification.
Host restart changes the incarnation, so old tokens cannot sleep on a reused
revision. Notifications coalesce without a per-event queue.

WaitForChange bypasses the serialized host command queue only after the
existing peer, framing and protocol checks. At most four waits can block, each
for1–1000ms, within the existing16-connection admission cap. Other commands
retain their ordinary bounded admission and unknown-outcome contract. Waits
have no launch or input side effects. Failed notification service emits one
bounded diagnostic per hub and omits the Poll token; it does not turn an
already committed launch into a failure. Overflow never wraps.

The app command worker blocks on its bounded FIFO channel independently of
its Poll thread. No app state lock spans wait or socket I/O. Under active
output, snapshot requests coalesce to at most nominal60Hz; all raw events stay
in the host journal/history. Older hosts without tokens and failed waits use
bounded timer polling. An explicit pre-admission queue_full on read-only Poll
shows replica lag and permits another Poll; it never retries a mutation or
clears an unknown command gate. Other Poll failures retain the current visible
error/input-lock policy.

## Verification and limits

Host unit regressions cover registration races, bursts, waiter capacity,
timeout/foreign epochs, overflow, poison, one-shot diagnostic reporting and
Poll token-before-view order. An IPC test proves wait bypasses dispatch. A real
host/PTY integration checks output-driven cells/tokens, independent commands,
historical restart without respawn and a new host incarnation. The actual app
has a private UDS test holding wait while independent input is acknowledged,
and tests legacy token absence and read-only busy recovery.

The first private software X11 runtime measurement on frozen app3eba79d2… and
host477d5f81… failed a1s causal-color sample deadline. Raw replay contains that
sample's color, but the original harness omitted partial sample/capture detail.
Retain that failure and add bounded diagnosis before attributing a speedup or
claiming a presentation budget. Formal echo latency, hardware GPU, sustained
output and idle CPU acceptance remain separate gates. Global per-host epochs
can wake a client for an unselected session; there is no subscription filter.

Subsequent same-frozen-host runs complete50 exact keys/ACK, with p95≈97ms.
A finite numeric trace finds Poll→UI p95≈41.5ms. All runs and scope are in
[the notification report](../reports/host-notification-20261017.md); the initial
1s outlier remains unresolved and no formal latency gate is claimed.
