# ADR0014: Diff-bound, durable per-file Git index actions

Status: accepted for the limited Stage/Unstage slice, 0.0.1-dev.

The CLI can change HEAD, the index and working files without changing PigADE's
catalog revision. A workspace revision alone cannot authorize a Git write.

The host issues an opaque, expiring ticket for the exact reviewed file side,
repository identity, HEAD/branch, index bytes/identity and working-file bytes/mode.
The UI sends that ticket once, never a shell command or arbitrary cwd. The host
revalidates these inputs, records a durable Unknown attempt before external
effects, builds a private index using Git's own object/index commands and replaces
the native index only under an owned `index.lock` with baseline comparison and
file/directory synchronization. The existing index file mode is preserved.
Explicit staging retains Git clean/EOL filtering; no working file is overwritten.

Saved receipts are bound to ticket, workspace revision, project, cwd, path, side
and action. Repeat/inspection returns the receipt, never repeats Git. Unknown
survives host restart and requires explicit acknowledgement; acknowledgement
does not relabel it Applied. Missing/corrupt receipts remain unavailable. Pending
receipts remain discoverable after catalog rename using their original revision.

The first slice deliberately supports regular files up to256KiB and indexes
up to8MiB, with8 live tickets/10-minute expiry and4096 durable receipts. Sparse
and split indexes, symlinks and conflicts are refused. A full receipt store stops
new actions rather than silently deleting recovery records. Stage All, discard,
commit, remote writes and receipt maintenance remain separate product work.
Native commit hooks/signing must retain their semantics when commit is added.

Evidence: `workspace_git_index` real-Git tests include rename/copy/deletion,
unborn HEAD, stale previews, foreign/replaced locks and faults after the attempt,
index rename and before the final result. `tests/project_setup.rs` covers real
IPC, host restart, duplicate tickets and a failing clean filter invoked once.
The existing private catalog UI fixture exercises four Stage/Unstage actions.
These tests do not claim power-loss durability or complete Git/MVP acceptance.


## Whole-status batch extension, 0.0.1-dev

Stage All and Unstage All use a separate expiring ticket for a complete frozen
status, repository/workspace identity, HEAD, index identity/bytes and all changed
regular working-file bytes/modes. Only explicit reviewed literal paths enter a
private candidate index. One native index.lock CAS publishes the whole candidate;
no worktree file is overwritten. Native clean/EOL filters remain explicit effects.
Changes to any frozen input, including a newly appearing untracked path, reject
the whole batch. The UI review is required; ordinary Enter never applies it.

Bounds:128 complete status entries,256 literal paths,256KiB per regular changed
file,8MiB aggregate working bytes and index,2 outstanding workspace previews for
10minutes,4096 separate durable batch receipts of at most2MiB each. A replacement
preview invalidates the unused ticket for the same workspace without locking
index writes. Per-file64KiB receipt bounds remain unchanged. Sparse/split indexes,
symlinks, submodules, conflicts and unsafe/control-character paths are refused.
Receipts are recorded before
filters/index changes; unknown outcomes survive restart and mutually block
per-file, batch and commit writes. Inspection never retries; acknowledgement
never relabels Unknown as Applied. Missing/corrupt receipts stay unavailable.
Original receipt revisions remain inspectable after workspace rename.

The additive index_batch_schema=1 capability does not require migration or
restarting a live legacy host. This extends the earlier per-file scope; discard,
remote writes and receipt maintenance remain separate work.

Batch evidence:60 focused Git-module tests (including11 batch tests),13
real-host project tests including batch IPC/restart/stale/Unknown handling,
434 app tests and37 harness tests pass. Private release catalog04 at
`/tmp/pigade-git-batch-20261018-04` passes both batch UI flows, exact staged
bytes/path sets, Enter/cancel, worktree/process/layout preservation, followed
by native commit/hooks and tab close/reattach. Review/result captures were
visually inspected. This is private software X11 verification; power-loss,
native Wayland, reboot, performance, soak and full accessibility gates remain
open.
