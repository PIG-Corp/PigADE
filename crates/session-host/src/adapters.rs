use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

const VERSION_TIMEOUT: Duration = Duration::from_secs(1);
const VERSION_STREAM_LIMIT: usize = 8 * 1024;
const INLINE_CONTEXT_LIMIT: usize = 96 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AdapterInfo {
    pub(crate) provider: String,
    pub(crate) cli_version: String,
    pub(crate) adapter_version: u32,
    pub(crate) capabilities: Capabilities,
    pub(crate) delivery_method: String,
    pub(crate) unsupported_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    /// A documented transport exists for this exact CLI version; this does
    /// not assert that the model received or applied the context.
    pub(crate) startup_context: bool,
    pub(crate) context_receipt: bool,
    pub(crate) native_resume: bool,
    pub(crate) conversation_id: bool,
}

impl Capabilities {
    fn generic() -> Self {
        Self {
            startup_context: false,
            context_receipt: false,
            native_resume: false,
            conversation_id: false,
        }
    }
}

/// Ask only for the CLI's bounded `--version` banner. PigADE does not read
/// provider-owned files or start a task as part of adapter inspection.
pub(crate) fn inspect(provider: &str, executable: &Path) -> Result<AdapterInfo> {
    if provider == "shell" {
        return Ok(generic_info(
            provider,
            "shell".to_owned(),
            "Shell sessions do not accept startup context.",
        ));
    }

    let output = match run_version(executable) {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            return Ok(generic_info(
                provider,
                "unknown".into(),
                &format!(
                    "Version inspection exited with {}; startup context is disabled",
                    output.status
                ),
            ));
        }
        Err(error) => {
            return Ok(generic_info(
                provider,
                "unknown".into(),
                &format!("Version inspection failed: {error}; startup context is disabled"),
            ));
        }
    };
    let version_text = bounded_text(&output.stdout);
    let cli_version =
        first_version_token(&version_text).unwrap_or_else(|| version_text.trim().to_owned());

    if provider == "opencode"
        && let Some(key) = opencode_adapter_override()
    {
        return Ok(generic_info(
            provider,
            cli_version,
            &format!(
                "{key} is already set; PigADE leaves it untouched and disables advanced context/resume adapters."
            ),
        ));
    }

    let supported = supported_transport(provider, &cli_version);

    if let Some((method, description)) = supported {
        Ok(AdapterInfo {
            provider: provider.to_owned(),
            cli_version,
            adapter_version: if provider == "codex" { 2 } else { 1 },
            capabilities: Capabilities {
                startup_context: true,
                context_receipt: false,
                // Exact-ID TUI resume and missing-ID failure were executed
                // for 0.162.1. Automatic ID discovery remains unsupported.
                native_resume: provider == "codex",
                conversation_id: false,
            },
            delivery_method: method.to_owned(),
            unsupported_reason: Some(format!(
                "Startup context transport is available ({description}), but application is not acknowledged."
            )),
        })
    } else {
        let mut info = generic_info(
            provider,
            cli_version,
            "This provider/version has no certified PigADE context adapter; startup context is disabled.",
        );
        if info.provider == "opencode" && info.cli_version == "1.19.3" {
            // Exact-ID selection is independent from startup instructions.
            // OpenCode validates/fetches the supplied session before its TUI;
            // automatic discovery and context merging remain unsupported.
            info.adapter_version = 2;
            info.capabilities.native_resume = true;
        }
        Ok(info)
    }
}

fn supported_transport(provider: &str, version: &str) -> Option<(&'static str, &'static str)> {
    match (provider, version) {
        ("claude", "2.1.259") => Some((
            "--append-system-prompt",
            "Claude append-system-prompt argument",
        )),
        ("codex", "0.162.1") => Some(("-c", "Codex developer_instructions config override")),
        // Instruction merging has not been verified for a frozen OpenCode
        // binary. Generic PTY launch remains available without context.
        _ => None,
    }
}

