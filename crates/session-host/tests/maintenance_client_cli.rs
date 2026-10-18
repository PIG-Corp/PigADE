//! CLI-level maintenance checks against private hosts owned by this test process.
use anyhow::{Context, Result, ensure};
use pigade_session_host::maintenance_client::{Inspection, ShutdownResult};
use pigade_session_host::{Command, Launch, PROTOCOL, Request, Response, receive, send};
use pigade_terminal::Geometry;
use std::{
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(8);

struct Host {
    root: TempDir,
    child: Child,
    active: Vec<Launch>,
}

impl Host {
    fn start() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let child = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"))
            .arg("serve")
            .arg(root.path())
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let host = Self {
            root,
            child,
            active: Vec::new(),
        };
        wait_for(|| match exchange(&host.socket(), Command::HostStatus) {
            Ok(response) if response.host_status.is_some() => Ok(Some(())),
            Ok(_) | Err(_) => Ok(None),
        })?;
        Ok(host)
    }

    fn socket(&self) -> PathBuf {
        self.root.path().join("host.sock")
    }

    fn inspect(&self) -> Result<Inspection> {
        let output = cli(&["host-inspect", path_arg(&self.socket())])?;
        ensure!(
            output.status.success(),
            "host-inspect failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn launch(&mut self) -> Result<Launch> {
        let spec = Launch {
            session: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            operation: Uuid::new_v4(),
            executable: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "printf READY; exec sleep 30".into()],
            env: Default::default(),
            cwd: self.root.path().canonicalize()?,
            geometry: Geometry::new(12, 60)?,
        };
        let response = exchange(&self.socket(), Command::Launch { spec: spec.clone() })?;
        ensure!(response.error.is_none(), "test-owned PTY launch failed");
        self.active.push(spec.clone());
        Ok(spec)
    }

    fn stop_session(&mut self, spec: &Launch) -> Result<()> {
        let response = exchange(
            &self.socket(),
            Command::Stop {
                session: spec.session,
                generation: spec.generation,
            },
        )?;
        ensure!(response.error.is_none(), "test-owned PTY stop failed");
        self.active.retain(|item| item.session != spec.session);
        wait_for(|| {
            let response = exchange(&self.socket(), Command::HostStatus)?;
            let status = response.host_status.context("host status missing")?;
            Ok((status.active_sessions == 0).then_some(()))
        })
    }

    fn running(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }

    fn shutdown(&mut self, expected: &Inspection) -> Result<Output> {
        let json = serde_json::to_string(expected)?;
        let mut cli = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"))
            .arg("host-shutdown-idle")
            .arg(self.socket())
            .arg(json)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + WAIT;
        loop {
            // Reap this test-owned host as soon as it exits so the CLI observes
            // actual process disappearance instead of a still-visible zombie.
            let _ = self.child.try_wait()?;
            if cli.try_wait()?.is_some() {
                return Ok(cli.wait_with_output()?);
            }
            ensure!(Instant::now() < deadline, "maintenance CLI timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        for launch in std::mem::take(&mut self.active) {
            let _ = exchange(
                &self.socket(),
                Command::Stop {
                    session: launch.session,
                    generation: launch.generation,
                },
            );
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn cli(args: &[&str]) -> Result<Output> {
    Ok(
        ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?,
    )
}

fn exchange(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    send(
        &mut stream,
        &Request {
            protocol: PROTOCOL,
            command,
        },
    )?;
    let response: Response = receive(&mut stream)?;
    ensure!(response.protocol == PROTOCOL, "protocol mismatch");
    Ok(response)
}

fn wait_for<T>(mut check: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = check()? {
            return Ok(value);
        }
        ensure!(Instant::now() < deadline, "bounded condition wait expired");
        thread::sleep(Duration::from_millis(10));
    }
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("test temp path is UTF-8")
}

#[test]
fn idle_inspection_shutdown_and_reconcile_use_exact_private_host() -> Result<()> {
    let mut host = Host::start()?;
    let metadata = std::fs::symlink_metadata(host.socket())?;
    ensure!(metadata.file_type().is_socket());
    ensure!(metadata.uid() == unsafe { libc::geteuid() });
    ensure!(metadata.permissions().mode() & 0o077 == 0);

    let inspection = host.inspect()?;
    ensure!(inspection.peer.pid == host.child.id());
    ensure!(
        inspection.same_binary,
        "the current CLI and host should be the same executable image"
    );
    ensure!(
        inspection
            .status
            .as_ref()
            .is_some_and(|s| s.can_shutdown && s.active_sessions == 0)
    );
    ensure!(inspection.socket_identity.ino == metadata.ino());
    ensure!(inspection.installed_image.ino != 0 && inspection.installed_image.len > 0);

    let output = host.shutdown(&inspection)?;
    ensure!(
        output.status.success(),
        "idle shutdown CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: ShutdownResult = serde_json::from_slice(&output.stdout)?;
    ensure!(
        result.stopped && !result.definitive_refusal,
        "idle host did not stop: {}",
        result.message
    );
    wait_for(|| Ok(host.child.try_wait()?.is_some().then_some(())))?;

    let expected = serde_json::to_string(&inspection)?;
    let output = cli(&["host-reconcile", path_arg(&host.socket()), &expected])?;
    ensure!(
        output.status.success(),
        "host-reconcile CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: ShutdownResult = serde_json::from_slice(&output.stdout)?;
    ensure!(result.stopped && !result.definitive_refusal);
    Ok(())
}

#[test]
fn stale_image_socket_and_peer_previews_are_refused_without_shutdown() -> Result<()> {
    let mut host = Host::start()?;
    let inspection = host.inspect()?;

    let mut stale_image = inspection.clone();
    stale_image.installed_image.len += 1;
    let mut stale_socket = inspection.clone();
    stale_socket.socket_identity.ino += 1;
    let mut stale_peer = inspection.clone();
    stale_peer.peer.start_ticks += 1;

    for stale in [&stale_image, &stale_socket, &stale_peer] {
        let output = host.shutdown(stale)?;
        ensure!(
            output.status.success(),
            "pre-send refusal was not returned as JSON"
        );
        let result: ShutdownResult = serde_json::from_slice(&output.stdout)?;
        ensure!(!result.stopped && result.definitive_refusal);
        ensure!(host.running()?);
    }
    Ok(())
}

#[test]
fn active_pty_returns_definitive_refusal_and_remains_running() -> Result<()> {
    let mut host = Host::start()?;
    let idle_preview = host.inspect()?;
    let spec = host.launch()?;
    let inspection = host.inspect()?;
    ensure!(
        inspection
            .status
            .as_ref()
            .is_some_and(|s| s.active_sessions == 1 && !s.can_shutdown)
    );

    // Simulate a PTY becoming active after the user inspected an idle preview.
    // The host must reject the one stale request and report a definitive refusal.
    let output = host.shutdown(&idle_preview)?;
    ensure!(
        output.status.success(),
        "refusal is returned as a JSON result"
    );
    let result: ShutdownResult = serde_json::from_slice(&output.stdout)?;
    ensure!(!result.stopped && result.definitive_refusal);
    ensure!(host.running()?);
    host.stop_session(&spec)?;
    Ok(())
}
