//! A non-reading PTY must not block the terminal actor's resize or Stop path.
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{
    Command, CommandRejection, Launch, PROTOCOL, Request, Response, View, identity, receive, send,
};
use pigade_storage::ProcessIdentity;
use pigade_terminal::Geometry;
use std::fs::{self, File};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(5);

fn exchange_response(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
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
fn exchange(socket: &Path, command: Command) -> Result<Vec<View>> {
    let response = exchange_response(socket, command)?;
    if let Some(error) = response.error {
        bail!("host: {error}");
    }
    Ok(response.views)
}

struct TestHost {
    directory: TempDir,
    child: Child,
    cli: Option<ProcessIdentity>,
}
impl TestHost {
    fn start() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let log = File::create(directory.path().join("host.log"))?;
        let child = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"))
            .arg("serve")
            .arg(directory.path())
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        let mut host = Self {
            directory,
            child,
            cli: None,
        };
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = host.child.try_wait()? {
                bail!(
                    "host exited {status}: {}",
                    fs::read_to_string(host.directory.path().join("host.log"))?
                );
            }
            if exchange(&host.socket(), Command::List).is_ok() {
                return Ok(host);
            }
            ensure!(Instant::now() < deadline, "host did not expose socket");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn socket(&self) -> PathBuf {
        self.directory.path().join("host.sock")
    }
    fn snapshot(&self, spec: &Launch) -> Result<View> {
        exchange(
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
        let deadline = Instant::now() + WAIT;
        loop {
            let view = self.snapshot(spec)?;
            ensure!(
                view.storage_error.is_none(),
                "actor degraded: {:?}",
                view.storage_error
            );
            if predicate(&view) {
                return Ok(view);
            }
            ensure!(
                Instant::now() < deadline,
                "terminal/control actor did not make progress"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for TestHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(expected) = &self.cli
            && identity(expected.pid).ok().as_ref() == Some(expected)
        {
            // SAFETY: the exact process identity was launched by this test.
            unsafe {
                let group = libc::getpgid(expected.pid as i32);
                if group == expected.pid as i32 {
                    libc::kill(-group, libc::SIGKILL);
                } else {
                    libc::kill(expected.pid as i32, libc::SIGKILL);
                }
            }
        }
    }
}

#[test]
fn unread_slave_input_does_not_block_resize_or_stop() -> Result<()> {
    ensure!(
        Path::new("/usr/bin/python3").is_file(),
        "Python 3 fixture dependency missing"
    );
    let mut host = TestHost::start()?;
    // Raw mode disables canonical input's overflow handling. This single process
    // never reads stdin and has no descendants holding the slave open after Stop.
    let spec = Launch {
        session: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        operation: Uuid::new_v4(),
        executable: PathBuf::from("/usr/bin/python3"),
        args: vec![
            "-u".into(),
            "-c".into(),
            "import os,tty,time; tty.setraw(0); os.write(1,b'__SLOW_READY__'); time.sleep(60)"
                .into(),
        ],
        cwd: host.directory.path().canonicalize()?,
        geometry: Geometry::new(12, 60)?,
    };
    let started = exchange(&host.socket(), Command::Launch { spec: spec.clone() })?;
    let pid = started
        .first()
        .and_then(|view| view.pid)
        .context("launched PID missing")?;
    host.cli = Some(identity(pid)?);
    host.wait_for(&spec, |view| view.frame.text().contains("__SLOW_READY__"))?;

    for _ in 0..4 {
        exchange(
            &host.socket(),
            Command::Input {
                session: spec.session,
                generation: spec.generation,
                bytes: vec![b'x'; 16 * 1024],
            },
        )?;
    }
    // Saturation must be reported to the caller without occupying the actor or
    // making the control channel depend on the input worker's progress.
    let mut full = false;
    for _ in 0..64 {
        let response = exchange_response(
            &host.socket(),
            Command::Input {
                session: spec.session,
                generation: spec.generation,
                bytes: vec![b'x'; 16 * 1024],
            },
        )?;
        match response.error {
            None => assert_eq!(response.rejected_before_admission, None),
            Some(error) if error.contains("queue full") => {
                assert_eq!(
                    response.rejected_before_admission,
                    Some(CommandRejection::QueueFull)
                );
                full = true;
                break;
            }
            Some(error) => bail!("host: {error}"),
        }
    }
    ensure!(
        full,
        "unread slave did not apply bounded input backpressure"
    );
    let geometry = Geometry::new(25, 81)?;
    exchange(
        &host.socket(),
        Command::Resize {
            session: spec.session,
            generation: spec.generation,
            geometry,
        },
    )?;
    let resized = host.wait_for(&spec, |view| view.frame.geometry == geometry)?;
    ensure!(resized.pid == Some(pid), "resize changed process identity");
    exchange(
        &host.socket(),
        Command::Stop {
            session: spec.session,
            generation: spec.generation,
        },
    )?;
    host.wait_for(&spec, |view| view.state == "exited")?;
    Ok(())
}
