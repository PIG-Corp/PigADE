//! Process-level checks for exact-incarnation, idle-only host shutdown.
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{Command, Launch, PROTOCOL, Request, Response, receive, send};
use pigade_storage::{Journal, JournalHeader, MetadataStore};
use pigade_terminal::{CORE_PROFILE, Geometry};
use std::{
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(8);

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
        thread::sleep(Duration::from_millis(5));
    }
}

struct TestHost {
    root: TempDir,
    child: Child,
}
impl TestHost {
    fn new() -> Result<Self> {
        Self::in_root(tempfile::tempdir()?)
    }
    fn in_root(root: TempDir) -> Result<Self> {
        let child = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"))
            .arg("serve")
            .arg(root.path())
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let host = Self { root, child };
        wait_for(|| match exchange(&host.socket(), Command::HostStatus) {
            Ok(response) if response.host_status.is_some() => Ok(Some(())),
            Ok(_) | Err(_) => Ok(None),
        })?;
        Ok(host)
    }
    fn socket(&self) -> PathBuf {
        self.root.path().join("host.sock")
    }
    fn status(&self) -> Result<pigade_session_host::maintenance::Status> {
        exchange(&self.socket(), Command::HostStatus)?
            .host_status
            .context("host status missing")
    }
    fn wait_idle(&self) -> Result<pigade_session_host::maintenance::Status> {
        wait_for(|| {
            let status = self.status()?;
            Ok(status.can_shutdown.then_some(status))
        })
    }
    fn launch(&self, command: &str) -> Result<Launch> {
        let spec = Launch {
            session: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            operation: Uuid::new_v4(),
            executable: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), command.into()],
            env: Default::default(),
            cwd: self.root.path().canonicalize()?,
            geometry: Geometry::new(12, 60)?,
        };
        let response = exchange(&self.socket(), Command::Launch { spec: spec.clone() })?;
        if let Some(error) = response.error {
            bail!("launch failed: {error}");
        }
        Ok(spec)
    }
    fn shutdown(&self, incarnation: Uuid) -> Result<Response> {
        exchange(
            &self.socket(),
            Command::ShutdownIfIdle {
                expected_incarnation: incarnation,
            },
        )
    }
    fn stop_generation(&self, spec: &Launch) -> Result<()> {
        let response = exchange(
            &self.socket(),
            Command::Stop {
                session: spec.session,
                generation: spec.generation,
            },
        )?;
        ensure!(response.error.is_none(), "test-owned PTY did not stop");
        wait_for(|| {
            let status = self.status()?;
            Ok((status.active_sessions == 0 && status.unresolved_launches == 0).then_some(()))
        })
    }
    fn running(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }
    fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
            self.child.wait()?;
        }
        Ok(())
    }
}
impl Drop for TestHost {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[test]
fn idle_host_reports_status_and_exits_after_explicit_shutdown() -> Result<()> {
    let mut host = TestHost::new()?;
    let status = host.wait_idle()?;
    ensure!(status.schema == 1);
    ensure!(status.pid == host.child.id());
    ensure!(status.active_sessions == 0 && status.unresolved_launches == 0);
    ensure!(status.version == env!("CARGO_PKG_VERSION"));

    // The success acknowledgement can race process exit; either a response or
    // a disconnected client must be reconciled against the exact endpoint.
    let _ = host.shutdown(status.incarnation);
    wait_for(|| Ok(host.child.try_wait()?.is_some().then_some(())))?;
    ensure!(
        !host.socket().exists(),
        "orderly exit left a live endpoint path"
    );
    ensure!(!host.root.path().join("launch-count").exists());
    Ok(())
}

#[test]
fn active_pty_and_wrong_incarnation_both_refuse_shutdown() -> Result<()> {
    let mut host = TestHost::new()?;
    let incarnation = host.status()?.incarnation;
    let mismatched = host.shutdown(Uuid::new_v4())?;
    ensure!(mismatched.error.is_some());
    ensure!(mismatched.host_status.as_ref().is_some_and(|status| {
        status
            .blockers
            .iter()
            .any(|blocker| blocker.contains("incarnation"))
    }));
    ensure!(host.running()?);

    let spec = host.launch("printf ACTIVE_READY; exec sleep 30")?;
    let busy = host.shutdown(incarnation)?;
    ensure!(busy.error.is_some());
    ensure!(
        busy.host_status
            .as_ref()
            .is_some_and(|status| status.active_sessions == 1 && !status.can_shutdown)
    );
    ensure!(host.running()?);
    host.stop_generation(&spec)?;
    Ok(())
}

#[test]
fn unresolved_durable_launch_blocks_shutdown() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let session = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let operation = Uuid::new_v4();
    {
        let mut metadata = MetadataStore::open(directory.path().join("metadata.sqlite"))?;
        metadata.begin_launch(session, generation, operation, Uuid::new_v4())?;
    }
    let mut host = TestHost::in_root(directory)?;
    let socket = host.socket();
    let status = host.status()?;
    ensure!(status.unresolved_launches == 1);
    ensure!(!status.can_shutdown);
    let refusal = exchange(
        &socket,
        Command::ShutdownIfIdle {
            expected_incarnation: status.incarnation,
        },
    )?;
    ensure!(refusal.error.is_some());
    ensure!(host.running()?);
    Ok(())
}

