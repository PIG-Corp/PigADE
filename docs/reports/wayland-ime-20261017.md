# Wayland IME fixture result — 2026-10-17

One isolated Linux Wayland IME fixture passed with PigADE, Sway 1.12/Pixman,
Fcitx 5.1.23, and its Pinyin input method. The separate SDK preparer remains a
non-executing download/verification/extraction tool; the successful runtime
was explicitly started by `scripts/isolated-ime-wayland.py --run` and stayed
inside the verified private namespace path.

## What the fixture proved

The private compositor advertised `zwp_text_input_manager_v3`,
`zwp_input_method_manager_v2`, and `zwp_virtual_keyboard_manager_v1`. The
harness verified the actual inner Python process' kernel namespace identities
for mount, PID, user, IPC, and network namespaces before releasing its
one-shot startup gate. Bubblewrap 0.11.2 omitted the user namespace ID in its
own early metadata; the parent used kernel namespace handles for the actual
inner process as authority. The private network namespace exposed loopback
only. Host display, session/system bus, GPU and input paths were absent.

Private Sway was the peer for both the Wayland and Sway IPC sockets, with
matching boot ID, PID, start ticks and socket inode evidence. The app window
was uniquely matched by its PID and focused through that private Sway IPC
connection. Fcitx ran with a fresh private home, D-Bus session and Pinyin
profile; `fcitx5-remote -n` confirmed Pinyin active.

One persistent test-only `zwp_virtual_keyboard_v1` object sent `n i h a o`.
Before Space, the PTY received **0 bytes**. After Space, the child read exactly
the six UTF-8 bytes `e4bda0e5a5bd` (`你好`), observed **0 extra bytes**, and
emitted its own unique ACK. The ACK was child-generated and was not inferred
from terminal echo. The helper accepts no arbitrary input text; its state
machine permits one COMPOSE, then one COMMIT, and QUIT at any point.

The root visually reviewed both retained captures: the preedit view showed
`ni hao` and the `你好` candidate near the insertion point; the post-commit
view showed the candidate popup cleared and the visible child ACK. This was a
human review, not automated image recognition.

![Preedit with candidate](../../crates/app/evidence/ime-wayland-20261017/preedit.png)

![Committed text and child ACK](../../crates/app/evidence/ime-wayland-20261017/committed.png)

## Run identity and evidence

The curated run summary is
[`summary.json`](../../crates/app/evidence/ime-wayland-20261017/summary.json);
previous fail-closed attempts are summarized in
[`earlier-attempts.json`](../../crates/app/evidence/ime-wayland-20261017/earlier-attempts.json).
The full disposable runtime state remains outside Git at
`/home/afterburner/.local/state/pigade/iw-v3-persist-1017`.

| Component | SHA-256 recorded by the passing run |
| --- | --- |
| App release binary | `484b1eacdb8df844c422e298470a3bd7736dc7907e8b7af79942cf0681bb9669` |
| Session-host binary | `610efac4b817116ed761217a9738485c2b499bf6fb05a30ebce7cb6dbf7e4791` |
| Wayland harness source | `a0c0f834bcf4ce551500391f7bae750971a196b6b6ec4dfdecc31d658002216c` |
| Persistent keyboard helper | `916e71bf2da35bacaf2bb4f385b50a9ae080e3132542e174745b315cbac5d426` |
| Helper source | `c18a2b89da86b03daa7cfaaaca7fa01851089a4afec5203fa14a3b03e8de08fa` |
| Wayland SDK manifest | `a22c5a441c07f9c29b242d3158b656473dc9b0fd65fbed428360c54f4d5a17e2` |
| X11/Fcitx SDK manifest | `7a3198632791a7fea235e3d2604ee21c838a5df823588fe2581cf9e506976d6a` |
| Software-renderer SDK manifest | `a7426b082a9224227392f964e9da3238066e9c2887201501e69a3fa5f59caa59` |
| Preedit screenshot | `d7bf1353aeda092746e24ac38a8b6c2cd276eafdd6e210d31358bce4cf5a0e02` |
| Post-commit screenshot | `8db7271fde6e5b0557c3ae17240bfe6c641b5ec96c8e80e3cd7bdb15d6ee771c` |