fn opencode_adapter_override() -> Option<&'static str> {
    // Check presence only. Values can contain user instructions or routing
    // selectors; never read, copy, log or merge them into a launch plan.
    for (name, key) in [
        ("OPENCODE_CONFIG_CONTENT", c"OPENCODE_CONFIG_CONTENT"),
        ("OPENCODE_WORKSPACE_ID", c"OPENCODE_WORKSPACE_ID"),
    ] {
        #[cfg(unix)]
        let present = unsafe { !libc::getenv(key.as_ptr()).is_null() };
        #[cfg(not(unix))]
        let present = {
            let _ = key;
            std::env::var_os(name).is_some()
        };
        if present {
            return Some(name);
        }
    }
    None
}

fn generic_info(provider: &str, cli_version: String, reason: &str) -> AdapterInfo {
    AdapterInfo {
        provider: provider.to_owned(),
        cli_version,
        adapter_version: 1,
        capabilities: Capabilities::generic(),
        delivery_method: "none".to_owned(),
        unsupported_reason: Some(reason.to_owned()),
    }
}

struct VersionOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
}

fn run_version(executable: &Path) -> Result<VersionOutput> {
    #[cfg(unix)]
    {
        run_version_unix(executable, VERSION_TIMEOUT)
    }
    #[cfg(not(unix))]
    {
        let _ = executable;
        bail!("bounded CLI version inspection is only implemented on Unix")
    }
}

