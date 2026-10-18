//! Real-host regression coverage for generation-owned PTY process-group Stop.
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{
    Command, Launch, PROTOCOL, Request, Response, View, identity, receive, send,
};
use pigade_storage::{EventKind, JournalReader, ProcessIdentity};
use pigade_terminal::Geometry;
use std::{
    fs::{self, File},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(10);

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
        bail!("host command failed: {error}");
    }
    Ok(response.views)
}

struct TestHost {
    directory: TempDir,
    child: Option<Child>,
    owned: Vec<ProcessIdentity>,
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
            child: Some(child),
            owned: Vec::new(),
        };
        host.wait_ready()?;
        Ok(host)
    }

    fn wait_ready(&mut self) -> Result<()> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self
                .child
                .as_mut()
                .context("host child missing")?
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

    fn root(&self) -> &Path {
        self.directory.path()
    }
    fn socket(&self) -> PathBuf {
        self.root().join("host.sock")
    }

    fn launch(&mut self, executable: &str, args: Vec<String>) -> Result<Launch> {
        let spec = Launch {
            session: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            operation: Uuid::new_v4(),
            executable: PathBuf::from(executable),
            args,
            env: Default::default(),
            cwd: self.root().canonicalize()?,
            geometry: Geometry::new(12, 60)?,
        };
        let view = checked(&self.socket(), Command::Launch { spec: spec.clone() })?
            .into_iter()
            .next()
            .context("launch view missing")?;
        let pid = view.pid.context("launch PID missing")?;
        self.owned.push(identity(pid)?);
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

    fn wait_view(&self, spec: &Launch, predicate: impl Fn(&View) -> bool) -> Result<View> {
        let deadline = Instant::now() + WAIT;
        loop {
            let view = self.snapshot(spec)?;
            ensure!(
                view.storage_error.is_none(),
                "generation storage error: {:?}",
                view.storage_error
            );
            if predicate(&view) {
                return Ok(view);
            }
            ensure!(
                Instant::now() < deadline,
                "generation did not reach requested state: {}",
                view.state
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

    fn restart(&mut self) -> Result<()> {
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
        self.wait_ready()
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let _ = self.stop_host();
        // Pin and revalidate each exact fixture process before cleanup. If pidfd
        // support is unavailable, fail closed instead of signaling a numeric PID.
        for expected in &self.owned {
            let _ = signal_exact_process(expected, libc::SIGKILL);
        }
    }
}

fn signal_exact_process(expected: &ProcessIdentity, signal: i32) -> Result<bool> {
    // SAFETY: pidfd_open only obtains a handle; it does not signal the process.
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid as libc::pid_t, 0) };
    if raw_fd < 0 {
        return Ok(false);
    }
    // SAFETY: successful pidfd_open returned a fresh owned descriptor.
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw_fd as i32) };
    if identity(expected.pid).ok().as_ref() != Some(expected) {
        return Ok(false);
    }
    // SAFETY: pidfd pins the task; identity was rechecked after acquiring it.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    Ok(result == 0)
}

