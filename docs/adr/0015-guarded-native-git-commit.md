# ADR0015: Guarded native Git commits

Status: accepted for the limited native commit slice, 0.0.1-dev.

PigADE commits only the exact staged state shown in review. The host issues a
short-lived opaque ticket bound to the workspace/repository identity, attached
local branch (including an unborn branch), HEAD, index bytes and identity, and
the reviewed staged paths. Before launching Git, it revalidates those inputs,
computes the frozen index tree with Git, and durably records the attempt. The
native command is Git 2.55.0, pinned to the installed ELF executable identity;
it runs `git commit` with the reviewed message, no editor/status, and verbatim
cleanup. Git's configured signing and native commit machinery remain enabled.
The guard accepts only the reviewed branch update with the reviewed old HEAD,
tree and parent list.

To guard that native ref transaction without suppressing user hooks, each
operation uses a private command-local `core.hooksPath` containing wrappers for
Git's traditional commit hooks. Each wrapper directly invokes the corresponding
hook from the original hooks directory, preserving its arguments, working
directory, environment, input, output and exit status. Git's configured
`hook.*` commands remain in native dispatch and are not replayed by PigADE.
Git's `hook.<event>.enabled=false` setting disables configured hooks for that
event; it does not disable the traditional wrapper that enforces the guard.
Reference-transaction callbacks unrelated to the reviewed branch (including
Git's cleanup refs) pass through to the user's original hook. Hook side effects
are native Git behavior and cannot be rolled back by PigADE.

The `prepared` reference-transaction callback runs after Git locks refs and
before publication. It rejects a changed old OID, target, commit tree, or parent
list and durably records the exact candidate. The `committed` callback records
a separate durable proof after Git publishes the ref. That proof attests that
the commit was published; it is historical evidence and does not claim that
HEAD still points to it later. An exclusive run marker and inherited pinned
host executable descriptor identify the exact Git process across host restart.
An exact owner whose boot/start or executable identity changed fails closed; it is not treated as an unrelated nested Git hook. The native Git process can finish after its host parent dies. A lost response or
missing publication proof never triggers an automatic retry: the result remains
Unknown without matching publication proof. Inspection never starts Git; explicit acknowledgement permits a new deliberate attempt but does not relabel the old result Applied.

This development slice requires Linux `/proc` process identity and supports
attached local branches only. It refuses merge/rebase/sequencer state, detached
HEAD, sparse/split indexes and interactive credential prompts (helper stdin is
closed). It does not implement commit-all, amend, remote writes, discard, or
hook rollback. Bounds are 120 seconds per attempt, 256 KiB captured output per stream,
8 MiB index, 512 staged paths, 8192 UTF-8 message bytes, two preview leases
with ten-minute expiry, and 4096 durable operation receipts. A full receipt
store refuses new commits rather than deleting recovery evidence.

Evidence: nine real-IPC tests passed, including native SSH signing and
`verify-commit`, stale-index and hook-changed-tree rejection, host termination
with recovery, and refusal to claim Applied when the publication proof is
missing or forged. The workspace suite, seven focused commit-module tests, app tests/strict Clippy and debug/release builds pass. The existing private release catalog fixture `/tmp/pigade-git-commit-20261018-02` passes review, exact tree/parent/message, original hooks once, preserved unstaged bytes/process identities, and tab close/reattach. Root inspected fresh review/result captures; the result list shows the new HEAD and no staged entries. This is not full Git MVP, native Wayland, reboot, soak, performance or accessibility acceptance.

References: [Git 2.55 hook dispatch](https://github.com/git/git/blob/v2.55.0/hook.c),
[native commit path](https://github.com/git/git/blob/v2.55.0/builtin/commit.c),
[ref transactions](https://github.com/git/git/blob/v2.55.0/refs.c),
[hook contract](https://github.com/git/git/blob/v2.55.0/Documentation/githooks.adoc).
