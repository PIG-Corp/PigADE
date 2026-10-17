# Codex conversation continuity — 0.0.1-dev

Installed `codex-cli0.162.1` completed two real provider turns using its native
authentication and rollout history. `codex --no-daemon exec --json` emitted one
`thread.started` UUID and acknowledged the initial synthetic-context prompt. A
second process used `exec resume` with that exact UUID and returned the synthetic
nonce from the prior turn without receiving it again. Its structured event
confirmed the same thread ID. No tools, file operations, search or MCP items
were observed; the model could emit reasoning items.

[Redacted result](../../benchmarks/results/codex-conversation-20261017/result.json)
binds the CLI binary, exact CLI version, harness source, both output streams,
thread ID and nonce through hashes. It saves no raw provider output or full
conversation ID. `status=passed` is the final outcome; `phase` records the last
spawn intent entered before that outcome. No retry, new-conversation fallback
or `--last` occurred. The full native rollout remains owned by Codex in the
existing CODEX_HOME; the harness does not inspect/copy that history or auth.

The CLI ran in a fresh0700 cwd outside Git, with read-only tool sandbox,
`--ignore-user-config`, `--ignore-rules`, a desktop-free allowlisted environment
and disabled browser launcher. It used the normal provider network; this was
not an OS/network-isolated test. Codex itself writes its native conversation
state as part of normal operation.

This proves narrow exact-ID **non-interactive CLI** context continuity across
two processes. It does not prove interactive TUI resume, UI/host/reboot resume,
PigADE adapter capability enforcement, revisioned shared context, role delivery,
a long development task or the required two-version certification matrix.
Automatic resume remains disabled until the product binds the exact ID and
launch outcome durably.

The reviewed harness drains both pipes through EOF after a fast child exit,
bounds output/events/time and writes phase intent before process launches.
Seventeen tests include real harmless Python output/FD checks and synthetic
protocol/unknown-outcome tests. See the [reproduction plan](../research/codex-conversation-plan.md)
for exact flags and official documentation. A repeat creates another native
provider test conversation; do not rerun an unknown invocation automatically.
