# Native session-host review — stage 0

Read-only review of `crates/session-host`, `crates/storage`, and `crates/terminal` against SPEC §§7/8/10. Reviewed on 2026-10-17 (local date), during active root integration. Only this report was edited. Findings distinguish actual defects from acceptance work not yet implemented. The root is adding experimental state exports in vendored VTE/Alacritty; this review **does not assume those libraries are unmodified**, and does not judge the new checkpoint format without its tests.

## Confirmed defects

### P1 — Blocking PTY input prevents resize and Stop for that session

**Source:** `crates/session-host/src/lib.rs:550`–553, 588–590; the single actor also handles `Resize` and `Stop` at 555–570.

The actor calls synchronous `writer.write_all()` for user input and terminal query responses. An alive child which does not read its slave PTY fills the input buffer; the actor then cannot consume output, resize or Stop actions. The channel capacity of eight bounds memory but does not preserve a control path. Other sessions have separate actors, so the defect is scoped to the stalled session.

**Dynamic reproduction:** existing `target/debug/pigade-session-host` (the pre-current-checkpoint binary) served an isolated temporary directory. Launched `/bin/sh -c 'stty raw -echo; printf READY; sleep 30'`, waited for `READY`, sent four valid 16,384-byte `Input` requests, then a valid `Resize` to 25×81. All five IPC requests returned `error: null`. After 300 ms, snapshot still reported 24×80, `state: running`, and no storage error. Cleanup killed only that test host and its own CLI process group.

The existing debug binary has the same blocking-write path as the reviewed source. This is a correctness/control defect, not a performance-budget extrapolation. A reproducible regression should hold the slave input unread and require a bounded Stop result despite queued input. Recommended repair: a bounded writer path which cannot block the terminal/control actor, or nonblocking/pollable writes with explicit pending input bounds and cancellation. Do not make the queue unbounded to hide the stall.

### P1 — Journal replay bypasses Geometry validation (fix in progress)

**Source at discovery:** `crates/session-host/src/lib.rs:216` directly calls `terminal.resize(serde_json::from_slice(...))`; `Geometry` had derived `Deserialize`, public fields, and bounds only in `Geometry::new`. `JournalReader` validates frame sizes, checksums and ordering but does not itself validate resize payload semantics.

A CRC-valid journal with initial geometry 24×80 followed by `Resize { rows: 0, cols: 0 }` bypassed the constructor. Dynamic reproduction against the available debug binary returned exit code **101** with:

```text
alacritty_terminal-0.26.0/src/grid/resize.rs:276:50:
attempt to subtract with overflow
```

The test fixture was created only in a temporary directory, with valid header/event checksums, then removed. The binary embeds the older profile `vte=0.15.0+pigade-utf8.1`; the current source retains the unchecked deserialize call, so the source-level finding applies unless its type deserialization is repaired. Oversized geometry was **not** executed because it could exhaust memory.

**Root response:** root is implementing custom `Geometry::Deserialize` through `Geometry::new`, with a malformed resize regression. Treat this item as pending until fresh integration tests prove rejection without panic and preservation of the original journal. A valid-prefix read-only result is preferable where recovery can retain earlier events; semantic corruption must be explicit.

### P2 — One slow or incomplete IPC client blocks all host command handling

**Source:** `crates/session-host/src/lib.rs:627`–654; one sequential accept/receive/handle/send loop, each connection having a two-second read/write timeout. Launch processing can also wait up to five seconds for helper accept and another five for its hello at 413–434.

A same-user client which connects and withholds its frame body occupies the only control loop until timeout. During that time, other sessions' actors can parse existing output, but no other UI input, resize, snapshots or Stop request is handled. This is directly established by the synchronous call structure; no latency claim beyond those configured waits is inferred. A bounded connection admission/worker design or asynchronous framed receive would retain independent session controls. This is not an authorization bypass: peer ownership is checked.

## Additional failure-path gaps (not demonstrated data loss)

