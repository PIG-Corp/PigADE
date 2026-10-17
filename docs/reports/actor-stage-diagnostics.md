# Independent actor and PTY-reader diagnostics — 0.0.1-dev

The last frozen-host soak established a completed child ACK write followed by a
five-second live publication delay, with the ACK in the next final journal
record. Published cumulative metrics could not locate the delay because those
metrics update only when a frame is published.

Live Snapshot/List responses now add an optional version1 `actor_diagnostic`.
Each session has a separate fixed-size probe. Actor updates identify waiting,
query-response admission, PTY resize, journal append, history begin, model apply,
history commit, checkpoint, frame construction/publication, degraded and exited
stages. The sample records stage entry in CLOCK_MONOTONIC ns and an explicitly
named candidate sequence; neither stage entry nor candidate proves durability.
Reader counters record completed reads and queue admissions, cumulative bytes
and their last monotonic timestamps, without content or hashes.

The probe lock is held only to update/copy fixed-size fields, never across PTY,
filesystem, model or archive work. Snapshot first releases the View lock, then
uses try_lock on the probe. Contention or poison yields `sample_available=false`
with absent stage/time; old or recovered sessions have no probe. Clock failure
leaves timestamps absent. Frame and probe are separate observations, not one
atomic snapshot. Reader gaps can indicate an in-flight queue admission, but do
not alone prove a deadlock. The probe is diagnostic, not terminal authority.

The soak retains a strict whitelist of version1 numeric fields/stage names on
ACK timeout and throttled resize rejection; unexpected strings/versions/types
are marked invalid/unsupported rather than copied. It records stage age at the
snapshot completion and the snapshot's age separately. Missing, unavailable,
invalid and unsupported samples are distinct. No input retry, timeout change,
raw output loss, journal/checkpoint/profile or terminal semantic change follows.
Protocol2 remains compatible: the field is additive/optional; the app's existing
permissive View DTO ignores it. No probe is persisted as terminal state.

Tests use a barrier-blocked simulated journal stage to prove Snapshot can expose
new diagnostics while the published sequence remains unchanged. They cover old
View decoding, contended/poisoned probe refusal, saturating reader counters and
harness whitelist/exclusion. Fresh185 workspace tests, strict Clippy, formatting
and release workspace build pass. A real short smoke and longer reproduction
are separate evidence; this code change does not establish the stalled stage or
pass the24h gate.

A30-second four-PTY smoke on frozen release SHA610efac4b817116ed761217a9738485c2b499bf6fb05a30ebce7cb6dbf7e4791 completed in30.019603s with all-session CRC/full-recovery checks. A manual read-only observation was attempted after this short run had exited and got connection-refused; it is not live-probe evidence. The harness now retains initial READY snapshot probe summaries directly, and a longer diagnostic will test that path. No24h claim follows from the smoke.

The new7200s diagnostic is running on that same610efac4 release. All four initial READY snapshots provide version1 available probe data and reader/enqueue counters. [Curated smoke and initial probes](../../benchmarks/results/actor-stage-20261017/) retain the exact source/binary and scope. Initial availability is not evidence of a completed long run or localization of a stall.
