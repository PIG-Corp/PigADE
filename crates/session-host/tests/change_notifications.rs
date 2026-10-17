//! Process-level tests for durable host change notifications.
//!
//! The fixture uses a private host state directory and a real PTY child. It
//! sends only synthetic input and never interacts with the desktop.

use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{Command, Launch, PROTOCOL, Request, Response, View, receive, send};
use pigade_terminal::Geometry;
use std::{
    fs::{self, File},
    os::unix::{net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

const STARTUP_LIMIT: Duration = Duration::from_secs(8);

fn exchange(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
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
    ensure!(response.protocol == PROTOCOL, "host protocol mismatch");
    Ok(response)
}

fn checked(socket: &Path, command: Command) -> Result<Vec<View>> {
    let response = exchange(socket, command)?;
    if let Some(error) = response.error {
        bail!("host: {error}");
    }
    Ok(response.views)
}

struct TestHost {
    directory: TempDir,
    child: Option<Child>,
    active_generation: Option<(Uuid, Uuid)>,
}

impl TestHost {
    fn new() -> Result<Self> {
        Self::in_directory(tempfile::tempdir()?)
    }

    fn in_directory(directory: TempDir) -> Result<Self> {
        let mut host = Self {
            directory,
            child: None,
            active_generation: None,
        };
        host.start()?;
        Ok(host)
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn socket(&self) -> PathBuf {
        self.root().join("host.sock")
    }

    fn start(&mut self) -> Result<()> {
        let log = File::create(self.root().join("host.log"))?;
        self.child = Some(
            ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"))
                .arg("serve")
                .arg(self.root())
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()?,
        );
        let deadline = Instant::now() + STARTUP_LIMIT;
        loop {
            if let Some(status) = self
                .child
                .as_mut()
                .context("host process missing")?
                .try_wait()?
            {
                bail!(
                    "host exited {status}: {}",
                    fs::read_to_string(self.root().join("host.log"))?
                );
            }
            if checked(&self.socket(), Command::List).is_ok() {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "host socket did not become ready"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn stop_host(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            child.kill()?;
            child.wait()?;
        }
        Ok(())
    }

    fn launch(&mut self) -> Result<Launch> {
        let spec = Launch {
            session: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            operation: Uuid::new_v4(),
            executable: PathBuf::from("/usr/bin/python3"),
            args: vec!["-u".into(), "-c".into(), PTY_FIXTURE.into()],
            cwd: self.root().canonicalize()?,
            geometry: Geometry::new(12, 60)?,
        };
        let views = checked(&self.socket(), Command::Launch { spec: spec.clone() })?;
        ensure!(
            views.first().and_then(|view| view.pid).is_some(),
            "PTY PID missing"
        );
        self.active_generation = Some((spec.session, spec.generation));
        Ok(spec)
    }

    fn snapshot(&self, spec: &Launch) -> Result<View> {
        checked(
            &self.socket(),
            Command::Snapshot {
                session: spec.session,
                generation: spec.generation,
            },
        )?
        .into_iter()
        .next()
        .context("snapshot missing")
    }

    fn wait_for(&self, spec: &Launch, predicate: impl Fn(&View) -> bool) -> Result<View> {
        let deadline = Instant::now() + STARTUP_LIMIT;
        loop {
            let view = self.snapshot(spec)?;
            ensure!(
                view.storage_error.is_none(),
                "host degraded: {:?}",
                view.storage_error
            );
            if predicate(&view) {
                return Ok(view);
            }
            ensure!(Instant::now() < deadline, "bounded PTY condition expired");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn restart(&mut self) -> Result<()> {
        self.stop_host()?;
        self.start()
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        if let Some((session, generation)) = self.active_generation {
            // Ask the host to stop the exact fixture generation before tearing
            // down its directly-owned Child handle.
            let _ = exchange(
                &self.socket(),
                Command::Stop {
                    session,
                    generation,
                },
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let exited = checked(
                    &self.socket(),
                    Command::Snapshot {
                        session,
                        generation,
                    },
                )
                .ok()
                .and_then(|views| views.into_iter().next())
                .is_some_and(|view| view.state == "exited");
                if exited {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = self.stop_host();
    }
}

const PTY_FIXTURE: &str = concat!(
    "import os,signal,tty\n",
    "signal.alarm(8)\n",
    "tty.setraw(0)\n",
    "os.write(1,b'__CHANGE_READY__')\n",
    "data=bytearray()\n",
    "while not data.endswith(b'\\n'):\n",
    " data.extend(os.read(0,1))\n",
    "os.write(1,b'\\r\\n__CHANGE_ACK__'+bytes(data)+b'\\r\\n')\n",
);

#[test]
fn published_pty_output_wakes_wait_without_blocking_commands_and_survives_restart() -> Result<()> {
    ensure!(
        Path::new("/usr/bin/python3").is_file(),
        "Python fixture missing"
    );
    let mut host = TestHost::new()?;
    let spec = host.launch()?;
    let ready = host.wait_for(&spec, |view| view.frame.text().contains("__CHANGE_READY__"))?;

    let initial_poll = exchange(&host.socket(), Command::Poll { known: vec![] })?;
    ensure!(
        initial_poll.error.is_none(),
        "initial poll failed: {:?}",
        initial_poll.error
    );
    let token = initial_poll
        .change_token
        .context("Poll omitted change token")?;
    let selected = initial_poll
        .selected
        .context("Poll omitted selected metadata")?;
    let known = selected.iter().map(|info| info.known()).collect();
    ensure!(
        initial_poll
            .views
            .iter()
            .any(|view| view.frame.text().contains("__CHANGE_READY__")),
        "initial Poll did not return the ready PTY frame (snapshot seq {})",
        ready.sequence
    );

    // A separate IPC thread blocks in WaitForChange. An ordinary command is
    // still served while that waiter is outstanding; input then makes the
    // child publish a visible ACK through its PTY.
    let wait_socket = host.socket();
    let waiter = thread::spawn(move || {
        exchange(
            &wait_socket,
            Command::WaitForChange {
                after: token,
                timeout_ms: 900,
            },
        )
    });
    let independent = host.snapshot(&spec)?;
    ensure!(
        independent.frame.text().contains("__CHANGE_READY__"),
        "ordinary Snapshot failed while change wait was outstanding"
    );
    let input = exchange(
        &host.socket(),
        Command::Input {
            session: spec.session,
            generation: spec.generation,
            bytes: b"wake\n".to_vec(),
        },
    )?;
    ensure!(input.error.is_none(), "input failed: {:?}", input.error);

    let notification = waiter
        .join()
        .map_err(|_| anyhow::anyhow!("change waiter thread panicked"))??;
    ensure!(
        notification.error.is_none(),
        "wait failed: {:?}",
        notification.error
    );
    ensure!(
        notification.views.is_empty(),
        "wait unexpectedly carried a frame"
    );
    ensure!(
        notification.change_token.is_some_and(|next| next != token),
        "durable output publication did not advance the change token"
    );

    let changed = exchange(&host.socket(), Command::Poll { known })?;
    ensure!(
        changed.error.is_none(),
        "changed Poll failed: {:?}",
        changed.error
    );
    let ack = changed
        .views
        .iter()
        .find(|view| view.generation == spec.generation)
        .context("changed Poll did not return the PTY frame")?;
    ensure!(
        ack.sequence > ready.sequence,
        "published frame sequence did not advance"
    );
    ensure!(
        ack.frame.text().contains("__CHANGE_ACK__wake"),
        "ACK absent from frame"
    );
    let old_token = changed.change_token.context("changed Poll omitted token")?;

    // The fixture exits after its ACK. Restarting the host must recover the
    // historical model without respawning the PTY process and create a new
    // notification epoch so a prior-process token cannot be reused.
    host.wait_for(&spec, |view| view.state == "exited")?;
    host.restart()?;
    let recovered_list = checked(&host.socket(), Command::List)?;
    ensure!(
        recovered_list.len() == 1,
        "restart created or lost a generation"
    );
    let recovered = &recovered_list[0];
    ensure!(
        recovered.generation == spec.generation,
        "restart changed generation"
    );
    ensure!(
        recovered.pid.is_none(),
        "historical recovery respawned the PTY child"
    );
    ensure!(
        recovered.frame.text().contains("__CHANGE_ACK__wake"),
        "recovered ACK missing"
    );

    let restarted_poll = exchange(&host.socket(), Command::Poll { known: vec![] })?;
    let restarted_token = restarted_poll
        .change_token
        .context("restarted Poll omitted token")?;
    ensure!(
        restarted_token.host_instance != old_token.host_instance,
        "host restart reused its notification epoch"
    );
    let stale = exchange(
        &host.socket(),
        Command::WaitForChange {
            after: old_token,
            timeout_ms: 900,
        },
    )?;
    ensure!(
        stale.error.is_none(),
        "stale-token wait failed: {:?}",
        stale.error
    );
    ensure!(
        stale
            .change_token
            .is_some_and(|current| current.host_instance == restarted_token.host_instance),
        "wait did not return the restarted host epoch"
    );

    let timeout = exchange(
        &host.socket(),
        Command::WaitForChange {
            after: restarted_token,
            timeout_ms: 100,
        },
    )?;
    ensure!(
        timeout.error.is_none(),
        "timeout wait failed: {:?}",
        timeout.error
    );
    ensure!(
        timeout.views.is_empty(),
        "timeout wait unexpectedly carried views"
    );
    ensure!(
        timeout.change_token == Some(restarted_token),
        "idle timeout did not preserve the unchanged token"
    );
    Ok(())
}
