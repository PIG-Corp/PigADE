# Query replies and PTY output backpressure — 0.0.1-dev

A real-PTY regression reproduced a host wait cycle: a child writes a burst of
terminal queries and more output before reading stdin; the host's input writer
fills with replies; the actor retries replies without draining output; the
child cannot finish writing and reach its input read. The original actor stalls
at sequence2 in this fixture. This is a confirmed defect, independently of the
intermittent soak ACK failures; the soak producer does not intentionally issue
terminal queries and no common cause is established.

The actor now keeps a FIFO of at most16MiB pending reply payload and tries at
most eight16KiB chunks before servicing the next ordered action. A full writer
queue leaves the original reply intact and permits PTY output to drain; a2ms
receive timeout retries replies when the child produces no more output. Small
replies coalesce into the tail chunk to avoid one queue node per query. Reader,
input and reply queues remain bounded; no raw output is dropped. The payload
cap is not an overall host heap/RSS budget.

The regression writes50,000 CSI6n queries and2MiB output, then receives exactly
300,000 reply bytes in FIFO order and emits a unique ACK. A separate fixture
emits up to3,000,000 queries without reading stdin. At the accumulated reply
cap, the actor reports an explicit degraded state and retains its previous
published frame/sequence. The failing output event is already raw-durable;
recovery applies that additional valid event as read-only history without
executing terminal responses. This is refusal of live side-effect delivery,
not refusal or semantic truncation of the already accepted model event. It
does not implement the remaining whole-event/global allocation admission plan.

Unit tests cover tiny-reply coalescing, exact FIFO and refusal leaving the prior
queue unchanged. Actual PTY tests cover the wait cycle and durable raw tail.
The fixtures use temporary storage for correctness, not NVMe performance proof.
No timeout increase, provider task, desktop input or active soak modification
is part of this change.

```sh
cargo test -p pigade-session-host --test query_backpressure
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo build --release --workspace --bins --examples
```
