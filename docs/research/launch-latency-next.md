# Synthetic launch and READY visibility probe

Status: bounded diagnosis, 2026-10-17. This probe used only a synthetic Python
PTY producer and the frozen stage-diagnostic host. It did not contact or signal
the active soak, launch a GUI/compositor/provider, or send user input.

## Result

The roughly 3.8-second delay in the actor-stage soak's initial READY snapshots
is not launch latency. The producer writes `READY:<stream>` immediately after
exec, then enters alternate-screen mode and clears it before the harness's
frame-text readiness oracle can observe it. The producer returns to the primary
screen in pattern phase 4, about 3.76 seconds later at 4096 bytes/s. At that
point the saved primary-screen READY text becomes visible again.

I reproduced this with the exact soak `PRODUCER` string and frozen binary
SHA-256 `610efac4b817116ed761217a9738485c2b499bf6fb05a30ebce7cb6dbf7e4791`.
The synthetic child recorded its READY PTY write 5.18 ms after the launch RPC
returned. The first snapshot whose active frame text contained `READY:0` arrived
3,795.26 ms after launch reply, at sequence 82 with 15,787 PTY bytes read. In
the immediately preceding sample, sequence 77 had read 15,505 bytes and READY
was not visible. The sampled actor stage at readiness was `waiting`, consistent
with completed processing rather than an actor stuck inside a pipeline stage.

A separate quiet synthetic child measured launch RPC at 16.68 ms on the user's
durable Btrfs state path and 8.94 ms on tmpfs; host startup was about 20 ms in
both runs. The exact soak producer on tmpfs measured a 13.54 ms launch RPC.
These are single-run diagnostic observations, not performance claims; the
Btrfs measurement may include interference from unrelated concurrent storage
work.

## Source path

The host's launch path in `crates/session-host/src/lib.rs` creates the journal
and history before opening the PTY, then spawns the helper, completes the
private-socket nonce exchange, registers and permits the helper, marks the
operation running, starts the PTY reader and actor, and returns the initial
view (`Host::launch`, approximately lines 787–958). The helper sends its nonce,
waits for the matching exec permission, then `exec`s the synthetic child
(`helper`, approximately lines 1533–1541). The launch RPC response is therefore
separate from later terminal frame publication.

The soak producer emits READY, starts its pacing clock, and sends pattern
phase zero. Phase zero switches to DECSET 1049 and clears the alternate screen;
phase four sends DECRST 1049. The harness's `STRESS.text(view)` searches only
the currently published active frame cells. Thus the original READY is saved
on the primary screen while the alternate screen is active and reappears when
the producer restores that screen. At rate 4096, each phase advances after 15
257-byte chunks, about 0.94 seconds; phase four starts at about 3.76 seconds.

The observed `15,787` byte count is a consequence of the producer's elapsed
time while the marker was hidden. It is not evidence that launch itself took
3.8 seconds. The new actor-stage and reader diagnostics distinguish the phases:
the child READY write is early, the PTY reader has received/enqueued output,
and the eventual visible snapshot follows alternate-screen restoration.

## Next harness adjustment

Gate synthetic streaming on the host observing a one-time READY frame. Have the
child write READY and wait for a synthetic GO command before enabling alternate
screen patterns or rate-limited output. This mirrors the readiness barrier in
`scripts/stress-pty.py`. Keep the child-side monotonic READY timestamp so a
future run can report both child-ready-to-frame-publication delay and the
post-GO workload independently. No host timing or soak deadline needs to
change.

This diagnosis addresses only the initial READY timing. It does not localize
any later actor stall, explain ACK timeouts, certify durable throughput, or
change the active long run's status.
