//! Read-only inspection and explicit idle shutdown for an existing host.
use anyhow::{Context, Result, bail, ensure};
use pigade_storage::ProcessIdentity;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    path::Path,
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};
use uuid::Uuid;

const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const IMAGE_COMPARE_TIMEOUT: Duration = Duration::from_secs(3);
const SHUTDOWN_RECONCILE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocketIdentity {
    pub dev: u64,
    pub ino: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageIdentity {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub modified_secs: i64,
    pub modified_nanos: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inspection {
    pub schema: u16,
    pub peer: ProcessIdentity,
    pub socket_identity: SocketIdentity,
    /// Snapshot of this client executable, used to reject stale confirmation after a rebuild.
    pub installed_image: ImageIdentity,
    pub same_binary: bool,
    pub legacy: bool,
    pub status: Option<crate::maintenance::Status>,
    pub active_sessions: Option<usize>,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownResult {
    pub stopped: bool,
    #[serde(default)]
    pub definitive_refusal: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PeerSnapshot {
    pid: u32,
    identity: ProcessIdentity,
}

pub fn inspect(socket: &Path) -> Result<Inspection> {
    let before = private_socket(socket)?;
    let installed_image = image_identity(&File::open("/proc/self/exe")?)?;
    let (mut stream, peer) = connect_peer(socket)?;
    ensure!(
        peer.identity.pid != std::process::id(),
        "host socket resolves to this maintenance client"
    );
    let response = request_on(&mut stream, crate::Command::HostStatus)?;

    let (status, active_sessions, legacy, mut blockers) =
        if response.error.as_deref() == Some("invalid request framing or payload") {
            let (mut stream, fallback_peer) = connect_peer(socket)?;
            ensure!(
                fallback_peer == peer,
                "host identity changed during legacy inspection"
            );
            let response = request_on(&mut stream, crate::Command::List)?;
            ensure!(response.error.is_none(), "legacy host list request failed");
            let mut blockers = vec![
                "This host uses a legacy protocol and does not support safe idle shutdown.".into(),
            ];
            let active_sessions = legacy_active_sessions(&response.views);
            if active_sessions.is_none() {
                blockers.push("Legacy host session states are unknown.".into());
            }
            (None, active_sessions, true, blockers)
        } else {
            ensure!(
                response.error.is_none(),
                "host status request failed: {}",
                response.error.as_deref().unwrap_or("unknown error")
            );
            let status = response
                .host_status
                .context("host did not return maintenance status")?;
            ensure!(
                status.schema == 1,
                "unsupported host status schema {}",
                status.schema
            );
            ensure!(
                status.pid == peer.pid,
                "host status PID does not match authenticated peer"
            );
            ensure!(
                status.version.len() <= 128,
                "host status version is too long"
            );
            ensure!(
                status.active_sessions <= 100_000,
                "host active session count is invalid"
            );
            ensure!(
                status.unresolved_launches <= 100_000,
                "host unresolved launch count is invalid"
            );
            ensure!(
                status.blockers.len() <= 64 && status.blockers.iter().all(|item| item.len() <= 256),
                "host status blockers exceed bounds"
            );
            ensure!(
                !status.can_shutdown
                    || (status.active_sessions == 0
                        && status.unresolved_launches == 0
                        && !status.git_busy
                        && !status.resume_busy
                        && status.blockers.is_empty()),
                "host status claims idle while reporting active work"
            );
            let blockers = status.blockers.clone();
            let active_sessions = status.active_sessions;
            (Some(status), Some(active_sessions), false, blockers)
        };
    drop(stream);

    ensure!(
        private_socket(socket)? == before,
        "host socket changed during inspection"
    );
    ensure!(
        crate::identity(peer.pid)? == peer.identity,
        "host process identity changed during inspection"
    );
    let same_binary = running_image_matches(peer.pid, &installed_image)
        .context("compare host image with the installed build")?;
    if !same_binary && !legacy {
        blockers.push("The running host executable differs from this installed build.".into());
    }
    Ok(Inspection {
        schema: 1,
        peer: peer.identity,
        socket_identity: before,
        installed_image,
        same_binary,
        legacy,
        status,
        active_sessions,
        blockers,
    })
}

pub fn shutdown(socket: &Path, expected: &Inspection) -> Result<ShutdownResult> {
    let preflight = (|| -> Result<(Uuid, UnixStream)> {
        let status = expected
            .status
            .as_ref()
            .context("inspection has no host status")?;
        let current_image = image_identity(&File::open("/proc/self/exe")?)?;
        validate_preview(
            expected,
            &current_image,
            &private_socket(socket)?,
            &crate::identity(expected.peer.pid)?,
        )?;

        let (stream, peer) = connect_peer(socket)?;
        ensure!(
            peer.identity == expected.peer,
            "host process changed before shutdown"
        );
        ensure!(
            private_socket(socket)? == expected.socket_identity,
            "host socket changed before shutdown"
        );
        Ok((status.incarnation, stream))
    })();
    let (incarnation, mut stream) = match preflight {
        Ok(prepared) => prepared,
        Err(error) => {
            return Ok(ShutdownResult {
                stopped: false,
                definitive_refusal: true,
                message: bounded_message(&format!("shutdown was not sent: {error:#}")),
            });
        }
    };

    // Exactly one request is sent. A lost acknowledgement is reconciled by observing
    // the original process identity, never by retrying or signaling a PID.
    let reply = request_on(
        &mut stream,
        crate::Command::ShutdownIfIdle {
            expected_incarnation: incarnation,
        },
    );
    drop(stream);
    if let Ok(response) = &reply
        && response.protocol == crate::PROTOCOL
        && let Some(error) = response.error.as_deref()
    {
        return Ok(ShutdownResult {
            stopped: false,
            definitive_refusal: true,
            message: bounded_message(error),
        });
    }

    let deadline = Instant::now() + SHUTDOWN_RECONCILE_TIMEOUT;
    loop {
        let result = reconcile(socket, expected)?;
        if result.stopped {
            return Ok(result);
        }
        if Instant::now() >= deadline {
            return Ok(result);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Read-only reconciliation for a prior shutdown attempt; never sends commands or signals.
pub fn reconcile(socket: &Path, expected: &Inspection) -> Result<ShutdownResult> {
    validate_inspection(expected)?;

    // Socket replacement is recorded as context only. Process identity is the proof:
    // another endpoint may have appeared while the inspected process was exiting.
    let socket_matches =
        private_socket(socket).is_ok_and(|current| current == expected.socket_identity);
    let message_suffix = if socket_matches {
        "The inspected host still owns its recorded socket."
    } else {
        "The recorded socket is absent or has changed."
    };
    match crate::identity(expected.peer.pid) {
        Ok(current) if current == expected.peer => Ok(ShutdownResult {
            stopped: false,
            definitive_refusal: false,
            message: format!("The inspected host process is still running. {message_suffix}"),
        }),
        Ok(_) => Ok(ShutdownResult {
            stopped: true,
            definitive_refusal: false,
            message: "The inspected host process identity has disappeared.".into(),
        }),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(ShutdownResult {
                stopped: true,
                definitive_refusal: false,
                message: "The inspected host process no longer exists.".into(),
            })
        }
        Err(_) => Ok(ShutdownResult {
            stopped: false,
            definitive_refusal: false,
            message: "The inspected host process state could not be confirmed.".into(),
        }),
    }
}

fn bounded_message(message: &str) -> String {
    let mut bounded = String::new();
    for ch in message.chars() {
        if bounded.len() + ch.len_utf8() > 512 {
            break;
        }
        bounded.push(ch);
    }
    bounded
}

fn validate_inspection(expected: &Inspection) -> Result<()> {
    ensure!(expected.schema == 1, "unsupported inspection schema");
    ensure!(
        expected.peer.pid > 0 && expected.peer.start_ticks > 0,
        "invalid host process identity"
    );
    ensure!(
        !expected.peer.boot_id.is_empty() && expected.peer.boot_id.len() <= 64,
        "invalid host boot identity"
    );
    if let Some(status) = &expected.status {
        ensure!(status.schema == 1, "unsupported host status schema");
        ensure!(
            status.pid == expected.peer.pid,
            "host status PID does not match inspected process"
        );
    }
    Ok(())
}

fn legacy_active_sessions(views: &[crate::View]) -> Option<usize> {
    count_active_states(views.iter().map(|view| view.state.as_str()))
}

fn count_active_states<'a>(mut states: impl Iterator<Item = &'a str>) -> Option<usize> {
    states.try_fold(0usize, |count, state| match state {
        "running" | "degraded" | "launch_outcome_unknown" => Some(count + 1),
        "exited" | "history" => Some(count),
        _ => None,
    })
}

fn validate_preview(
    expected: &Inspection,
    current_image: &ImageIdentity,
    current_socket: &SocketIdentity,
    current_peer: &ProcessIdentity,
) -> Result<()> {
    ensure!(expected.schema == 1, "unsupported inspection schema");
    ensure!(!expected.legacy, "legacy hosts cannot be shut down safely");
    let status = expected
        .status
        .as_ref()
        .context("inspection has no host status")?;
    ensure!(
        status.can_shutdown,
        "host is not idle: {}",
        status.blockers.join(", ")
    );
    ensure!(
        status.active_sessions == 0
            && status.unresolved_launches == 0
            && !status.git_busy
            && !status.resume_busy
            && status.blockers.is_empty(),
        "host status reports active work"
    );
    ensure!(
        current_image == &expected.installed_image,
        "maintenance client changed since inspection; inspect again"
    );
    ensure!(
        current_socket == &expected.socket_identity,
        "host socket changed since inspection"
    );
    ensure!(
        current_peer == &expected.peer,
        "host process changed since inspection"
    );
    Ok(())
}

fn private_socket(path: &Path) -> Result<SocketIdentity> {
    let metadata = fs::symlink_metadata(path).context("inspect host socket")?;
    ensure!(
        metadata.file_type().is_socket(),
        "host endpoint is not a Unix socket"
    );
    // SAFETY: geteuid has no preconditions.
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "host socket belongs to another user"
    );
    ensure!(
        metadata.mode() & 0o077 == 0,
        "host socket permissions are not private"
    );
    Ok(SocketIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

fn connect_peer(path: &Path) -> Result<(UnixStream, PeerSnapshot)> {
    let before = private_socket(path)?;
    let stream = UnixStream::connect(path).context("connect to host socket")?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let pid = crate::peer_pid(&stream)?;
    let identity = crate::identity(pid)?;
    ensure!(
        private_socket(path)? == before,
        "host socket changed while connecting"
    );
    ensure!(
        crate::identity(pid)? == identity,
        "host process changed while connecting"
    );
    Ok((stream, PeerSnapshot { pid, identity }))
}

fn request_on(stream: &mut UnixStream, command: crate::Command) -> Result<crate::Response> {
    crate::send(
        stream,
        &crate::Request {
            protocol: crate::PROTOCOL,
            command,
        },
    )?;
    crate::receive(stream)
}

fn image_identity(file: &File) -> Result<ImageIdentity> {
    let metadata = file.metadata()?;
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let duration = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
    Ok(ImageIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        len: metadata.len(),
        modified_secs: duration.as_secs() as i64,
        modified_nanos: duration.subsec_nanos() as i64,
    })
}

fn running_image_matches(pid: u32, installed: &ImageIdentity) -> Result<bool> {
    let running = File::open(format!("/proc/{pid}/exe"))?;
    let current = File::open("/proc/self/exe")?;
    compare_images(&running, &current, installed.len)
}

fn compare_images(left: &File, right: &File, expected_len: u64) -> Result<bool> {
    let left_meta = left.metadata()?;
    let right_meta = right.metadata()?;
    if left_meta.len() != expected_len || right_meta.len() != expected_len {
        return Ok(false);
    }
    if left_meta.dev() == right_meta.dev() && left_meta.ino() == right_meta.ino() {
        return Ok(true);
    }
    if expected_len > MAX_IMAGE_BYTES {
        bail!("executable image exceeds the bounded comparison size");
    }
    let deadline = Instant::now() + IMAGE_COMPARE_TIMEOUT;
    let mut left = left.try_clone()?;
    let mut right = right.try_clone()?;
    let mut left_buf = [0_u8; 32 * 1024];
    let mut right_buf = [0_u8; 32 * 1024];
    let mut remaining = expected_len;
    while remaining > 0 {
        if Instant::now() >= deadline {
            bail!("executable image comparison timed out");
        }
        let count = remaining.min(left_buf.len() as u64) as usize;
        left.read_exact(&mut left_buf[..count])?;
        right.read_exact(&mut right_buf[..count])?;
        if left_buf[..count] != right_buf[..count] {
            return Ok(false);
        }
        remaining -= count as u64;
    }
    // Detect trailing data if files changed while being compared.
    let mut extra = [0; 1];
    Ok(left.read(&mut extra)? == 0 && right.read(&mut extra)? == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_copied_host_images_by_bytes_and_detects_changes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let original = dir.path().join("host-a");
        let copy = dir.path().join("host-copy");
        let changed = dir.path().join("host-changed");
        fs::write(&original, b"same host image")?;
        fs::copy(&original, &copy)?;
        fs::write(&changed, b"other host image")?;
        let original = File::open(original)?;
        let copy = File::open(copy)?;
        let changed = File::open(changed)?;
        assert!(compare_images(&original, &copy, 15)?);
        assert!(!compare_images(&original, &changed, 15)?);
        Ok(())
    }

    #[test]
    fn refuses_to_compare_images_over_the_bound() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        File::create(&a)?.set_len(MAX_IMAGE_BYTES + 1)?;
        File::create(&b)?.set_len(MAX_IMAGE_BYTES + 1)?;
        assert!(compare_images(&File::open(a)?, &File::open(b)?, MAX_IMAGE_BYTES + 1).is_err());
        Ok(())
    }

    #[test]
    fn image_identity_captures_stable_file_snapshot() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("helper");
        fs::write(&path, b"helper")?;
        let file = File::open(path)?;
        let identity = image_identity(&file)?;
        assert_eq!(identity.len, 6);
        assert_ne!(identity.ino, 0);
        Ok(())
    }

    #[test]
    fn rejects_stale_process_identity_before_shutdown() -> Result<()> {
        let expected = Inspection {
            schema: 1,
            peer: ProcessIdentity {
                boot_id: "boot".into(),
                pid: 22,
                start_ticks: 100,
            },
            socket_identity: SocketIdentity { dev: 1, ino: 2 },
            installed_image: ImageIdentity {
                dev: 3,
                ino: 4,
                len: 5,
                modified_secs: 6,
                modified_nanos: 7,
            },
            same_binary: false,
            legacy: false,
            status: Some(crate::maintenance::Status {
                schema: 1,
                incarnation: Uuid::from_u128(1),
                version: "0.0.1-dev".into(),
                pid: 22,
                active_sessions: 0,
                unresolved_launches: 0,
                git_busy: false,
                resume_busy: false,
                can_shutdown: true,
                blockers: Vec::new(),
            }),
            active_sessions: None,
            blockers: Vec::new(),
        };
        let changed_peer = ProcessIdentity {
            start_ticks: 101,
            ..expected.peer.clone()
        };
        assert!(
            validate_preview(
                &expected,
                &expected.installed_image,
                &expected.socket_identity,
                &changed_peer,
            )
            .is_err()
        );
        let changed_image = ImageIdentity {
            ino: 8,
            ..expected.installed_image.clone()
        };
        assert!(
            validate_preview(
                &expected,
                &changed_image,
                &expected.socket_identity,
                &expected.peer,
            )
            .is_err()
        );
        let changed_socket = SocketIdentity {
            ino: 9,
            ..expected.socket_identity.clone()
        };
        assert!(
            validate_preview(
                &expected,
                &expected.installed_image,
                &changed_socket,
                &expected.peer,
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn legacy_session_count_ignores_exited_and_history_and_rejects_unknown_states() {
        assert_eq!(
            count_active_states(["running", "exited", "history", "degraded"].into_iter()),
            Some(2)
        );
        assert_eq!(
            count_active_states(["running", "future-state"].into_iter()),
            None
        );
    }

    #[test]
    fn reconcile_is_read_only_and_checks_exact_process_identity() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("socket");
        let peer = crate::identity(std::process::id())?;
        let helper = image_identity(&File::open("/proc/self/exe")?)?;
        let expected = Inspection {
            schema: 1,
            peer: peer.clone(),
            socket_identity: SocketIdentity { dev: 0, ino: 0 },
            installed_image: helper,
            same_binary: true,
            legacy: false,
            status: None,
            active_sessions: None,
            blockers: Vec::new(),
        };
        let still_running = reconcile(&socket, &expected)?;
        assert!(!still_running.stopped);

        let helper_rebuilt = Inspection {
            installed_image: ImageIdentity {
                len: expected.installed_image.len + 1,
                ..expected.installed_image.clone()
            },
            ..expected.clone()
        };
        let still_running_after_rebuild = reconcile(&socket, &helper_rebuilt)?;
        assert!(!still_running_after_rebuild.stopped);

        let replaced = Inspection {
            peer: ProcessIdentity {
                start_ticks: peer.start_ticks + 1,
                ..peer
            },
            ..helper_rebuilt
        };
        let old_identity_gone = reconcile(&socket, &replaced)?;
        assert!(old_identity_gone.stopped);
        Ok(())
    }
}
