# Historical comparison — retired by user request

The initial SPEC required an Electron/xterm comparison. It was implemented in commit6b9ed5c, then the user explicitly removed control stands from scope on2026-10-17. Runnable comparison applications and their dependencies are removed; Git history preserves the experiment.

Observed stock serialize+tail failures included partial UTF-8/CSI/OSC, saved cursor, tabstops, scroll region and alternate-screen attributes. Full replay matched the compared public states. These findings motivated the maintained Rust core checkpoint extension. They do not approve an Electron runtime, a production fallback, or relax terminal/recovery/performance requirements. No further comparative app is maintained.
