//! Bounded local CLI metadata fallback for hosts without the discovery request.
use crate::protocol::ProviderInfo;
use std::{
    ffi::OsStr,
    io::{self, Read},
    os::fd::OwnedFd,
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const DEADLINE: Duration = Duration::from_secs(8);
const MAX_OUTPUT_BYTES: usize = 128 * 1024;
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Query the application's own session host without opening its server or
/// reading provider state. The host command performs its existing bounded
/// parallel version probes and emits the ProviderInfo DTO as JSON.
pub(crate) fn detect() -> Result<Vec<ProviderInfo>, String> {
    let binary = crate::startup::resolve_host_binary()?;
    detect_with_binary(&binary, DEADLINE)
}

fn detect_with_binary(binary: &Path, timeout: Duration) -> Result<Vec<ProviderInfo>, String> {
    run_json(binary, &[OsStr::new("detect-providers")], timeout)
}

/// Bounded one-shot commands on our installed host; never opens its server.
pub(crate) fn run_json<T: serde::de::DeserializeOwned>(
    binary: &Path,
    args: &[&OsStr],
    timeout: Duration,
) -> Result<T, String> {
    run_json_tracked(binary, args, timeout).map_err(|error| error.message)
}

pub(crate) struct HelperFailure {
    pub message: String,
    pub started: bool,
}

pub(crate) fn run_json_tracked<T: serde::de::DeserializeOwned>(
    binary: &Path,
    args: &[&OsStr],
    timeout: Duration,
) -> Result<T, HelperFailure> {
    let mut started = false;
    let result = (|| -> Result<T, String> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "local host helper deadline is out of range".to_owned())?;
        let (mut reader, writer) = UnixStream::pair()
            .map_err(|error| format!("create local host helper pipe: {error}"))?;
        let stderr_writer = writer
            .try_clone()
            .map_err(|error| format!("clone local host helper pipe: {error}"))?;

        let mut command = Command::new(binary);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(OwnedFd::from(writer)))
            .stderr(Stdio::from(OwnedFd::from(stderr_writer)));
        // Linux may retain a newly written executable through a concurrent
        // fork until that child execs. ETXTBSY proves exec never happened;
        // retry only that pre-start failure, within the original deadline.
        let busy_deadline = deadline.min(Instant::now() + Duration::from_millis(100));
        let process = loop {
            match command.spawn() {
                Ok(child) => break child,
                Err(error)
                    if cfg!(target_os = "linux")
                        && error.raw_os_error() == Some(26)
                        && Instant::now() < busy_deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(format!("start local host helper: {error}")),
            }
        };
        // Command owns the parent's pipe writers; close them before reading
        // so EOF continues to mean that the actual helper closed its output.
        drop(command);
        let mut child = ChildOnFailure(Some(process));

        started = true;
        let mut output = Vec::with_capacity(4096);
        let mut chunk = [0_u8; READ_CHUNK_BYTES];
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(deadline_error)?;
            reader
                .set_read_timeout(Some(remaining))
                .map_err(|error| format!("set local host helper deadline: {error}"))?;
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    if output.len().saturating_add(count) > MAX_OUTPUT_BYTES {
                        return Err("local host helper exceeded 128 KiB of output".into());
                    }
                    output.extend_from_slice(&chunk[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err(deadline_error());
                }
                Err(error) => return Err(format!("read local host helper output: {error}")),
            }
        }

        let status = wait_until_deadline(&mut child, deadline)?;
        if !status.success() {
            return Err(format!("local host helper exited with status {status}"));
        }
        serde_json::from_slice(&output)
            .map_err(|error| format!("decode local host helper output: {error}"))
    })();
    result.map_err(|message| HelperFailure { message, started })
}

fn wait_until_deadline(
    child: &mut ChildOnFailure,
    deadline: Instant,
) -> Result<ExitStatus, String> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(deadline_error)?;
        let process = child
            .0
            .as_mut()
            .expect("local host helper child is present until successful wait");
        if let Some(status) = process
            .try_wait()
            .map_err(|error| format!("wait for local host helper: {error}"))?
        {
            child.0.take();
            return Ok(status);
        }
        thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

fn deadline_error() -> String {
    "локальная команда host превысила временной лимит".into()
}

/// Kill and reap the child on every error path. Taking it is reserved for a
/// successful wait, after the caller has observed its final exit status.
struct ChildOnFailure(Option<Child>);

impl Drop for ChildOnFailure {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST_BINARY: AtomicU64 = AtomicU64::new(1);

    struct FakeBinary {
        directory: std::path::PathBuf,
        path: std::path::PathBuf,
    }

    impl FakeBinary {
        fn new(body: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "pigade-provider-discovery-{}-{}",
                std::process::id(),
                NEXT_TEST_BINARY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            let path = directory.join("fake-host");
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self { directory, path }
        }
    }

    impl Drop for FakeBinary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn helper_failure_distinguishes_not_started_from_lost_result() {
        let binary = FakeBinary::new("printf '%s' 'invalid-json'");
        let error =
            run_json_tracked::<serde_json::Value>(&binary.path, &[], Duration::from_secs(1))
                .err()
                .unwrap();
        assert!(error.started);
        let error = run_json_tracked::<serde_json::Value>(
            &binary.directory.join("absent"),
            &[],
            Duration::from_secs(1),
        )
        .err()
        .unwrap();
        assert!(!error.started);
    }

    #[test]
    fn invokes_only_the_discovery_subcommand_and_decodes_provider_info() {
        let binary = FakeBinary::new(
            "[ \"$#\" -eq 1 ] && [ \"$1\" = detect-providers ] || exit 21\nprintf '%s' '[{\"provider\":\"codex\",\"executable\":null,\"version\":null,\"available\":false,\"reason\":\"not installed\"}]'",
        );

        let result = detect_with_binary(&binary.path, Duration::from_secs(2)).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].provider, "codex");
        assert_eq!(result[0].reason.as_deref(), Some("not installed"));
    }

    #[test]
    fn kills_and_reaps_a_timed_out_discovery_child() {
        let binary = FakeBinary::new("exec sleep 5");
        let started = Instant::now();

        let error = detect_with_binary(&binary.path, Duration::from_millis(150)).unwrap_err();

        assert!(
            error.contains("временной лимит"),
            "unexpected helper error: {error}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn retries_busy_helper_only_before_exec_and_within_the_same_deadline() {
        let binary = FakeBinary::new("printf '%s' '[]'");
        let writer = fs::OpenOptions::new()
            .write(true)
            .open(&binary.path)
            .unwrap();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            drop(writer);
        });
        let result = detect_with_binary(&binary.path, Duration::from_secs(1));
        release.join().unwrap();
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn rejects_output_above_the_combined_limit() {
        let binary = FakeBinary::new("exec head -c 131073 /dev/zero");

        let error = detect_with_binary(&binary.path, Duration::from_secs(2)).unwrap_err();

        assert!(error.contains("exceeded 128 KiB"));
    }
}
