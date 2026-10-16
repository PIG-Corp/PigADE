# Versions

Current product version: **0.0.1-dev** (human form: dev 0.0.1). This is an experimental development version, not a released MVP/beta.

VERSION is the canonical first-party product version. Keep Cargo workspace package version, native application package version, startup diagnostics and CHANGELOG aligned. Do not change third-party package versions in vendor directories to imitate the product version: record upstream pins and downstream patch profiles separately. IPC protocol, journal, terminal checkpoint schema and context revision are independent compatibility identities; bump their schemas deliberately when incompatible bytes/semantics change.

Development work remains 0.0.1-dev until the next explicitly recorded milestone/version decision. A commit is not a release. A release requires an updated CHANGELOG, exact verification evidence and known limitations. Never label beta/MVP while SPEC release blockers remain. Future stable versions use SemVer `MAJOR.MINOR.PATCH`; pre-release builds use an explicit suffix. Do not retag or rewrite published releases.

Local enforcement: `git config core.hooksPath .githooks`. The tracked pre-commit hook checks version alignment, a new staged WORKLOG entry and whitespace errors. On a new clone, set this config before the first implementation commit; hooks are not automatically enabled by Git cloning. Agent-specific accountability remains mandatory in AGENTS.md even when the lead makes the integration commit.