fn wait_file(path: &Path) -> Result<u32> {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse()
        {
            return Ok(pid);
        }
        ensure!(
            Instant::now() < deadline,
            "fixture PID file was not published: {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn process_running(pid: u32) -> Result<bool> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    // comm is parenthesized and may itself contain spaces or parentheses.
    let state = stat
        .rsplit_once(") ")
        .context("malformed proc stat")?
        .1
        .chars()
        .next();
    Ok(state.is_some_and(|state| state != 'Z' && state != 'X'))
}

fn wait_stopped(pids: &[u32]) -> Result<()> {
    let deadline = Instant::now() + WAIT;
    loop {
        if pids
            .iter()
            .all(|pid| process_running(*pid).ok() == Some(false))
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "PTY process group remains live: {pids:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn identity_running(expected: &ProcessIdentity) -> Result<bool> {
    if identity(expected.pid).ok().as_ref() != Some(expected) {
        return Ok(false);
    }
    process_running(expected.pid)
}

fn assert_successful_stop(view: &View) -> Result<()> {
    ensure!(
        view.stop_scope.as_deref() == Some("pty_process_group_v1"),
        "successful Stop did not report its PTY process-group scope: {:?}",
        view.stop_scope
    );
    ensure!(
        view.stop_error.is_none(),
        "Stop reported an error: {:?}",
        view.stop_error
    );
    Ok(())
}

fn stop(host: &TestHost, spec: &Launch) -> Result<()> {
    let response = exchange(
        &host.socket(),
        Command::Stop {
            session: spec.session,
            generation: spec.generation,
        },
    )?;
    if let Some(error) = response.error {
        bail!("Stop failed: {error}");
    }
    Ok(())
}

#[test]
fn stop_kills_ignoring_pty_group_preserves_journal_and_spares_other_generation() -> Result<()> {
    ensure!(
        Path::new("/usr/bin/python3").is_file(),
        "Python 3 fixture dependency missing"
    );
    let mut host = TestHost::start()?;
    let child_pid_file = host.root().join("descendant.pid");
    let script = format!(
        r#"
import os, signal, time
signal.signal(signal.SIGHUP, signal.SIG_IGN)
pid = os.fork()
if pid == 0:
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    open({child_pid_file:?}, 'w').write(str(os.getpid()))
    print('DESCENDANT_READY', flush=True)
    while True: time.sleep(1)
print('LEADER_READY', flush=True)
while True: time.sleep(1)
"#
    );
    let first = host.launch("/usr/bin/python3", vec!["-u".into(), "-c".into(), script])?;
    let leader = host
        .wait_view(&first, |view| view.frame.text().contains("LEADER_READY"))?
        .pid
        .context("leader PID missing")?;
    let descendant = wait_file(&child_pid_file)?;
    let leader_identity = identity(leader)?;
    let descendant_identity = identity(descendant)?;
    ensure!(
        unsafe { libc::getpgid(leader as i32) } == leader as i32,
        "PTY leader is not process-group leader"
    );
    ensure!(
        unsafe { libc::getpgid(descendant as i32) } == leader as i32,
        "descendant did not inherit PTY process group"
    );
    host.owned.push(descendant_identity.clone());

    let unrelated = host.launch(
        "/usr/bin/python3",
        vec![
            "-u".into(),
            "-c".into(),
            "import sys,time; print('UNRELATED_READY',flush=True); print(sys.stdin.readline().strip(),flush=True); time.sleep(30)".into(),
        ],
    )?;
    let unrelated_pid = host
        .wait_view(&unrelated, |view| {
            view.frame.text().contains("UNRELATED_READY")
        })?
        .pid
        .context("unrelated PID missing")?;
    let journal_path = host.root().join(format!("{}.journal", first.generation));

    let stop_socket = host.socket();
    let stop_session = first.session;
    let stop_generation = first.generation;
    let stopper = thread::spawn(move || {
        exchange(
            &stop_socket,
            Command::Stop {
                session: stop_session,
                generation: stop_generation,
            },
        )
    });
    // The Stop's bounded HUP grace must not occupy the unrelated generation's
    // actor or control path.
    let input = exchange(
        &host.socket(),
        Command::Input {
            session: unrelated.session,
            generation: unrelated.generation,
            bytes: b"SECOND_STILL_LIVE\n".to_vec(),
        },
    )?;
    ensure!(
        input.error.is_none(),
        "unrelated session input failed during Stop: {:?}",
        input.error
    );
    host.wait_view(&unrelated, |view| {
        view.frame.text().contains("SECOND_STILL_LIVE")
    })?;
    let response = stopper
        .join()
        .map_err(|_| anyhow::anyhow!("Stop request thread panicked"))??;
    if let Some(error) = response.error {
        bail!("Stop failed: {error}");
    }
    let exited = host.wait_view(&first, |view| view.state == "exited")?;
    assert_successful_stop(&exited)?;
    wait_stopped(&[leader, descendant])?;
    ensure!(
        !identity_running(&leader_identity)?,
        "leader still running after Stop"
    );
    ensure!(
        !identity_running(&descendant_identity)?,
        "descendant still running after Stop"
    );
    let unrelated_view = host.snapshot(&unrelated)?;
    ensure!(
        unrelated_view.state == "running" && unrelated_view.pid == Some(unrelated_pid),
        "Stop affected unrelated generation"
    );
    let mut journal = JournalReader::open(&journal_path)?;
    let mut saw_exit = false;
    let mut saw_leader = false;
    while let Some(event) = journal.next_event()? {
        saw_exit |= event.kind == EventKind::Exit;
        saw_leader |= event.kind == EventKind::Output
            && String::from_utf8_lossy(&event.payload).contains("LEADER_READY");
    }
    ensure!(
        saw_exit && saw_leader,
        "journal did not retain output and durable Exit"
    );
    let replay = pigade_session_host::replay_journal(&journal_path)?;
    ensure!(
        replay.sequence == exited.sequence && replay.frame.text().contains("LEADER_READY"),
        "stopped generation journal replay mismatch"
    );

    // A fresh host must be able to recover the stopped generation from its normal journal.
    host.stop_host()?;
    host.restart()?;
    let reopened = host.snapshot(&first)?;
    ensure!(
        reopened.frame.text().contains("LEADER_READY"),
        "reopened generation lost saved output"
    );
    Ok(())
}

#[test]
fn stop_kills_descendant_after_leader_naturally_exits_while_pty_is_held() -> Result<()> {
    ensure!(
        Path::new("/usr/bin/python3").is_file(),
        "Python 3 fixture dependency missing"
    );
    let mut host = TestHost::start()?;
    let child_pid_file = host.root().join("orphan.pid");
    let script = format!(
        r#"
import os, signal, time
pid = os.fork()
if pid == 0:
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    open({child_pid_file:?}, 'w').write(str(os.getpid()))
    print('ORPHAN_HOLDING_PTY', flush=True)
    while True: time.sleep(1)
print('LEADER_NATURAL_EXIT', flush=True)
"#
    );
    let spec = host.launch("/usr/bin/python3", vec!["-u".into(), "-c".into(), script])?;
    let leader = host
        .wait_view(&spec, |view| {
            view.frame.text().contains("LEADER_NATURAL_EXIT")
        })?
        .pid
        .context("leader PID missing")?;
    let descendant = wait_file(&child_pid_file)?;
    let leader_identity = identity(leader)?;
    let descendant_identity = identity(descendant)?;
    host.owned.push(descendant_identity.clone());
    ensure!(
        unsafe { libc::getpgid(descendant as i32) } == leader as i32,
        "descendant escaped PTY process group"
    );
    ensure!(
        process_running(descendant)?,
        "descendant exited with leader instead of holding PTY"
    );
    ensure!(
        identity(leader).ok().as_ref() == Some(&leader_identity),
        "original leader identity was reaped before Stop could validate its group"
    );

    // Stop is intentionally issued while the old leader identity is still
    // observable, even though its program has naturally returned.
    stop(&host, &spec)?;
    host.wait_view(&spec, |view| view.state == "exited")?;
    let exited = host.snapshot(&spec)?;
    assert_successful_stop(&exited)?;
    wait_stopped(&[leader, descendant])?;
    ensure!(
        !identity_running(&leader_identity)?,
        "leader still running after Stop"
    );
    ensure!(
        !identity_running(&descendant_identity)?,
        "orphan descendant still running after Stop"
    );
    Ok(())
}

#[test]
fn stop_escalates_after_hup_leader_exit_and_pty_eof() -> Result<()> {
    ensure!(
        Path::new("/usr/bin/python3").is_file(),
        "Python 3 fixture dependency missing"
    );
    let mut host = TestHost::start()?;
    let child_pid_file = host.root().join("closed_pty_descendant.pid");
    let script = format!(
        r#"
import os, signal, time
pid = os.fork()
if pid == 0:
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    open({child_pid_file:?}, 'w').write(str(os.getpid()))
    for fd in (0, 1, 2):
        try: os.close(fd)
        except OSError: pass
    while True: time.sleep(1)
print('LEADER_READY_FOR_HUP', flush=True)
while True: time.sleep(1)
"#
    );
    let spec = host.launch("/usr/bin/python3", vec!["-u".into(), "-c".into(), script])?;
    let leader = host
        .wait_view(&spec, |view| {
            view.frame.text().contains("LEADER_READY_FOR_HUP")
        })?
        .pid
        .context("leader PID missing")?;
    let descendant = wait_file(&child_pid_file)?;
    let leader_identity = identity(leader)?;
    let descendant_identity = identity(descendant)?;
    host.owned.push(descendant_identity.clone());
    ensure!(
        unsafe { libc::getpgid(descendant as i32) } == leader as i32,
        "PTY-closed descendant escaped the original process group"
    );
    ensure!(
        process_running(descendant)?,
        "PTY-closed descendant is not running"
    );

    let stop_socket = host.socket();
    let session = spec.session;
    let generation = spec.generation;
    let stopper = thread::spawn(move || {
        exchange(
            &stop_socket,
            Command::Stop {
                session,
                generation,
            },
        )
    });
    // HUP should terminate the leader immediately. Since the descendant closed
    // every PTY descriptor, EOF arrives too; the actor must still retain the
    // unreaped leader identity until delayed SIGKILL reaches the original group.
    thread::sleep(Duration::from_millis(60));
    ensure!(
        identity(leader).ok().as_ref() == Some(&leader_identity),
        "actor reaped the leader before the process-group escalation window"
    );
    ensure!(
        identity_running(&descendant_identity)?,
        "descendant did not survive HUP until delayed escalation"
    );
    let response = stopper
        .join()
        .map_err(|_| anyhow::anyhow!("Stop request thread panicked"))??;
    if let Some(error) = response.error {
        bail!("Stop failed: {error}");
    }
    let exited = host.wait_view(&spec, |view| view.state == "exited")?;
    assert_successful_stop(&exited)?;
    wait_stopped(&[leader, descendant])?;
    ensure!(
        !identity_running(&leader_identity)?,
        "leader still running after Stop"
    );
    ensure!(
        !identity_running(&descendant_identity)?,
        "descendant still running after Stop"
    );
    Ok(())
}
