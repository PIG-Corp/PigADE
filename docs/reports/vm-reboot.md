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
