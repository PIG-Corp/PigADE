# Provider PTY diagnostic — 2026-10-17

This report records the final root-supervised, offline PTY runs using the
provider harness source SHA-256
`5e576e8f7488c671973de3564c327d45fd9aecbf94c1dea8faf3f7099ad4cd50`. The
host source and frozen host copy both had SHA-256
`42bc8f71cd2cd8bbf0d98823bf4e48c1049213b90250ecfbf7a0e1e159ed785d`.
Curated machine-readable results are in
[`benchmarks/results/provider-pty-20261017/`](../../benchmarks/results/provider-pty-20261017/);
their hashes are listed in `SHA256SUMS`.

| Provider | Selected executable SHA-256 | Observed result |
| --- | --- | --- |
| Codex | `628e35487d888a28f0427fc37501da529dd3d81fcbad7a2c3421c1e8670d92e1` | Startup frame, resize to 30×100, resize back to 24×80, companion DSR probe, and one-second running observation passed. |
| OpenCode | `74f4edb9f28c8c0466fbd99aae459a46aaf338d4968b84d8c10d3e1b9136bc9f` | Startup frame, both resizes, companion DSR probe, and one-second running observation passed. |
| Claude Code | `f7dd62ae415378018cd21dd950eb3bac174ab085830304d3b8b098146bfd47b6` | Started and produced PTY output; both resizes completed. The final snapshot found that the session was no longer running. |

Claude was run with the standalone ELF override. Its normal PATH launcher was
the 110-byte shell wrapper at `/usr/bin/claude`, SHA-256
`356673de39ab5e9e4d19314c33b2d0b01fa06519a44c3374aa947bfbd71a4049`; that
wrapper targets an executable outside the read-only `/usr` tree. The override
mounted only the exact selected ELF as a read-only file. Both launcher and
selected executable hashes are recorded in the curated result without paths.

The Claude disposable-home terminal replay was inspected in memory with a
fixed phrase classifier; no terminal text, title, log contents, or provider
home values were copied to repository artifacts. The final screen matched the
classifier's network-connection failure category for the Anthropic API host.
The session journal reached sequence 8, replay showed a historical/exited
session with no host storage error, and the application log in the fresh home
was empty. This supports classifying the failure as provider API endpoint
connectivity during a deliberately network-isolated run. It does not establish
what Claude would do with network access, nor does it test login or task
execution. The private failed-run directory remains outside the repository.

All runs used a fresh disposable provider home, an empty workspace, Bubblewrap
network isolation, no display/session environment forwarding, no prompt, no
resume identifier, and zero provider input bytes. The companion query probe
verified one host DSR response path. The runs establish bounded PTY startup,
resize observations, and short-term process state for these exact binaries in
this environment. They do not establish semantic rendering correctness,
provider task/context receipt, authentication, resume, GUI/accessibility, IME,
or cross-platform behavior. Claude did not pass the running-observation check.

No isolation setting was relaxed and no provider was launched by the report
authoring step.