#[cfg(unix)]
fn run_version_unix(executable: &Path, timeout: Duration) -> Result<VersionOutput> {
    use std::os::fd::AsRawFd;

    let deadline = Instant::now() + timeout;
    let mut child = Command::new(executable)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not start {} --version", executable.display()))?;
    let mut stdout = Some(child.stdout.take().context("version stdout unavailable")?);
    let mut stderr = Some(child.stderr.take().context("version stderr unavailable")?);
    if let Err(error) = set_nonblocking(stdout.as_ref().expect("stdout taken").as_raw_fd())
        .and_then(|()| set_nonblocking(stderr.as_ref().expect("stderr taken").as_raw_fd()))
    {
        stop_probe(&mut child);
        bail!("could not configure bounded version output pipes: {error}");
    }

    let mut stdout_bytes = Vec::with_capacity(VERSION_STREAM_LIMIT);
    let mut stderr_bytes = Vec::with_capacity(VERSION_STREAM_LIMIT);
    loop {
        if Instant::now() >= deadline {
            stop_probe(&mut child);
            bail!(
                "{} --version exceeded the {:?} probe limit",
                executable.display(),
                timeout
            );
        }
        if let Err(error) = drain_nonblocking(&mut stdout, &mut stdout_bytes)
            .and_then(|()| drain_nonblocking(&mut stderr, &mut stderr_bytes))
        {
            stop_probe(&mut child);
            return Err(error).context("reading bounded CLI version output");
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // Drain bytes already queued at exit, but never wait for EOF:
                // a descendant may have inherited either pipe descriptor.
                if let Err(error) = drain_nonblocking(&mut stdout, &mut stdout_bytes)
                    .and_then(|()| drain_nonblocking(&mut stderr, &mut stderr_bytes))
                {
                    return Err(error).context("reading bounded CLI version output");
                }
                return Ok(VersionOutput {
                    status,
                    stdout: stdout_bytes,
                });
            }
            Ok(None) => {}
            Err(error) => {
                stop_probe(&mut child);
                return Err(error).context("waiting for CLI version probe");
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

#[cfg(unix)]
fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: `fd` is a live child pipe descriptor owned by this function.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor remains live and this only adds O_NONBLOCK.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn drain_nonblocking(reader: &mut Option<impl Read>, destination: &mut Vec<u8>) -> Result<()> {
    let Some(reader) = reader else { return Ok(()) };
    let mut buffer = [0_u8; 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                ensure!(
                    destination.len().saturating_add(count) <= VERSION_STREAM_LIMIT,
                    "version output exceeds 8 KiB per stream"
                );
                destination.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(unix)]
fn stop_probe(child: &mut std::process::Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn bounded_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn first_version_token(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| token.as_bytes().first().is_some_and(u8::is_ascii_digit))
        .map(|token| {
            token
                .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
                .to_owned()
        })
}

/// Build the exact startup transport for a previously inspected adapter.
/// Unsupported or forged adapter metadata fails closed.
pub(crate) fn context_arguments(
    info: &AdapterInfo,
    text: &str,
) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    ensure!(
        text.len() <= INLINE_CONTEXT_LIMIT,
        "startup context exceeds the 96 KiB inline limit"
    );
    let env = BTreeMap::new();
    let args = match (info.provider.as_str(), info.cli_version.as_str()) {
        ("claude", "2.1.259") if info.capabilities.startup_context => {
            vec!["--append-system-prompt".to_owned(), text.to_owned()]
        }
        ("codex", "0.162.1") if info.capabilities.startup_context => {
            let value =
                serde_json::to_string(text).context("encoding Codex developer instructions")?;
            let override_value = format!("developer_instructions={value}");
            ensure!(
                override_value.len() <= INLINE_CONTEXT_LIMIT,
                "Codex startup context transport exceeds the 96 KiB argument limit"
            );
            vec!["-c".to_owned(), override_value]
        }
        _ => bail!(
            "startup context is unsupported for {} {}",
            info.provider,
            info.cli_version
        ),
    };
    Ok((args, env))
}

/// Build a version-pinned exact-ID resume invocation. It never uses a latest
/// selector, forks the conversation, or sends a new task as positional text.
/// Context transports remain independent capabilities.
pub(crate) fn resume_arguments(
    info: &AdapterInfo,
    conversation_id: &str,
    cwd: &Path,
    context: Option<&str>,
) -> Result<(Vec<String>, BTreeMap<String, String>)> {
    ensure!(cwd.is_absolute(), "resume cwd must be absolute");
    let cwd_text = cwd.to_str().context("resume cwd must be valid UTF-8")?;
    ensure!(!cwd_text.contains('\0'), "resume cwd must not contain NUL");
    if info.provider == "opencode" && info.cli_version == "1.19.3" {
        ensure!(
            is_opencode_session_id(conversation_id),
            "invalid OpenCode session ID"
        );
        ensure!(
            context.is_none(),
            "OpenCode resume does not support PigADE context injection"
        );
        return Ok((
            vec!["--session".into(), conversation_id.into()],
            BTreeMap::new(),
        ));
    }
    ensure!(
        info.provider == "codex" && info.cli_version == "0.162.1",
        "native resume is unsupported for {} {}",
        info.provider,
        info.cli_version
    );
    ensure!(
        is_canonical_nonnil_uuid(conversation_id),
        "invalid conversation UUID"
    );

    let mut args = vec![
        "resume".to_owned(),
        "--no-daemon".to_owned(),
        "--cd".to_owned(),
        cwd_text.to_owned(),
    ];
    let mut env = BTreeMap::new();
    if let Some(context) = context {
        let (context_args, context_env) = context_arguments(info, context)?;
        args.extend(context_args);
        env = context_env;
    }
    args.push(conversation_id.to_owned());
    Ok((args, env))
}

pub(crate) fn is_opencode_session_id(value: &str) -> bool {
    value.starts_with("ses_")
        && value.len() > 4
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_canonical_nonnil_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || ![8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes[index] == b'-')
    {
        return false;
    }
    let mut nonzero = false;
    for (index, byte) in bytes.iter().enumerate() {
        if [8, 13, 18, 23].contains(&index) {
            continue;
        }
        if !byte.is_ascii_hexdigit() {
            return false;
        }
        nonzero |= *byte != b'0';
    }
    nonzero
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(provider: &str, version: &str) -> AdapterInfo {
        AdapterInfo {
            provider: provider.to_owned(),
            cli_version: version.to_owned(),
            adapter_version: 1,
            capabilities: Capabilities {
                startup_context: true,
                context_receipt: false,
                native_resume: false,
                conversation_id: false,
            },
            delivery_method: "test".to_owned(),
            unsupported_reason: None,
        }
    }

    #[test]
    fn codex_context_is_json_escaped_and_capabilities_are_unconfirmed() {
        let adapter = info("codex", "0.162.1");
        let (args, env) = context_arguments(&adapter, "line1\n\"quoted\"").unwrap();
        assert!(env.is_empty());
        assert_eq!(args[0], "-c");
        assert_eq!(
            args[1],
            r####"developer_instructions="line1\n\"quoted\"""####
        );
        assert!(!adapter.capabilities.context_receipt);
        assert!(!adapter.capabilities.native_resume);
        assert!(!adapter.capabilities.conversation_id);
    }

    #[test]
    fn opencode_resume_uses_only_exact_id_and_rejects_context_or_unverified_versions() {
        let adapter = info("opencode", "1.19.3");
        let cwd = Path::new("/workspace");
        let id = "ses_0123456789abcdefABCDEF";
        let (args, env) = resume_arguments(&adapter, id, cwd, None).unwrap();
        assert_eq!(args, vec!["--session", id]);
        assert!(env.is_empty());
        assert!(resume_arguments(&adapter, id, cwd, Some("must not send a new task")).is_err());
        assert!(resume_arguments(&info("opencode", "1.19.2"), id, cwd, None).is_err());
        for invalid in [
            "ses_",
            "latest",
            "ses_bad id",
            "ses_\ncontrol",
            "--continue",
        ] {
            assert!(resume_arguments(&adapter, invalid, cwd, None).is_err());
        }
        assert!(
            resume_arguments(&adapter, &format!("ses_{}", "a".repeat(253)), cwd, None).is_err()
        );
    }

    #[test]
    fn codex_resume_selects_exact_id_directory_and_context_without_prompt() {
        let adapter = info("codex", "0.162.1");
        let cwd = std::env::current_dir().unwrap();
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        let (args, env) = resume_arguments(&adapter, id, &cwd, Some("line1\n\"quoted\"")).unwrap();
        assert!(env.is_empty());
        assert_eq!(
            args,
            vec![
                "resume",
                "--no-daemon",
                "--cd",
                cwd.to_string_lossy().as_ref(),
                "-c",
                r####"developer_instructions="line1\n\"quoted\"""####,
                id,
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--last"));
    }

    #[test]
    fn codex_resume_validates_saved_metadata_and_path_representation_only() {
        let cwd = std::env::current_dir().unwrap();
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        assert!(resume_arguments(&info("claude", "2.1.259"), id, &cwd, None).is_err());
        assert!(resume_arguments(&info("codex", "0.162.2"), id, &cwd, None).is_err());
        for invalid in [
            "abc",
            "00000000-0000-0000-0000-000000000000",
            "01234567-89ab-cdef-0123-456789abcdeZ",
        ] {
            assert!(resume_arguments(&info("codex", "0.162.1"), invalid, &cwd, None).is_err());
        }
        assert!(
            resume_arguments(&info("codex", "0.162.1"), id, Path::new("relative"), None).is_err()
        );
        let missing = Path::new("/definitely/missing/pigade");
        let (args, _) = resume_arguments(&info("codex", "0.162.1"), id, missing, None).unwrap();
        assert_eq!(args[3], missing.to_str().unwrap());
        assert!(
            resume_arguments(
                &info("codex", "0.162.1"),
                id,
                Path::new("/tmp/with\0nul"),
                None
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let non_utf8 = std::ffi::OsString::from_vec(b"/tmp/invalid-\xff".to_vec());
            assert!(
                resume_arguments(&info("codex", "0.162.1"), id, Path::new(&non_utf8), None)
                    .is_err()
            );
        }
    }

    #[test]
    fn opencode_context_stays_disabled_until_versioned_transport_is_verified() {
        assert!(supported_transport("opencode", "1.18.22").is_none());
        assert!(supported_transport("opencode", "1.19.3").is_none());
        assert!(context_arguments(&info("opencode", "1.18.22"), "context").is_err());
    }

    #[test]
    fn unknown_or_generic_versions_cannot_receive_context() {
        let mut unknown = info("claude", "2.1.260");
        assert!(context_arguments(&unknown, "text").is_err());
        unknown.capabilities.startup_context = false;
        assert!(context_arguments(&unknown, "text").is_err());
        assert!(
            context_arguments(
                &info("codex", "0.162.1"),
                &"x".repeat(INLINE_CONTEXT_LIMIT + 1)
            )
            .is_err()
        );
        assert!(
            context_arguments(
                &info("codex", "0.162.1"),
                &"\0".repeat(INLINE_CONTEXT_LIMIT / 6)
            )
            .is_err()
        );
    }

    #[test]
    fn version_output_reader_enforces_per_stream_cap() {
        let mut within_limit = Some(io::Cursor::new(vec![b'x'; VERSION_STREAM_LIMIT]));
        let mut captured = Vec::with_capacity(VERSION_STREAM_LIMIT);
        drain_nonblocking(&mut within_limit, &mut captured).unwrap();
        assert_eq!(captured.len(), VERSION_STREAM_LIMIT);

        let mut over_limit = Some(io::Cursor::new(vec![b'x'; VERSION_STREAM_LIMIT + 1]));
        let mut captured = Vec::with_capacity(VERSION_STREAM_LIMIT);
        assert!(drain_nonblocking(&mut over_limit, &mut captured).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn opencode_user_config_or_workspace_override_disables_advanced_adapter_without_reading_values()
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(key) = std::env::var_os("PIGADE_ADAPTER_ENV_PROBE") {
            let root = tempfile::tempdir().unwrap();
            let executable = root.path().join("opencode");
            std::fs::write(&executable, b"#!/bin/sh\nprintf 'opencode 1.19.3\\n'\n").unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let info = inspect("opencode", &executable).unwrap();
            assert_eq!(info.cli_version, "1.19.3");
            assert!(!info.capabilities.native_resume && !info.capabilities.startup_context);
            let reason = info.unsupported_reason.unwrap();
            assert!(reason.contains(key.to_str().unwrap()));
            assert!(!reason.contains("SYNTHETIC_PRIVATE_VALUE"));
            return;
        }
        for key in ["OPENCODE_CONFIG_CONTENT", "OPENCODE_WORKSPACE_ID"] {
            let status=Command::new(std::env::current_exe().unwrap())
                .args(["--exact","adapters::tests::opencode_user_config_or_workspace_override_disables_advanced_adapter_without_reading_values"])
                .env_remove("OPENCODE_CONFIG_CONTENT").env_remove("OPENCODE_WORKSPACE_ID")
                .env(key,"SYNTHETIC_PRIVATE_VALUE").env("PIGADE_ADAPTER_ENV_PROBE",key)
                .stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
            assert!(status.success(), "override guard failed for {key}");
        }
    }

    #[test]
    fn only_the_exact_installed_versions_get_startup_transport() {
        assert!(supported_transport("claude", "2.1.259").is_some());
        assert!(supported_transport("codex", "0.162.1").is_some());
        assert!(supported_transport("opencode", "1.18.22").is_none());
        assert!(supported_transport("claude", "2.1.260").is_none());
        assert!(supported_transport("shell", "5.3.3").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_kills_a_child_at_its_deadline() {
        use std::os::unix::fs::PermissionsExt;

        let script = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(script.path(), b"#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
        std::fs::set_permissions(script.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        // Close the writable descriptor before exec; otherwise Linux rejects
        // the script with ETXTBSY and never exercises the timeout boundary.
        let script = script.into_temp_path();
        let start = Instant::now();
        let error = run_version_unix(&script, Duration::from_millis(40))
            .err()
            .expect("probe must time out");
        assert!(error.to_string().contains("probe limit"));
        assert!(start.elapsed() >= Duration::from_millis(40));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
