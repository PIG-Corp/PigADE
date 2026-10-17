# Isolated provider PTY diagnostic plan

Status: harness implemented; one root-supervised isolated run exercised all
three installed providers. This is a bounded continuation of the stage-0
real-CLI check in SPEC §10.4, not provider integration certification.

## What the harness does

`python3 scripts/provider-pty.py` is plan-only and does not spawn a process. The
live path is Linux-only and requires both `--run` and
`--acknowledge-disposable-home`. It starts a private session host and launches
one real provider TUI with no prompt or resume arguments. The provider runs in a
Bubblewrap namespace with a private PID/network/IPC/UTS view, no network, a
read-only `/usr`, an empty temporary `HOME`, an empty read-only workspace, an
empty `/run`, and an allowlisted environment. It receives the PTY supplied by
the host, but the harness sends no input bytes. The desktop display, runtime
directory, activation token and session bus variables/sockets are not passed.

The harness first launches a fixed, small DSR probe through the same host/PTY
path. It writes `CSI 6 n`, waits at most two seconds for the terminal model's
initial-cursor-position reply, and reports only whether `CSI 1;1 R` arrived.
This is deterministic evidence for the host terminal-query response path; it
does not attribute any individual query response to the provider TUI.

The provider session uses the existing durable launch IPC once, with fresh
session, generation and operation UUIDs. If the launch response is ambiguous,
the script only polls `list` for that exact session/generation; it never retries
the mutating launch. The test observes bounded PTY output metadata, requests
two host resizes and checks the resulting frame geometry and output counters,
then stops that exact session. It retains no provider frame text or title in
the result. The host state is a unique mode-0700 directory under a private path
outside the repository. Successful runs remove it after cleanup. Failed runs
retain it for diagnosis, including private journals and logs, and write a
mode-0600 `failure.json` containing executable hashes, the failure stage and
the last bounded geometry/counter frame summaries. That JSON excludes
exception text and provider output; the retained durable state directory can
contain sensitive terminal output and must not be copied into evidence or Git.
Process cleanup opens a Linux pidfd, rechecks boot ID/PID/start time, then sends
signals through the pidfd; it never signals a process group. The host process
is its direct unreaped `Popen` child while `poll()` is `None`, so its PID cannot
be reused before reaping. Bubblewrap `--die-with-parent` contains provider
descendants in its private PID namespace; uncertain launch outcomes never
trigger a relaunch.

The durable state directory and IPC runtime directory are separate. The
session host receives the durable state path plus a new mode-0700 `/tmp/pga-*`
runtime directory, so the Unix socket path remains under the Linux `sun_path`
limit even when the private state path is long. Cleanup removes only that
fresh exact runtime directory after the owned host exits; failed durable state
is retained as described above. The fixed DSR probe may report its successful
marker after the short-lived child has exited; the real provider must remain
running during the observation window.

## What this can establish

An eventual passing run can establish that a named installed CLI process starts
under the pinned host/PTY setup, emits some terminal bytes without a task
prompt, remains alive for the short observation interval, and causes observable
PTY output after the requested resize events. It also establishes the DSR
response for the fixed companion probe. The JSON records only geometry, mode
bits, sequence, event/output counters, executable hashes and status.

It cannot establish which terminal queries the provider issued or whether it
received each response, semantic correctness of rendered provider text, a
complete provider TUI, authentication, AI task execution, context receipt,
conversation identity, resume, GUI presentation, accessibility, IME, clipboard,
or cross-platform behavior. In particular, output/sequence movement after a
resize is not proof that every TUI control repainted correctly. No provider task
or initial prompt is part of this diagnostic.

## Running after review

The default command prints a plan only:

```sh
python3 scripts/provider-pty.py --provider codex
```

An explicit live run requires a built host and a new empty temporary provider
home. It must be performed only after review of the harness and only in an
environment where Bubblewrap namespaces work:

```sh
python3 scripts/provider-pty.py \
  --provider codex \
  --host-binary target/release/pigade-session-host \
  --directory "$HOME/.local/state/pigade/provider-pty" \
  --run --acknowledge-disposable-home
```

Use `--provider claude` or `--provider opencode` for the other installed CLIs.
The script does not accept provider argv or prompt text. It launches Codex with
read-only sandbox, no approval requests and no shared daemon; OpenCode with
external plugins disabled; Claude Code with no prompt and no permission-bypass
flag. All three are network-isolated by Bubblewrap. The provider home is fresh
and disposable, so an authentication screen or immediate exit is an expected
diagnostic outcome; do not type credentials or task text into it.

The path `/usr` is exposed read-only for system-installed CLI runtimes. If the
provider executable is outside `/usr`, only its exact resolved file is mounted
read-only; the preflight reads only the four-byte ELF magic, so a user-home
script install that requires modules elsewhere will fail closed. The frozen
session-host copy is checked against the source SHA-256 captured before copy;
the run aborts if the hashes differ. The CLI matrix currently
records Codex 0.162.1, Claude Code 2.1.259 and OpenCode 1.18.22 based on prior
`--version`/`--help` probes. The official
Codex documentation says a bare `codex` invocation starts the interactive TUI,
and documents exact-ID resume separately; this harness supplies neither an ID
nor a prompt and does not use `--last` ([OpenAI Docs: developer commands](https://learn.chatgpt.com/docs/developer-commands#codex-resume)).
The installed binary's own version/help output remains the source for its
flags, while the current official docs may describe a newer release.

The final 2026-10-17 isolated run passed Codex and OpenCode startup, both
requested resizes and the companion DSR probe; both remained running during
the one-second observation. The Claude PATH entry is a 110-byte shell wrapper
(`/usr/bin/claude`, SHA-256
`356673de39ab5e9e4d19314c33b2d0b01fa06519a44c3374aa947bfbd71a4049`) that
executes `/opt/claude-code/bin/claude`, outside the mounted `/usr` tree. The
standalone ELF at that exact path was inspected by metadata only (216,677,784
bytes; SHA-256
`f7dd62ae415378018cd21dd950eb3bac174ab085830304d3b8b098146bfd47b6`). To test
that binary without mounting `/opt` or the user home, the harness now accepts
`--provider-executable PATH`; the override must be executable and have ELF
magic, and Bubblewrap exposes only that exact file read-only at
`/pigade/provider-cli`. Results record the PATH launcher and selected binary
hashes/kinds without recording their paths. For example:

```sh
python3 scripts/provider-pty.py --provider claude \
  --provider-executable /opt/claude-code/bin/claude \
  --run --acknowledge-disposable-home
```

The final Claude override run emitted PTY frames and completed both resizes,
then was no longer running at the final snapshot. A fixed phrase detector over
that run's private disposable terminal replay classified the screen as an
Anthropic API endpoint connectivity failure. The fresh-home log contained no
records, and replay reported no host storage error. Because networking was
deliberately unavailable, this run does not test authentication or task
execution. Network and filesystem isolation remain in place; do not infer that
either should be relaxed. Curated metrics and the report are in
`benchmarks/results/provider-pty-20261017/` and
`docs/reports/provider-pty-20261017.md`; raw terminal output and the disposable
runtime remain outside the repository.

Run the pure harness tests with:

```sh
python3 scripts/test_provider_pty.py
```

Those tests use only command builders, mocked host IPC, and disposable
synthetic Python children. They never execute a provider or open a window.
