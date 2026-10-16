# VM reboot recovery evidence — 0.0.1-dev

**Status:** an actual isolated two-boot guest run now verifies synthetic cold
history and checkpoint recovery. It does not establish provider conversation
resume, product layout recovery, power-loss behavior or SPEC REC-03 acceptance.

## Recorded guest run

The earlier partial manifest is preserved in
[`vm-reboot-hot-history.json`](../../benchmarks/results/vm-reboot-hot-history.json).
It recorded zero cold records, so its `cold_history_equal: true` compared empty
histories and did not exercise the archive.

The fresh cold-history manifest is
[`vm-reboot-cold-history.json`](../../benchmarks/results/vm-reboot-cold-history.json).
It records a btrfs-owned guest disk, an Alpine 3.24.2 x86_64 cloud image with
valid official signature and pinned SHA-512, and release host binary SHA-256
`ca4cab88f758823dcda263fe62e3426e254de3f5496250e7eb98db7f008670c6`. QEMU ran
headless with network and input devices absent; the only host mount was the
read-only staged test payload. The guest recorded distinct boot IDs
`f8fd52e8-934f-4c62-a709-ccf80183773c` and
`ed04debc-d22a-4f06-9a31-fe924fcb5a1b` before and after its explicit reboot.

Four synthetic PTY sessions each recovered 2,467 cold records, equal hot/parser
checkpoint state and equal frame state. Full-replay/checkpoint-tail comparison
reported `complete_state_equal`, `frame_equal` and `cold_history_equal` for all
four sessions. Each checkpoint-tail read 356 journal bytes and completed in
65.8–68.95 ms in this component-level recovery check. The scenario refused
input to `launch_outcome_unknown` generations and recorded no retry PID. The
CLI invocation marker list was unchanged (`0`, `1`, `2`, `3`) across the reboot,
so no scenario invocation was repeated.

The fresh run uses synthetic shell PTYs and 12,500 lines per session; it is not
a real provider CLI conversation or AI task resume. It does not verify product
layout restoration, storage-cache-loss or power-loss behavior. It is partial
REC-03 evidence, not REC-03 acceptance. The separate 24-hour soak remains open.

## Harness setup findings

The attempts exposed distinct seed problems. QEMU rejected the first
read-only IDE-device form. A later CD-ROM form was detected by cloud-init as
`/dev/sr0` but failed to mount, so that run used only the DMI seed. The working
setup creates a raw FAT seed using 512-byte sectors and attaches it as a regular
IDE disk; cloud-init mounts it read-only. Cloud-init is configured to probe
`NoCloud`, with QEMU DMI serial `ds=nocloud`; the guest reports cloud-init 26.1.
See the [cloud-init NoCloud data source](https://github.com/canonical/cloud-init/blob/26.1/cloudinit/sources/DataSourceNoCloud.py)
and [NoCloud configuration reference](https://docs.cloud-init.io/en/26.1/reference/datasources/nocloud.html).

The harness uses the signed Arch QEMU SDK prepared by
`scripts/prepare-vm-sdk.py`, an Alpine cloud image with signature verification,
a private QMP socket, `-nic none`, and no display/input device. Run directories
are retained; earlier failed UUID-named attempts remain evidence and must not be
overwritten.

## Commands

On a durable filesystem, prepare the SDK and VM inputs, then explicitly run the
guest reboot scenario. `--prepare` never starts QEMU; `--run` performs two
guest boots and writes a run manifest. Use fresh output directories and preserve
each result:

```sh
python3 scripts/prepare-vm-sdk.py --prepare
python3 scripts/vm-reboot.py --prepare \
  --directory /absolute/durable/path/pigade/vm-reboot
python3 scripts/vm-reboot.py --run \
  --directory /absolute/durable/path/pigade/vm-reboot
```

The current runner additionally requires each recovered session to contain at
least one cold record and compares baseline cold history. The tracked cold-run
manifest records four sessions × 2,467 cold rows. Earlier runs remain as
separate artifacts: [read-only seed failure](../../benchmarks/results/vm-reboot-initial-readonly-seed-failure.json),
[console timeout](../../benchmarks/results/vm-reboot-console-timeout.json),
[wrong datasource](../../benchmarks/results/vm-reboot-wrong-datasource.json), and
[seed mount failure](../../benchmarks/results/vm-reboot-seed-mount-failure.json).
Keep all attempts; do not overwrite failed evidence. A rerun should record the
new manifest path, binary hash, boot IDs, session/generation identities, retry
evidence, cold-row counts, full recovery output, and any failure.

## Completed-corpus VM path

`--corpus INNER_DIRECTORY --reference RECOVER_RESULT_JSON` stages only a
completed, four-session corpus with all full/fast/live equality proofs and
the exact same frozen host binary used by `scripts/recover-corpus.py`.
Source files are whitelisted and hashed; runtime SQLite, locks, sockets and
private replay caches are excluded. The private staging copy is read-only.
The guest copies it to a newly created 64 GiB qcow2/ext4 data disk, identified
by its unique device serial before formatting. The second boot requires the
existing filesystem and copy marker; it does not format or recopy data.
QEMU has no network, display or input devices and sees only these staged files.

The diagnostic run in
[`vm-reboot-corpus-small.json`](../../benchmarks/results/vm-reboot-corpus-small.json)
uses the explicit `--allow-small-corpus` override: four 1 MiB journals and
100 resizes each, not the required four 1 GiB corpus. Distinct boot IDs were
`3ae73e6b-2041-41e2-bd27-8d735c45dfa4` and
`f3fca9c7-7c6d-4b32-9648-2425311e389f`. Both boots matched the same-binary host
reference, retained 3,197–3,199 cold records per generation, and reconstructed
all six private checkpoint-tail records as exact canonical bytes through the
bounded history API. Journal-tail reads were 405–441 bytes. Four host frames
were available in 466.87 ms and 1,166.85 ms respectively. These two concurrent
diagnostic samples do not establish a percentile, UI-present time or REC-08.
Every source fixture hash remained unchanged. The completed corpus's earlier
full cold oracle runs outside the VM; this VM path verifies the persisted
source inventory, recovered frames and canonical tail, not a fresh full
1 GiB replay inside the guest.

The failed owned-process-identity attempt is retained in
[`vm-reboot-corpus-identity-failure.json`](../../benchmarks/results/vm-reboot-corpus-identity-failure.json).
QEMU exited when its removed QMP directory could not be bound. Cleanup now
uses a Linux pidfd opened for the exact unreaped Popen child, including when
later identity capture fails; a regression exercises that boundary with a
harmless owned child. The SDK also now includes signed `qemu-img` and its
private library path; see [the SDK record](../research/vm-sdk-qemu-img.md).
Other preflight failures (missing image tool and its loader library path)
occurred before a guest boot and were corrected before the passing run.

```sh
python3 scripts/recover-corpus.py --corpus /absolute/completed/inner-directory \
  --output /absolute/new/recovery-reference --iterations 30
python3 scripts/vm-reboot.py --run --directory /absolute/durable/vm-runs \
  --sdk-manifest /absolute/prepared/sdk/evidence/manifest.json \
  --corpus /absolute/completed/inner-directory \
  --reference /absolute/new/recovery-reference/result.json
```

The default corpus mode refuses small or incomplete fixtures. The full corpus,
real-time soak, product layout/provider recovery, storage quota, measured total
memory and REC-08 acceptance remain open.
