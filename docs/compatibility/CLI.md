# CLI compatibility matrix — stage 0, 2026-10-17

`supported` requires executed certification, not the presence of a help flag. `unknown` is deliberately not treated as supported. Raw probe results: [installed-cli-probes.json](installed-cli-probes.json). Probes only run `--version` / `--help`; they do not start a provider task, copy credentials or inspect private provider databases.

| CLI / exact installed version | Version/help in this environment | Generic PTY | Context delivery + receipt | Exact native resume | Automatic conversation binding | Auto-resume certified |
|---|---|---|---|---|---|---|
| Claude Code 2.1.259 | executed, exit 0 | unknown (interactive TUI not certified) | unknown | documented `--resume ID`; not executed | unknown | no |
| Codex 0.162.1 | executed, exit 0 | unknown (interactive TUI not certified) | unknown | documented `codex resume ID`; not executed | unknown | no |
| OpenCode 1.18.22 | executed, exit 0 | unknown (interactive TUI not certified) | unknown | documented `--session ID`; not executed | unknown | no |
| Previous supported version of each CLI | not selected/installed | unknown | unknown | unknown | unknown | no |
| Bash 5.3.3, Python 3.14 | actual PTY input/output, alternate TUI, resize and reconnect tested | supported in prototype tests | unsupported (explicit input only) | unsupported | unsupported | no |

These are detection results, not the final §9.2 two-version certification matrix. Unknown AI versions must stay generic; no provider-specific adapter or automatic resume is enabled yet. A future adapter must bind a concrete conversation through documented structured events or explicit user binding, preserve cwd/revision, never use `--last`, and never fall back to a new conversation after resume failure.

Official sources inspected:

- [OpenAI Docs: codex resume](https://learn.chatgpt.com/docs/developer-commands#codex-resume): exact session resume; cwd choice is part of the resume contract, and explicit `--cd` wins over `tui.resume_cwd`. This does not prove the installed version's context receipt or automatic binding.
- [Claude CLI reference](https://code.claude.com/docs/en/cli-reference): interactive launch, exact resume and session-scoped instruction flags. Current docs can describe versions later than the installed binary; help alone does not certify behavior.
- [OpenCode CLI](https://opencode.ai/docs/cli/) and [agents](https://opencode.ai/docs/agents/): session selection and documented role configuration. Actual adapter tests remain required.

No account/login changes or provider-home writes are part of these probes. Real task/resume testing needs dedicated test conversations and recorded versions; it is an open release gate, not silently inferred from local authentication.