- **Unrepresented launch intent:** errors after `begin_launch` but before `sessions.push` leave a safely locked intent, yet `List` only returns terminal views (`lib.rs:300`–305). A journal creation/openpty/helper/metadata failure can therefore leave a locked operation without a UI-visible registry row; after restart, a missing/unreplayable journal still has no view. This is fail-closed for duplicate launch, but the caller has no structured recovery action/status manifest. Do not report full recovery of every intent until the manifest represents these states. Host handles retry by returning an explicit error when the old operation has no attachable frame (365–376), which prevents a duplicate spawn.
- **PTY read errors are treated as EOF:** reader combines `Ok(0) | Err(_)` (`lib.rs:459`), then the actor waits for process exit (`585`). Real Linux PTY EIO commonly means closure, but other read failures should not silently turn into a possibly unbounded wait on a live child. No injected-error reproduction was run.
- **Storage degradation is permanent parking:** failure paths preserve the current event plus bounded queue/master and publish `degraded`, then park indefinitely (`531`–539, 604–613). This protects the durable prefix and applies PTY backpressure, but does not implement the SPEC's recover/switch-storage workflow, and Stop is not serviced in this state. This is an explicitly incomplete stage-0 recovery path; no raw-output discard was observed.

## Evidence of correct boundaries in the reviewed source

- `Journal::append_impl` writes checksummed, sequence/generation-tagged events, calls `sync_all`, then updates its durable sequence and returns a receipt (`crates/storage/src/journal.rs:347`–398). The writer is poisoned before attempting I/O and stays poisoned on failure. Header publication also fsyncs its parent directory (287–320).
- The actor applies output/model changes only **after** `journal.append` succeeds, then publishes the receipt sequence/frame (`session-host/lib.rs:576`–594). The timeout branch similarly journals before applying the timeout. Thus the ordinary output path respects durable-before-display in source; this is not a power-loss certification.
- The actual PTY resize is performed before the resize record's durable append (`573`–576). If the latter fails the actor degrades without applying/publishing the new model. This does not falsely commit a new displayed geometry; it does leave an intentionally stalled PTY/model mismatch until recovery. Exhaustive resize failure-boundary testing remains required.
- PTY output reads use 16 KiB chunks, a blocking `sync_channel(8)` and one retained pending action, so output overload backpressures the reader instead of dropping bytes (`451`–470, 547, 608–613). Client commands use `try_send` so a full queue is explicit (337–345).
- IPC rejects zero or >8 MiB frame lengths before allocation, checks protocol version, and uses Linux `SO_PEERCRED` to require the current UID (`87`–145, 629–651). A private owned state directory is enforced, and the public socket is mode 0600. Input is capped at 16 KiB, argv and live launch geometry have bounds.
- Launch intent/generation lock is committed before spawning the helper. Helper PID is checked via peer credentials, boot/PID/start-time identity is durably registered, exec permission is durably recorded, then nonce permission is sent (`378`–438). A lost gate before permission cannot exec (`659`–669). On host restart unfinished launches are marked unknown and never respawned (`264`–277). These are good safety boundaries; exhaustive REC-07 fault injection is still needed.
- Replay creates `Terminal::new(..., false)`; replay events do not deliver input or create processes. Terminal responses are gated on `live`, OSC52 is disabled, and clipboard/bell/external actions are ignored (`terminal/lib.rs:125`–138, 163–194). There is an existing response-suppression unit test. No external side-effect execution during replay was found.

## Acceptance work not yet established by this review

These are missing proofs/features, **not evidence of a newly discovered bug**:

- Current checkpoint exports, checkpoint + tail equivalence, atomic snapshot publication, cold-history indexing and lossless compaction require the ongoing implementation's tests. A render `Frame` alone is explicitly not a checkpoint. No claim is made about an incomplete vendor patch while another lane owns it.
- The current host stops parsing/replaying after 64 MiB, explicitly degrades/skips the history instead of claiming a bounded production restore path. This cannot pass SPEC's 4×1 GiB long-history gate.
- No live-PTY recovery across host death is assumed. Historical views and `launch_outcome_unknown` are distinct; exact identity reconciliation and explicit recovery UX remain acceptance work.
- Registry/metadata journal-offset references, recovery of unpublished snapshots, user-service/cgroup behavior, 24 h soak, VM reboot, actual provider resume IDs, GPU/Wayland/X11/IME/accessibility/performance matrix were not validated by this bounded review.

## Verification performed

Read all assigned sources/tests/manifests relevant to ordering, launch and replay, inspected the modified vendor parser/terminal integration boundary, and ran the two isolated dynamic probes above against the available debug binary. No source build or workspace-wide tests were started because root integration was changing the same crates; root owns fresh post-fix validation. The probes do not establish the current experimental checkpoint behavior. The next integration gate is the geometry regression plus a blocked-input/Stop regression, followed by the existing native test suite.