The run's font profile was `pigade-terminal-render-profile-aliases-v1` with
manifest SHA-256
`f2a3ad82c22a5be1cf21c431ff04473b31b4cb4919a30c1379f957cb6edd82e3`.
It recorded the four derived terminal mono faces and one pinned Noto Sans Mono
CJK JP face in the tested executable. This validates those font bytes were
embedded in that app binary; it is not an all-Unicode or cross-platform
glyph-shaping proof.

Neither release binary currently at the command paths matches the saved run.
At report validation, `crates/app/target/release/pigade-app` hashes to
`3eba79d2d9b84d14e280a6a1de8ac07409432f49db6c61cedcd8a13e747af39a`, and
`target/release/pigade-session-host` hashes to
`477d5f81375b38a15aae177144f8e3515017649100bf0424dfd89d4646b8d4d2`. A new
run with those current files is new evidence, not an exact replay. The runner
records app, host, harness and SDK hashes at start; compare those values when
reproducing.

## Failed attempts and corrections

The five earlier private attempts are preserved as bounded metadata outside
the repository. They failed closed before sending fixture input:

1. The first outer validation imported the embedded-font verifier from the
   wrong helper module.
2. The next attempt failed at bubblewrap startup; its report incorrectly
   remained `running` because descriptor cleanup and final failure reporting
   needed repair. The namespace gate was not released.
3. An attempt rejected missing user-namespace metadata from bubblewrap before
   services started.
4. An attempt compared the bubblewrap setup PID's user namespace with the
   actual inner Python process and failed closed before services started.
5. One private app-start attempt could not confirm active Pinyin, so it sent no
   fixture input.

The passing run resolved those issues by binding the kernel attestation to the
actual inner process via a private peer-credentialed socket and one-shot gate,
and by creating one persistent virtual keyboard before app startup. No failed
attempt was automatically retried or replaced with an unverified run.

## Reproduction

The exact flags and defaults are defined by
[`isolated-ime-wayland.py`](../../scripts/isolated-ime-wayland.py) and
[`prepare-wayland-ime-sdk.py`](../../scripts/prepare-wayland-ime-sdk.py). Run
the first command only to inspect the plan; the second prepares signed SDK
files under `/tmp` without starting desktop services or input:

```sh
python3 scripts/isolated-ime-wayland.py
python3 scripts/prepare-wayland-ime-sdk.py --plan
python3 scripts/prepare-wayland-ime-sdk.py
python3 scripts/test_prepare_wayland_ime_sdk.py
python3 -m unittest scripts.test_isolated_ime_wayland -v
```

For a new private run, use the release binaries and a fresh output directory
outside Git. The recorded app remains frozen at
`~/.local/state/pigade/ime-x11-wake-final-app-20261017`, and the host at
`~/.local/state/pigade/soak/actor-stage-host-20261017`. Verify the full hashes
above before using those files; current build paths may contain newer versions.

```sh
ime_run_path="/home/afterburner/.local/state/pigade/ime-wayland-rerun-$(date -u +%Y%m%dT%H%M%SZ)-$$"
python3 scripts/isolated-ime-wayland.py --run \
  --app crates/app/target/release/pigade-app \
  --host target/release/pigade-session-host \
  --wayland-sdk /tmp/pigade-wayland-ime-sdk-20261017 \
  --x11-sdk /tmp/pigade-x11-ime-sdk-20261017 \
  --platform-sdk /tmp/pigade-platform-sdk \
  --output "$ime_run_path"
```

The SDK manifest status `prepared-not-executed` describes the SDK-preparation
operation only. It does not negate the separately recorded passing runtime.
The SDK uses host `/usr` ABI libraries and its archived set is x86_64-specific.

## Remaining boundaries

- Only one Sway 1.12 / wlroots 0.20.2 / Pixman and Fcitx 5.1.23 Pinyin path
  was tested. There is no universal compositor, IME, locale or OS claim.
- Pixman is software rendering. This does not test hardware GPU/Vulkan behavior
  or fractional-scale output.
- The images were inspected by a person; the harness does not classify preedit
  text or popup placement automatically.
- Clipboard, arbitrary user text, and other input methods were not tested.
- The native app's IME session-generation/reset barrier across panel or target
  changes remains open; stale callbacks must be addressed and tested separately.
- The virtual-keyboard helper is test-only and does not establish a product
  input mechanism.
