# Foundation verification — 0.0.1-dev

Date2026-10-17. This is a development checkpoint, not MVP/beta acceptance. SPEC gate10.4 remains open. Separate control applications were removed by explicit user instruction; the tests below exercise PigADE itself.

## Executed checks

Root workspace: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release --workspace --bins --examples`.72tests pass:13layout,17hostunit,8realPTY/recovery,1saturatedinput,23storage,10terminal. Includes100client reopens,1000realresizes, UI-independent lifecycle, hostSIGKILL/no duplicate launch, quiet/timer/resize-only checkpoints,20kquery replies,50persistedundo/redo and60crossworkspace transfers preserving the running process.

Actualapp: `crates/app/bootstrap.sh {build,test,clippy} --locked`; clippy uses `--all-targets -- -D warnings`.13headless tests pass. Upstream GPUI retains two future float warnings and Cargo dependency future-compat notices, documented in sourcecheck. No dependency warning is presented as a first-party lint success.

Vendor commands: `cargo test --manifest-path vendor/vte/Cargo.toml --features ansi,serde` passes56tests. Alacritty with explicit local VTE patch passes146lib and45reference tests, including45captures×3checkpoint cuts. Exact command: `cargo test --manifest-path vendor/alacritty_terminal/Cargo.toml --config 'patch.crates-io.vte.path="/home/afterburner/Projects/PigADE/vendor/vte"'`; replace absolute root for another checkout. The sole golden adaptation changes6generated hyperlink-ID suffixes; original/patched hashes and prior44/45 result are preserved in patchnotes. Other golden fields and all comparisons remain intact.

Independent foundation review reproduced twoHIGH availability/recovery defects: valid legacyv2 placement became unreadable, and JSONescaping could commit a catalog too large for IPC. Both were repaired and independently retested through a real host. See foundation-review.md. Layout serialization also now preserves fractional f64 weights exactly and rejects JSON-too-deep candidates beforecommit.

## Private rendering proof

`python3 scripts/prepare-platform-sdk.py` verifies signed pinned local-only Weston/Lavapipe packages. `python3 scripts/isolated-render.py --app PATH_TO_DEBUG_APP --host target/release/pigade-session-host --output FRESH_DIRECTORY` launches only inside new mount/PID/user/IPC/network namespaces, private bus/runtime/socket and stub devices. Host home/runtime/config, X11 sockets and GPU/input devices are not mounted. Beforeappstart, socket SO_PEERCRED and owned compositorFDs are checked. No keyboard/mouse injection occurs.

The actualapp rendered fourliveBash panels at1280×800. A first screenshot exposed a negative-descent metric error and interleaved backgrounds erasing glyphs. Afterrepair, visual inspection showed complete Cyrillic, combining accent and CJK example text. Four69×15PTY geometries were acknowledged. Killing/reopening UI preserved each generation/PID. Evidence is under crates/app/evidence/isolated-*; this is software rendering, not physicalGPU/Unicode/font/fractional-scale acceptance. Normal last-window closure has a quit hook; the recorded process comparison specifically uses UIkill/reopen.

## Measurements and limits

Recorded hardware/component benchmark precedes this slice: native-components.json has fsyncp95=1.425ms, one checkpointrestore+framep95=52.727ms, fullreplay+comparisonp95=324.542ms on the scoped short warm-cache corpus. These are not GPU/presentation results.

Two reproducible60s4-stream runs use releasehost and durablebtrfs, noUI/providers. Beforeoutputcoalescing each60MiBproducer took~82.3s, so throughput failed; allbyteCRC/state integrity still passed. Afterbounded adjacent-output batching each took59.985s, sustaining4×1MiB/s. Every journal CRC, generation/sequence and full terminal/parser checkpoint comparison passes. Host sampledpeakRSS=30,334,976bytes, averageCPU82.60%of one logicalCPU.112input→durable-snapshot-observedACK samples: p50=30.414ms,p95=67.648ms,p99=113.127ms. Snapshot polling/JSON/harness are included; presentation is absent. The 60s observed-exit deadline is not passed because drain/observation occurs afterproducercompletion. Raw failures and improved results are both retained. Do not rename this an input-to-present gate pass.

Unverified release blockers: immutable coldhistory/index and aggregateheapbudget;4×1GiB/100kresizecorpus; VMreboot;24hreal-timesoak; truepresentation/frame/idleGPU/RSSbudgets; actualAIcontext+resume/two-versionmatrix; X11/IME/clipboard/accessibility/scale/GPUrecovery; otherOSbuilds; complete productUI/worktrees/context/packaging and end-to-end MVP. Detailed nextsteps are in AGENT_HANDOFF and cold-history-plan. No beta/stackapproval is implied.
