//! Regression for hosts that outlive replacement of their installed binary.
#![cfg(target_os = "linux")]
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{Command, Launch, PROTOCOL, Request, Response, receive, send};
use pigade_terminal::Geometry;
use std::{
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::tempdir;
use uuid::Uuid;

fn exchange(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    send(
        &mut stream,
        &Request {
            protocol: PROTOCOL,
            command,
        },
    )?;
    let response: Response = receive(&mut stream)?;
    ensure!(response.protocol == PROTOCOL);
    if let Some(error) = &response.error {
        bail!("host: {error}");
    }
    Ok(response)
}

fn wait_for(mut check: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !check()? {
        ensure!(Instant::now() < deadline, "bounded condition wait expired");
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

struct RunningHost(Child);
impl Drop for RunningHost {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn running_host_launches_helper_after_its_binary_is_unlinked() -> Result<()> {
    let root = tempdir()?;
    let binary_path = root.path().join("pigade-session-host");
    fs::copy(env!("CARGO_BIN_EXE_pigade-session-host"), &binary_path)?;
    let child = ProcessCommand::new(&binary_path)
        .arg("serve")
        .arg(root.path())
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut host = RunningHost(child);
    let socket = root.path().join("host.sock");
    wait_for(|| Ok(exchange(&socket, Command::List).is_ok()))?;
    let host_pid = host.0.id();

    fs::remove_file(&binary_path)?;
    ensure!(!binary_path.exists(), "test executable path still exists");

    let marker = root.path().join("launch-count");
    let spec = Launch {
        session: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        operation: Uuid::new_v4(),
        executable: PathBuf::from("/bin/sh"),
        args: vec![
            "-c".into(),
            format!(
                "printf x >> '{}'; printf SELF_EXEC_OK; exit 0",
                marker.display()
            ),
        ],
        env: Default::default(),
        cwd: root.path().to_path_buf(),
        geometry: Geometry::new(12, 60)?,
    };
    let first = exchange(&socket, Command::Launch { spec: spec.clone() })?;
    let launched = first
        .views
        .first()
        .context("launch returned no generation")?;
    let launched_pid = launched.pid.context("launched generation has no pid")?;
    wait_for(|| {
        let response = exchange(
            &socket,
            Command::Snapshot {
                session: spec.session,
                generation: spec.generation,
            },
        )?;
        let view = response
            .views
            .first()
            .context("snapshot returned no generation")?;
        Ok(view.frame.text().contains("SELF_EXEC_OK") && view.state != "running")
    })?;

    let duplicate = exchange(&socket, Command::Launch { spec: spec.clone() })?;
    ensure!(duplicate.views.first().and_then(|view| view.pid) == Some(launched_pid));
    let listed = exchange(&socket, Command::List)?;
    ensure!(
        listed.views.len() == 1,
        "launch produced duplicate generations"
    );
    ensure!(
        fs::read(&marker)?.len() == 1,
        "launch command executed more than once"
    );
    ensure!(
        host.0.try_wait()?.is_none(),
        "host exited during helper launch"
    );
    ensure!(host.0.id() == host_pid);
    Ok(())
}
