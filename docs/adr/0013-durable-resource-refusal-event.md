# ADR0013 — resource refusal is one durable raw-output decision

Status: accepted storage/recovery slice; live admission disabled, 0.0.1-dev,
2026-10-17. This does not pass a global heap/RSS budget or stage0.

## Decision and requirement clarification

SPEC7.4 requires an ordered durable journal before terminal-model publication,
bounded queues and no raw-output loss. It does not require fsync before a
strictly read-only preflight/reservation. The earlier research plan added that
stronger order and proposed pending/apply pairs. That proposal is superseded.

A future conservative whole-event preflight and shared reservation occur before
mutation. One CRC-covered kind then records the decision with the **unchanged**
raw payload: existing Output=1 for accepted output, OutputRejectedResource=5 for
refused output. Both use one ordinary journal append/fsync; only accepted output
may mutate the authoritative model after successful sync. No extra accepted
payload wrapper, second fsync or separate model/journal sequence space is needed.
If refusal persistence fails, it is an explicit storage failure with no mutation;
it is not a durable refusal. Raw bytes cannot be promised durable after failed
storage. There is no automatic retry or skipping the refused event.

The new journal policy is the exact existing terminal CORE_PROFILE plus
`;journal=resource-refusal-v1`. Legacy terminal/checkpoint semantic schema2 is
unchanged; the event policy is separate. Readers accept exactly the legacy or
new profile, reject kind5 under legacy semantics before mutation, and stop at
kind5 under the new policy with failed sequence/raw-retained diagnostics.
Production launches **still write the legacy profile and never emit kind5**.
Enable the new policy only with the complete estimator/reservation path.

## Refused suffix and checkpoint boundary

Successful kind5 append seals the journal writer. Subsequent appends fail before
writing; reopening fully scans and restores the seal even if a CRC-valid suffix
was manually inserted after refusal. Readers can inspect all raw records, while
model replay stops at the refused event. Failed/uncertain writes poison the writer
without returning a receipt, as before.

Durable Receipt now carries its event kind. Both synchronous and asynchronous
checkpoint entry points reject refusal receipts before waiting/capturing; the
persistence path checks again. A busy global checkpoint slot cannot disguise an
invalid watermark as deferred work. No later ordinary receipt can be obtained
from the sealed writer, so compliant publishers cannot create a checkpoint at
or after refusal. Fast recovery continues trusting source-bound, caller-validated
checkpoints under that writer contract; it does not scan the whole prefix.
CRC is corruption detection, not authentication or proof that an arbitrarily
forged model matches its source. Manual forged receipts/checkpoints remain
invalid callers/artifacts, as for existing checkpoint semantics. Hot-only legacy
checkpoints with valid raw input already force full replay.

Replay caches accept only the two exact supported journal policies and the exact
cold-format suffix. New-policy caches retain the same inode-bound marker,
lease, quarantine and unknown-file preservation rules. No existing sources,
checkpoint bytes or profiles are rewritten.

## Evidence and next steps

Storage tests cover exact1MiB/all-byte raw payload, every partial-frame cut,
kind-byte CRC tampering, uncertain fsync/no receipt, immediate/reopened sealing
and a manually inserted CRC-valid suffix. Host tests compare the complete
normal/alternate grids, cursor/modes, Unicode and partial parser continuation
plus **every nonzero cold record** across live prefix/full replay/checkpoint
+tail. Refusal as the first record keeps an empty model and no process. Clean
new-policy replay preserves existing schema2 checkpoint semantics. Refused
recovery cannot pass the clean recovery oracle.

A real host-process recovery scenario serves the retained frame and all cold
records for new-policy refusal and legacy-policy mismatch without respawn or
source mutation. Existing invalid resize/timer/Exit and healthy-generation cases
remain checked. Scoped cache and checkpoint-busy regressions cover the added
boundaries. See the resource-refusal report for fresh validation.

Still required: defensible persistent/transient bounds (including COW marks,
CSI-only grid operations, parser copies, frames, checkpoint and IPC ownership),
shared reservations, replay-time admission, live rejection/backpressure and a
resize operation outcome before kernel side effects. The resource-refusal reader
alone provides none of those protections. Resize success followed by journal
failure remains a separate explicit gap; do not record a failed kernel resize as
an applied model event.
