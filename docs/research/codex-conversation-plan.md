# Codex native conversation continuity harness

`scripts/codex-conversation.py` checks one Codex CLI conversation with two
non-interactive turns: `codex exec --json` starts it, and
`codex exec resume <exact-thread-id> --json` continues it. The first fixed
harmless prompt asks the model to retain a generated synthetic nonce; the
follow-up asks for that nonce without repeating it. A pass means the JSONL
stream identified one native `thread.started` UUID, both turns completed, and
the resumed turn returned the nonce. This is narrow CLI context-continuity
evidence, not TUI, provider-task, broad context-window, or long-term persistence
certification.

The script is plan-only by default and never spawns Codex in that mode. A live
run requires both `--run` and `--acknowledge-native-auth-and-network`, plus a
new `--directory` outside the repository. It creates a mode-0700 run directory
and child working directory, runs with `--sandbox read-only`,
`--ignore-user-config`, `--ignore-rules`, and `--skip-git-repo-check`, with no
stdin and bounded stdout/stderr, event count, line size, and per-turn timeout.
Pipe contents are drained through EOF even after quick child exit, so final
buffered JSONL events are retained. The child environment is allowlisted to
executable lookup, home/Codex home, locale, XDG config/data/cache/state paths,
TLS certificate settings, proxy settings, and supported native API credential
variables. Codex session/app-server/daemon controls and desktop/socket-routing
variables are excluded; `BROWSER=/bin/false` is set. The harness does not open
a GUI or type into a terminal. The child prompt is fixed; there is no task
prompt or shell passthrough option.

The harness does not inspect or copy Codex credentials or history. It inherits
the existing native `CODEX_HOME`/environment so Codex can authenticate and
resume its own saved rollout. This means Codex's native rollout storage itself
is not isolated into the run directory; only the harness cwd and its redacted
`result.json` are unique and private outside Git. `--ephemeral` is deliberately
not used because it disables persistence needed by resume. The run directory
contains hashes and pass/failure codes only; raw stdout/stderr, full thread ID,
prompt nonce, and transcript are never written there. The full thread ID is
held only in process memory long enough to pass it to the exact-ID resume
command. The result file is created exclusively with mode 0600 before any
process starts, then atomically updated and synced with a non-secret phase
intent before the version probe, initial turn, and exact-ID resume. If the
harness is interrupted mid-run, the retained `running` phase identifies the
last spawn intent; it is not a resume instruction. On any uncertain initial
phase, the script stops; it never retries a mutation, selects `--last`, or
silently creates a replacement conversation.

The initial and resume commands use argv arrays with a fixed string prompt,
not shell interpolation. The JSONL parser accepts exactly one completed
`agent_message` item while ignoring non-action reasoning items without saving
their content. It rejects tool/MCP/search/command item activity,
malformed events, missing/ambiguous `thread.started`, failed turns, oversized
streams, and an unexpected resume thread ID. The sandbox is a CLI policy for
model-generated commands; this harness does not claim OS/network isolation.
The provider API request still uses Codex's normal native authentication and
network path when the explicitly opted-in live run is performed.

Run pure tests without spawning Codex:

```sh
python3 scripts/test_codex_conversation.py
```

Plan only:

```sh
python3 scripts/codex-conversation.py
```

Example explicit live invocation, to be used only when a native provider turn
is intended:

```sh
python3 scripts/codex-conversation.py \
  --run \
  --acknowledge-native-auth-and-network \
  --directory "$XDG_STATE_HOME/pigade/codex-conversation/run-unique-name"
```

The directory must not exist before the command and must resolve outside the
repository. The tool writes only redacted run metadata there; Codex retains its
native rollout under its own `CODEX_HOME` as required for exact resume.

## Official and installed CLI evidence

The official [Codex CLI command reference](https://learn.chatgpt.com/docs/developer-commands#codex-exec)
documents `codex exec`, `--json`, `--sandbox read-only`, `--ignore-user-config`
(authentication still uses `CODEX_HOME`), `--ignore-rules`, and the exact-ID
`codex exec resume [SESSION_ID] [PROMPT]` form. The official
[non-interactive output reference](https://learn.chatgpt.com/docs/non-interactive-mode#make-output-machine-readable)
documents JSONL event names including `thread.started`, `turn.started`,
`turn.completed`, and `turn.failed`, with a `thread_id` field in
`thread.started`.

Locally installed `codex-cli 0.162.1` was inspected with `codex --version`,
`codex exec --help`, and `codex exec resume --help`. The installed parser also
accepted safety/config flags before `resume` while displaying resume help.
`--no-daemon` is a top-level option: `codex --no-daemon exec --help` succeeds,
while `codex exec --no-daemon --help` rejects the placement. Both initial and
resume argv place it before `exec`, preventing the CLI from using its
persistent daemon on the installed version. This flag is not a claim of full
process or OS isolation. The harness invokes non-interactive `exec` with an
explicit working directory, stdin closed, and a filtered environment. The
version probe records the installed `--version` string and the SHA-256 of both
executable and harness source. No provider task or network request was started
for this harness; native API credential variables are only inherited if an
explicitly opted-in live run is later performed.
