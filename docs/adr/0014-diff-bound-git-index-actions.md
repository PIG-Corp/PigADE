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