#[test]
fn restored_exited_history_without_a_process_does_not_count_as_active() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let session = Uuid::new_v4();
    let generation = Uuid::new_v4();
    let operation = Uuid::new_v4();
    {
        let mut metadata = MetadataStore::open(directory.path().join("metadata.sqlite"))?;
        metadata.begin_launch(session, generation, operation, Uuid::new_v4())?;
        metadata.register_helper(
            operation,
            pigade_session_host::identity(std::process::id())?,
        )?;
        metadata.permit_exec(operation)?;
        metadata.mark_running(operation)?;
        metadata.mark_exited(operation)?;
    }
    drop(Journal::create(
        directory.path().join(format!("{generation}.journal")),
        JournalHeader {
            session_id: session,
            generation,
            profile: CORE_PROFILE.into(),
            initial_rows: 12,
            initial_cols: 60,
        },
    )?);
    let host = TestHost::in_root(directory)?;
    let status = host.wait_idle()?;
    ensure!(status.active_sessions == 0);
    ensure!(status.unresolved_launches == 0);
    ensure!(status.can_shutdown);
    Ok(())
}

#[test]
fn concurrent_launch_and_shutdown_never_launch_after_shutdown_barrier() -> Result<()> {
    let mut host = TestHost::new()?;
    let status = host.wait_idle()?;
    let marker = host.root.path().join("race-launch-count");
    let spec = Launch {
        session: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        operation: Uuid::new_v4(),
        executable: PathBuf::from("/bin/sh"),
        args: vec![
            "-c".into(),
            format!("printf x >> '{}'; exec sleep 30", marker.display()),
        ],
        env: Default::default(),
        cwd: host.root.path().canonicalize()?,
        geometry: Geometry::new(12, 60)?,
    };
    let barrier = Arc::new(Barrier::new(3));
    let launch_socket = host.socket();
    let launch_barrier = barrier.clone();
    let launch_spec = spec.clone();
    let launch = thread::spawn(move || {
        launch_barrier.wait();
        exchange(&launch_socket, Command::Launch { spec: launch_spec })
    });
    let shutdown_socket = host.socket();
    let shutdown_barrier = barrier.clone();
    let incarnation = status.incarnation;
    let shutdown = thread::spawn(move || {
        shutdown_barrier.wait();
        exchange(
            &shutdown_socket,
            Command::ShutdownIfIdle {
                expected_incarnation: incarnation,
            },
        )
    });
    barrier.wait();
    let launch_result = launch.join().expect("launch worker panicked");
    let shutdown_result = shutdown.join().expect("shutdown worker panicked");
    let marker_exists = marker.exists();
    let shutdown_acknowledged = shutdown_result
        .as_ref()
        .is_ok_and(|response| response.error.is_none());
    let exited = host.child.try_wait()?.is_some();
    if shutdown_acknowledged || (exited && !host.socket().exists()) {
        ensure!(
            !marker_exists,
            "launch ran after successful shutdown barrier"
        );
        if let Ok(response) = &launch_result {
            ensure!(
                response.error.is_some()
                    && response.rejected_before_admission
                        == Some(pigade_session_host::CommandRejection::HostShuttingDown),
                "connection surviving the shutdown barrier did not receive a known refusal"
            );
        }
        wait_for(|| Ok(host.child.try_wait()?.is_some().then_some(())))?;
    } else {
        ensure!(
            host.running()?,
            "refused shutdown unexpectedly exited the host"
        );
        if marker_exists
            || launch_result
                .as_ref()
                .is_ok_and(|response| response.error.is_none())
        {
            host.stop_generation(&spec)?;
        }
    }
    ensure!(fs::read(&marker).map_or(0, |bytes| bytes.len()) <= 1);
    Ok(())
}
