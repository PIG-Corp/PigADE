# Full history corpus and host reopen — 0.0.1-dev

The completed model corpus contains four sessions with 1 GiB of requested
output and 100,000 geometry-changing resizes per session. The actual durable
Journal, authoritative Terminal, retirement archive/index and atomic checkpoint
paths generated it. All four compare equal against the generating live model,
full replay and checkpoint+tail, including every cold record. This is model
evidence; the generator does not create PTYs, a UI or provider processes.

The frozen builder SHA is
`354b5987feef92acb77186576a99745d63e6fced0a04a7d95c99123f49f47625`.
Its run lasted 8,020.847 seconds. The versioned SHA-bound sidecars span a logical
24-hour timeline, **not** 24 hours of elapsed runtime. Final checkpoint/tail
boundaries cover partial UTF-8; alternate screen with partial CSI; partial OSC8
terminator; and pending synchronized output with an ordered timeout.

| Session | Raw journal bytes | Cold records | Full replay, ms | Checkpoint+tail, ms | Tail journal bytes read |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 1,082,542,726 | 13,590,103 | 109,838.76 | 68.43 | 405 |
| 2 | 1,082,542,782 | 13,590,101 | 111,061.10 | 66.94 | 418 |
| 3 | 1,082,542,809 | 13,590,103 | 106,429.86 | 87.85 | 422 |
| 4 | 1,082,542,835 | 13,590,103 | 119,472.72 | 162.52 | 441 |

These are individual component observations, not a four-frame startup
percentile. The cold oracle compares all 54,360,410 records; its complete scan
cost is recorded separately and is outside fast recovery. The reported derived
history files occupy about 5.8 GB per session under this repeated-resize corpus.
Storage cost/quotas therefore remain material work. Host Btrfs free-space
preflight is not a configured filesystem quota.

[Raw builder manifest and runner](../../benchmarks/results/long-history-full-20261017/)
retain all checkpoint samples, source filenames, timeline hashes and comparison
results. Original data and the frozen executable remain under the private
`~/.local/state/pigade/long-history/long-history-run-32c6f5c4-645d-4845-a029-8f61da9a7cef`.

The current release host was then frozen as SHA
`42bc8f71cd2cd8bbf0d98823bf4e48c1049213b90250ecfbf7a0e1e159ed785d`
and killed/reopened 30 times on that immutable corpus. All four historical
frames, exact generations/sequences, history counts and all six canonical
tail records match in every cycle. Each source inventory hash remains unchanged.
Each reopen reads only 405–441 bytes of the corresponding raw journal tail.

| Host-only four-frame availability | Measured |
| --- | ---: |
| p50 | 691.53 ms |
| p95 | 1,367.28 ms |
| p99 | 1,731.69 ms |
| Maximum sampled post-start host RSS | 56,528,896 B |

[All 30 samples and source hashes](../../benchmarks/results/recovery-full-corpus-30-cycles.json)
are retained. IPC peer PID/UID and frozen executable inode are verified; this
measures the host response, not a presented UI frame. No CLI is relaunched.
OS page cache is uncontrolled initially and warm thereafter. The actual24h soak
and app release compilation overlap this diagnostic; it is not isolated PERF-01
acceptance. RSS excludes UI/GPU and the harness. Native Linux build is proven;
no other platform is certified.

```sh
python3 scripts/long-history.py --directory /absolute/private/durable/corpus \
  --binary target/release/examples/history_corpus --sessions 4 \
  --output-bytes 1073741824 --resize-events 100000 --logical-hours 24 --verify
python3 scripts/recover-corpus.py --corpus /absolute/completed/inner-directory \
  --output /absolute/new/recovery-reference --iterations 30
```

The full corpus was then copied to an isolated guest ext4 disk and recovered
across two distinct guest boots using the same frozen host and reference. On
both boots, all four generations were historical/read-only and matched the
same session IDs, sequences, frame hashes, history digests/counts, and
checkpoint-tail canonical records. The second-boot four-frame host response
was 1,665.91 ms; this is not a presented UI frame or a REC-08 p95 claim. The
host-side fixture inventory matched after the guest run. The VM did not run a
full cold oracle in-guest.

The first harness attempt failed while reading the guest's result marker before
the serial JSON line had finished. It was retained as
[`failed-attempt.json`](../../benchmarks/results/vm-reboot-full-20261017/failed-attempt.json).
The collector now waits for the result line's terminating LF before decoding;
the regression is covered by `scripts.test_vm_reboot.SerialResultTests`. The
second attempt passed and its compact, hash-linked result is
[`result.json`](../../benchmarks/results/vm-reboot-full-20261017/result.json).
The private VM run directory retains the complete manifest, guest result,
serial log, and QEMU artifacts; the 27.7 GB corpus itself remains outside Git.

This test covers an actual guest reboot with the prebuilt corpus copied to
guest ext4. It does not establish a 24-hour elapsed soak, UI-present recovery,
total product resource budgets, provider/layout resume, a full in-VM cold
oracle, or REC-08 acceptance. Those gates remain open.
