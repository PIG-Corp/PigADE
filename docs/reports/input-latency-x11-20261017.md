# Private X11 causal input/cell latency — 0.0.1-dev

The same actual PigADE app now has a fixed50-key latency fixture. The helper
uses XTEST only after private namespace/server peer/app PID/focus checks. Each
ordered alphabet key makes a raw, no-echo PTY child clear the screen to a unique
flat RGB color. CLOCK_MONOTONIC starts immediately before XTEST press/release;
a1x1 XGetImage readback at a fixed interior cell observes that key's exact color.
The1ms poll sleep,1s sample deadline and50 sequential samples are explicit.
The child rejects wrong order, missing or extra input. The host still owns ANSI
parsing and journals output durably before displaying the cell replica.

This is software Xvfb/Lavapipe submission-to-observed-cell diagnosis. It excludes
physical keyboard delivery and GPU scanout, runs alongside builds/soak, and is
not isolated PERF-01 acceptance or the specification's echo-harness result.

## Retained baseline

App SHA `f9fea2865624249bcaf0d19cf6be1101e589a703ed840dad2da9db51a720bbf2`
corresponds to app source e5ac10b; host SHA
`610efac4b817116ed761217a9738485c2b499bf6fb05a30ebce7cb6dbf7e4791`
corresponds to8b2ecaa. Both executables are frozen separately from later edits.
Raw samples, source/helper hashes, private run paths and capture hashes are in
[baseline.json](../../benchmarks/results/input-latency-x11-20261017/baseline.json).
Quantiles use nearest rank, n=50 per run.

| Run | p50 ms | p95 ms | p99/max ms | Ordered/exact child input |
| --- | ---: | ---: | ---: | --- |
| Initial |143.028|208.031|208.463|50/50, no extras |
| Baseline with postmeasurement ACK wait |95.922|158.223|191.949|50/50, no extras |

The initial capture was taken before child ACK publication: it shows the final
causal color, but no ACK text. After the samples, the harness now waits for that
independent host-visible ACK before capturing. This does not alter sample
measurement or deadlines. Root visually inspected the second capture: the final
color and the unique child ACK are visible. Both runs are retained; variation
and concurrent workload prevent attributing the difference to that wait.

## Event wake change and limits

The worker now waits on the command channel instead of an unconditional50ms
sleep, retaining the first request in FIFO order. Batches are capped at32 so
continuous input cannot starve polling. A coalesced single-consumer wake signal
notifies the UI of changed state and individual receipts; receipt notifications
occur both before a potentially blocking send and after insertion to avoid a
drain-before-insert race. The existing50ms timer remains as idle/blink service.
There is no new ANSI parser, output dropping or asynchronous raw journal write.

| Event-wake run | p50 ms | p95 ms | p99/max ms | Ordered/exact child input |
| --- | ---: | ---: | ---: | --- |
| Initial integration |111.656|159.936|752.672|50/50, no extras |
| Final constructor bundle, source-bound |95.752|96.627|111.938|50/50, no extras |

All samples, including the slow integration run, remain in
[wake.json](../../benchmarks/results/input-latency-x11-20261017/wake.json).
The final executable SHA is
`484b1eacdb8df844c422e298470a3bd7736dc7907e8b7af79942cf0681bb9669`;
[wake-source.json](../../benchmarks/results/input-latency-x11-20261017/wake-source.json)
records exact first-party app source/lock hashes. The earlier integration binary
precedes the constructor argument bundle and lacks a retained exact main-source
hash; that gap is explicit. Root reviewed final private capture and ACK.

These samples do not establish a stable speedup or meet the formal50ms p95/
80ms p99 gate. The command wake removes avoidable sleep before dispatch, but
output arriving just after a host poll can still wait for the next50ms poll.
Software rendering, disk sync and concurrent work also remain in the path.
Further profiling/host change notification and isolated hardware measurement
are required; do not reduce durability to improve the numbers.

Headless raw-PTY regressions check ordered keys and trailing duplicates; helper
compilation and fixed-source bounds are covered by the X11 harness tests.
Reproduce on an already prepared signed SDK with the default plan first:

```sh
python3 scripts/isolated-ime-x11.py --fixture latency
python3 scripts/isolated-ime-x11.py --fixture latency --run \
  --app /absolute/path/to/frozen/pigade-app \
  --host /absolute/path/to/frozen/pigade-session-host \
  --output /absolute/new/path/outside/repository
```

Only the verified private display receives those fixed inputs. Do not run the
legacy active-desktop smoke path or substitute the user's Wayland/X11 endpoint.
