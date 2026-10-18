//! The standalone discovery command probes provider banners without host state.
use anyhow::{Context, Result, ensure};
use pigade_session_host::ProviderInfo;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn standalone_provider_detection_only_runs_bounded_version_probes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let bin = directory.path().join("bin");
    fs::create_dir(&bin)?;
    let invocations = directory.path().join("invocations");
    for (name, version) in [
        ("bash", "GNU bash, version 5.2.0"),
        ("claude", "Claude Code 2.1.259"),
        ("codex", "codex-cli 0.162.1"),
        ("opencode", "opencode 1.19.3"),
    ] {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = --version ]; then printf '%s\\n' '{}'; else printf 'TASK_LAUNCH\\n'; fi\n",
            invocations.display(),
            version
        );
        let path = bin.join(name);
        fs::write(&path, script)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let output = Command::new(env!("CARGO_BIN_EXE_pigade-session-host"))
        .arg("detect-providers")
        .env("PATH", &bin)
        .env("SHELL", bin.join("bash"))
        .output()?;
    ensure!(
        output.status.success(),
        "discovery command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let providers: Vec<ProviderInfo> = serde_json::from_slice(&output.stdout)?;
    ensure!(providers.len() == 4, "expected four providers");
    for name in ["shell", "claude", "codex", "opencode"] {
        let provider = providers
            .iter()
            .find(|provider| provider.provider == name)
            .with_context(|| format!("{name} was missing from discovery output"))?;
        ensure!(provider.available, "{name} should be available");
        ensure!(
            provider
                .executable
                .as_ref()
                .is_some_and(|path| path.starts_with(&bin))
        );
    }
    let calls = fs::read_to_string(&invocations)?;
    ensure!(
        calls.lines().count() == 3,
        "unexpected provider probe count: {calls:?}"
    );
    ensure!(
        calls.lines().all(|line| line == "--version"),
        "probe executed a task: {calls:?}"
    );
    ensure!(!directory.path().join("host.sock").exists());
    ensure!(!directory.path().join("state").exists());
    ensure!(
        !String::from_utf8_lossy(&output.stdout).contains("TASK_LAUNCH"),
        "task command output leaked into discovery response"
    );

    let extra_argument = Command::new(env!("CARGO_BIN_EXE_pigade-session-host"))
        .arg("detect-providers")
        .arg("unexpected")
        .output()?;
    ensure!(
        !extra_argument.status.success(),
        "unexpected argument was accepted"
    );
    Ok(())
}
