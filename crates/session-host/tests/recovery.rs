//! Process-level Linux recovery probes. No provider accounts or credentials.
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{
    Command, Launch, PROTOCOL, Request, Response, View, identity, receive, send,
};
use pigade_storage::{EventKind, JournalReader, ProcessIdentity};
use pigade_terminal::Geometry;
use std::{
    fs::{self, File},
    os::unix::{
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(8);

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
    ensure!(response.protocol == PROTOCOL, "protocol mismatch");
    Ok(response)
}
fn checked(socket: &Path, command: Command) -> Result<Vec<View>> {
    let response = exchange(socket, command)?;
    if let Some(error) = response.error {
        bail!("host: {error}");
    }
    Ok(response.views)
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
    directory: TempDir,
    child: Option<Child>,
    owned_cli: Vec<ProcessIdentity>,
}
impl TestHost {
    fn new() -> Result<Self> {
        Self::in_directory(tempfile::tempdir()?)
    }
    fn in_directory(directory: TempDir) -> Result<Self> {
        let mut host = Self {
            directory,
            child: None,
            owned_cli: Vec::new(),
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
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.as_mut().unwrap().try_wait()? {
                bail!(
                    "host exited {status}: {}",
                    fs::read_to_string(self.root().join("host.log"))?
                );
            }
            if checked(&self.socket(), Command::List).is_ok() {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "host did not expose socket");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn kill_host(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            child.kill()?;
            child.wait()?;
        }
        Ok(())
    }
    fn launch(&mut self, executable: &str, args: Vec<String>) -> Result<Launch> {
        let spec = Launch {
            session: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            operation: Uuid::new_v4(),
            executable: PathBuf::from(executable),
            args,
            cwd: self.root().canonicalize()?,
            geometry: Geometry::new(12, 60)?,
        };
        let views = checked(&self.socket(), Command::Launch { spec: spec.clone() })?;
        let pid = views
            .first()
            .and_then(|v| v.pid)
            .context("live generation missing PID")?;
        self.owned_cli.push(identity(pid)?);
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
    fn input(&self, spec: &Launch, bytes: &[u8]) -> Result<()> {
        checked(
            &self.socket(),
            Command::Input {
                session: spec.session,
                generation: spec.generation,
                bytes: bytes.to_vec(),
            },
        )?;
        Ok(())
    }
    fn wait_text(&self, spec: &Launch, text: &str) -> Result<View> {
        wait_for(|| {
            let view = self.snapshot(spec)?;
            ensure!(
                view.storage_error.is_none(),
                "storage degraded: {:?}",
                view.storage_error
            );
            Ok(view.frame.text().contains(text).then_some(view))
        })
    }
}
impl Drop for TestHost {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Never kill a reused PID or a process not launched by this test. PTY
        // children normally lead a private session/process group.
        for expected in &self.owned_cli {
            if identity(expected.pid).ok().as_ref() == Some(expected) {
                // SAFETY: target is an exact verified process owned by this test.
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
}

#[test]
fn shell_survives_100_client_reopens_and_real_stty_resize() -> Result<()> {
    let mut host = TestHost::new()?;
    let spec = host.launch(
        "/bin/bash",
        vec!["--noprofile".into(), "--norc".into(), "-i".into()],
    )?;
    // Octal escapes keep the marker out of tty command echo.
    host.input(&spec, b"printf '\\137\\137PIGADE_READY\\137\\137\\n'\n")?;
    let ready = host.wait_text(&spec, "__PIGADE_READY__")?;
    for _ in 0..100 {
        let view = host.snapshot(&spec)?;
        assert_eq!(view.generation, spec.generation);
        assert_eq!(view.pid, ready.pid);
        assert_eq!(view.state, "running");
    }
    checked(
        &host.socket(),
        Command::Resize {
            session: spec.session,
            generation: spec.generation,
            geometry: Geometry::new(31, 97)?,
        },
    )?;
    wait_for(|| {
        Ok((host.snapshot(&spec)?.frame.geometry == Geometry::new(31, 97)?).then_some(()))
    })?;
    host.input(&spec, b"stty size\n")?;
    let resized = host.wait_text(&spec, "31 97")?;
    assert_eq!(resized.pid, ready.pid);
    let stale = exchange(
        &host.socket(),
        Command::Input {
            session: spec.session,
            generation: Uuid::new_v4(),
            bytes: b"echo WRONG\n".to_vec(),
        },
    )?;
    assert!(stale.error.is_some());
    let duplicate = checked(&host.socket(), Command::Launch { spec: spec.clone() })?;
    assert_eq!(duplicate[0].pid, ready.pid);
    assert_eq!(checked(&host.socket(), Command::List)?.len(), 1);
    Ok(())
}

#[test]
fn real_tui_1000_resizes_have_lossless_order_and_replay() -> Result<()> {
    let mut host = TestHost::new()?;
    let python = r#"import os,signal,fcntl,termios,struct,sys
os.write(1,b'\x1b[?1049h')
def draw(*_):
 r,c,_,_=struct.unpack('HHHH',fcntl.ioctl(1,termios.TIOCGWINSZ,b'\0'*8))
 os.write(1, ('\x1b[2J\x1b[HGEOMETRY=%dx%d\r\nРусский e\u0301 界' % (r,c)).encode())
signal.signal(signal.SIGWINCH,draw)
draw()
for line in sys.stdin:
 if line.strip()=='q': break
os.write(1,b'\x1b[?1049lTUI_DONE\r\n')
"#;
    let executable = if Path::new("/usr/bin/python3").is_file() {
        "/usr/bin/python3"
    } else {
        bail!("Python 3 fixture dependency missing");
    };
    let spec = host.launch(executable, vec!["-u".into(), "-c".into(), python.into()])?;
    host.wait_text(&spec, "GEOMETRY=12x60")?;
    let deadline = Instant::now() + Duration::from_secs(45);
    for n in 0..1000 {
        let geometry = Geometry::new(12 + n % 3, 60 + n % 7)?;
        loop {
            ensure!(Instant::now() < deadline, "resize corpus deadline exceeded");
            let result = exchange(
                &host.socket(),
                Command::Resize {
                    session: spec.session,
                    generation: spec.generation,
                    geometry,
                },
            )?;
            match result.error {
                None => break,
                Some(error) if error.contains("full") => thread::sleep(Duration::from_millis(1)),
                Some(error) => bail!("resize failed: {error}"),
            }
        }
    }
    host.wait_text(&spec, "GEOMETRY=12x65")?;
    host.input(&spec, b"q\n")?;
    let final_live = wait_for(|| {
        let view = host.snapshot(&spec)?;
        Ok((view.state == "exited").then_some(view))
    })?;
    assert!(final_live.frame.text().contains("TUI_DONE"));
    let path = host.root().join(format!("{}.journal", spec.generation));
    let mut journal = JournalReader::open(&path)?;
    let mut resizes = 0;
    while let Some(event) = journal.next_event()? {
        if event.kind == EventKind::Resize {
            resizes += 1;
        }
    }
    assert_eq!(resizes, 1000);
    let replay = pigade_session_host::replay_journal(&path)?;
    assert_eq!(replay.sequence, final_live.sequence);
    assert_eq!(replay.frame, final_live.frame);
    let verified = pigade_session_host::verify_recovery(&path)?;
    assert!(verified.complete_state_equal && verified.frame_equal);
    assert_eq!(verified.sequence, final_live.sequence);
    Ok(())
}

#[test]
fn host_sigkill_restart_restores_history_without_another_spawn() -> Result<()> {
    let mut host = TestHost::new()?;
    let marker = host.root().join("launch-count");
    let script = format!(
        "printf x >> '{}'; printf '\\137\\137DURABLE\\137\\137\\n'; while IFS= read -r line; do :; done",
        marker.display()
    );
    let spec = host.launch(
        "/bin/bash",
        vec!["--noprofile".into(), "--norc".into(), "-c".into(), script],
    )?;
    let before = host.wait_text(&spec, "__DURABLE__")?;
    host.kill_host()?;
    host.start()?;
    let after = host.snapshot(&spec)?;
    assert!(matches!(
        after.state.as_str(),
        "history" | "launch_outcome_unknown"
    ));
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.frame, before.frame);
    assert_eq!(after.sequence, before.sequence);
    assert_eq!(fs::read(&marker)?, b"x");
    let retry = exchange(&host.socket(), Command::Launch { spec: spec.clone() })?;
    if retry.error.is_none() {
        assert!(retry.views.iter().all(|v| v.state != "running"));
    }
    assert_eq!(fs::read(&marker)?, b"x");
    Ok(())
}

#[test]
fn real_pty_cold_history_survives_host_kill_and_matches_every_replayed_record() -> Result<()> {
    let mut host = TestHost::new()?;
    let marker = host.root().join("cold-launch-count");
    let script = "import sys\nwith open(sys.argv[1], 'a') as f: f.write('x')\nfor i in range(12500): sys.stdout.write(f'{i:05d} Русский e\\u0301 界\\n')\nsys.stdout.write('__COLD_DONE__\\n'); sys.stdout.flush()";
    let spec = host.launch(
        "/usr/bin/python3",
        vec![
            "-c".into(),
            script.into(),
            marker.to_string_lossy().into_owned(),
        ],
    )?;
    let before = wait_for(|| {
        let view = host.snapshot(&spec)?;
        ensure!(
            view.storage_error.is_none(),
            "cold history degraded: {:?}",
            view.storage_error
        );
        Ok((view.state == "exited").then_some(view))
    })?;
    assert!(before.frame.text().contains("__COLD_DONE__"));
    assert!(before.history.as_ref().unwrap().retired_rows > 2000);
    let journal = host.root().join(format!("{}.journal", spec.generation));
    let verified = pigade_session_host::verify_recovery(&journal)?;
    assert!(verified.complete_state_equal && verified.frame_equal);
    host.kill_host()?;
    host.start()?;
    let after = host.snapshot(&spec)?;
    assert_eq!(after.sequence, before.sequence);
    assert_eq!(after.frame, before.frame);
    assert_eq!(after.history, before.history);
    assert_eq!(after.recovery_path, "checkpoint-tail");
    assert_eq!(fs::read(&marker)?, b"x");
    Ok(())
}

#[test]
fn long_valid_host_socket_directory_still_allows_authenticated_launch() -> Result<()> {
    let directory = tempfile::Builder::new()
        .prefix(&format!("pigade-state-{}", "x".repeat(48)))
        .tempdir()?;
    assert!(directory.path().as_os_str().len() + 42 >= 108);
    assert!(directory.path().join("host.sock").as_os_str().len() < 108);
    let mut host = TestHost::in_directory(directory)?;
    let spec = host.launch(
        "/bin/sh",
        vec![
            "-c".into(),
            "printf '__LONG_PATH_OK__\\n'; read hold".into(),
        ],
    )?;
    host.wait_text(&spec, "__LONG_PATH_OK__")?;
    assert!(!host.root().join("g.sock").exists());
    Ok(())
}

#[test]
fn stop_wakes_clean_actor_after_idle_checkpoint_without_polling_delay() -> Result<()> {
    let mut host = TestHost::new()?;
    let spec = host.launch(
        "/bin/sh",
        vec!["-c".into(), "printf '__IDLE_READY__\\n'; read hold".into()],
    )?;
    let ready = host.wait_text(&spec, "__IDLE_READY__")?;
    let checkpoint = host.root().join(format!(
        "{}.{:020}.checkpoint",
        spec.generation, ready.sequence
    ));
    wait_for(|| Ok(checkpoint.is_file().then_some(())))?;
    // A completed quiet checkpoint leaves no timer; Stop must explicitly wake
    // the actor rather than rely on a continuous ten-millisecond polling loop.
    thread::sleep(Duration::from_millis(25));
    let start = Instant::now();
    checked(
        &host.socket(),
        Command::Stop {
            session: spec.session,
            generation: spec.generation,
        },
    )?;
    wait_for(|| Ok((host.snapshot(&spec)?.state == "exited").then_some(())))?;
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "idle Stop took {:?}",
        start.elapsed()
    );
    Ok(())
}

#[test]
fn helper_disconnected_before_exec_permission_has_no_external_side_effect() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let gate = directory.path().join("gate.sock");
    let marker = directory.path().join("must-not-exist");
    let listener = UnixListener::bind(&gate)?;
    listener.set_nonblocking(true)?;
    let nonce = Uuid::new_v4().to_string();
    let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"))
        .arg("__helper")
        .arg(&gate)
        .arg(&nonce)
        .arg("/bin/sh")
        .arg("-c")
        .arg("printf executed > \"$1\"")
        .arg("pigade-gate-test")
        .arg(&marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Keep child cleanup scoped even when a handshake assertion fails.
    struct Guard<'a>(&'a mut Child);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let guard = Guard(&mut child);
    let mut stream = wait_for(|| match listener.accept() {
        Ok((stream, _)) => Ok(Some(stream)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e.into()),
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    assert_eq!(receive::<String>(&mut stream)?, nonce);
    drop(stream);
    drop(listener);
    let status = wait_for(|| Ok(guard.0.try_wait()?))?;
    assert!(!status.success());
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn layout_cross_workspace_commit_reopens_with_undo_and_does_not_restart_cli() -> Result<()> {
    use pigade_layout::{Command as Placement, Direction};
    use pigade_session_host::LayoutAction;
    use pigade_storage::{
        Catalog, CatalogBatch, CatalogWrite, Project, Tab, Workspace, WorkspaceMode,
        WorkspaceStatus,
    };

    fn catalog_call(host: &TestHost, command: Command) -> Result<Catalog> {
        let response = exchange(&host.socket(), command)?;
        ensure!(response.error.is_none(), "catalog: {:?}", response.error);
        response.catalog.context("catalog response missing")
    }
    fn edit(host: &TestHost, revision: u64, action: LayoutAction) -> Result<Catalog> {
        catalog_call(
            host,
            Command::LayoutEdit {
                expected_revision: revision,
                action,
            },
        )
    }

    let mut host = TestHost::new()?;
    let spec = host.launch("/usr/bin/bash", vec!["--noprofile".into(), "--norc".into()])?;
    host.input(&spec, b"printf '\\137\\137LAYOUT_READY\\137\\137\\n'\n")?;
    let live = host.wait_text(&spec, "__LAYOUT_READY__")?;
    let project = Uuid::new_v4();
    let workspaces = [Uuid::new_v4(), Uuid::new_v4()];
    let tabs = [Uuid::new_v4(), Uuid::new_v4()];
    // This probe provisions metadata; Git/worktree preparation is a separate
    // contract and is intentionally not asserted by a fake Git directory.
    let batch = CatalogBatch {
        projects: vec![CatalogWrite {
            value: Project {
                id: project,
                canonical_path: host.root().canonicalize()?,
                git_common_dir: host.root().join("not-a-git-probe"),
                name: "Recovery fixture".into(),
                roots: vec![],
                revision: 0,
            },
            expected_revision: None,
        }],
        workspaces: workspaces
            .iter()
            .map(|id| CatalogWrite {
                value: Workspace {
                    id: *id,
                    project_id: project,
                    task: "layout metadata fixture".into(),
                    cwd: host.root().to_path_buf(),
                    mode: WorkspaceMode::SharedCheckout,
                    status: WorkspaceStatus::Ready,
                    error: None,
                    context_revision: None,
                    revision: 0,
                },
                expected_revision: None,
            })
            .collect(),
        tabs: tabs
            .iter()
            .zip(workspaces)
            .map(|(id, workspace_id)| CatalogWrite {
                value: Tab {
                    id: *id,
                    workspace_id,
                    name: "Terminal".into(),
                    layout_json: "null".into(),
                    active_pane: None,
                    revision: 0,
                },
                expected_revision: None,
            })
            .collect(),
        ..CatalogBatch::default()
    };
    let mut catalog = catalog_call(&host, Command::CatalogApply { batch })?;
    catalog = edit(
        &host,
        catalog.layout.revision,
        LayoutAction::Apply {
            command: Placement::Insert {
                tab: tabs[0],
                pane: spec.session,
                target: None,
                side: Direction::Right,
                split: Uuid::new_v4(),
            },
        },
    )?;
    for step in 0..60 {
        catalog = edit(
            &host,
            catalog.layout.revision,
            LayoutAction::Apply {
                command: Placement::Transfer {
                    pane: spec.session,
                    to_tab: tabs[(step + 1) % 2],
                    target: None,
                    side: Direction::Right,
                    split: Uuid::new_v4(),
                },
            },
        )?;
    }
    let same = host.snapshot(&spec)?;
    assert_eq!(
        (same.session, same.generation, same.pid),
        (live.session, live.generation, live.pid)
    );
    host.input(
        &spec,
        b"printf '\\137\\137STILL_SAME_PROCESS\\137\\137\\n'\n",
    )?;
    host.wait_text(&spec, "__STILL_SAME_PROCESS__")?;
    let saved = catalog.clone();
    host.kill_host()?;
    host.start()?;
    catalog = catalog_call(&host, Command::Catalog)?;
    assert_eq!(catalog, saved);
    let historical = host.snapshot(&spec)?;
    assert_eq!(historical.generation, spec.generation);
    assert_eq!(historical.state, "launch_outcome_unknown");
    assert!(historical.pid.is_none());
    for _ in 0..50 {
        catalog = edit(&host, catalog.layout.revision, LayoutAction::Undo)?;
    }
    // Stale UI cannot overwrite the committed placement or its history.
    let stale = exchange(
        &host.socket(),
        Command::LayoutEdit {
            expected_revision: saved.layout.revision,
            action: LayoutAction::Redo,
        },
    )?;
    assert!(
        stale
            .error
            .as_deref()
            .is_some_and(|s| s.contains("revision conflict"))
    );
    assert_eq!(catalog_call(&host, Command::Catalog)?, catalog);
    host.kill_host()?;
    host.start()?;
    for _ in 0..50 {
        catalog = edit(&host, catalog.layout.revision, LayoutAction::Redo)?;
    }
    assert_eq!(
        catalog
            .tabs
            .iter()
            .map(|t| (&t.layout_json, t.active_pane))
            .collect::<Vec<_>>(),
        saved
            .tabs
            .iter()
            .map(|t| (&t.layout_json, t.active_pane))
            .collect::<Vec<_>>()
    );
    let views = checked(&host.socket(), Command::List)?;
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].generation, spec.generation);
    assert!(
        views[0].pid.is_none(),
        "placement redo must never create a process"
    );
    Ok(())
}

#[test]
fn stop_remains_serviceable_after_live_process_closes_all_pty_descriptors() -> Result<()> {
    let mut host = TestHost::new()?;
    let python = "import os,time\nos.write(1,b'PTY_CLOSING\\r\\n')\nfor fd in (0,1,2): os.close(fd)\ntime.sleep(120)\n";
    let spec = host.launch(
        "/usr/bin/python3",
        vec!["-u".into(), "-c".into(), python.into()],
    )?;
    let before = host.wait_text(&spec, "PTY_CLOSING")?;
    assert_eq!(before.state, "running");
    thread::sleep(Duration::from_millis(40));
    let start = Instant::now();
    checked(
        &host.socket(),
        Command::Stop {
            session: spec.session,
            generation: spec.generation,
        },
    )?;
    wait_for(|| {
        let view = host.snapshot(&spec)?;
        Ok((view.state == "exited").then_some(view))
    })?;
    ensure!(
        start.elapsed() < Duration::from_secs(2),
        "Stop blocked after PTY EOF"
    );
    Ok(())
}

#[test]
fn quiet_dirty_state_and_resize_only_tail_receive_checkpoints() -> Result<()> {
    let mut host = TestHost::new()?;
    let spec = host.launch(
        "/usr/bin/python3",
        vec![
            "-u".into(),
            "-c".into(),
            "import os,time;os.write(1,b'\\x1b[?2026hQUIET_READY\\r\\n');time.sleep(120)".into(),
        ],
    )?;
    host.wait_text(&spec, "QUIET_READY")?;
    let path = host.root().join(format!("{}.journal", spec.generation));
    let header = JournalReader::open(&path)?.header().clone();
    let latest = || -> Result<Option<pigade_session_host::checkpoint::Loaded>> {
        Ok(pigade_session_host::checkpoint::latest(
            host.root(),
            &header,
            fs::metadata(&path)?.len(),
        )?
        .loaded)
    };
    // A quiet session must not wait indefinitely for the next output event.
    let first = wait_for(latest)?;
    let base = host.snapshot(&spec)?.sequence;
    assert_eq!(first.sequence, base);
    for index in 0..1100 {
        loop {
            let reply = exchange(
                &host.socket(),
                Command::Resize {
                    session: spec.session,
                    generation: spec.generation,
                    geometry: Geometry::new(12, 60 + index % 3)?,
                },
            )?;
            match reply.error {
                None => break,
                Some(error) if error.contains("full") => thread::sleep(Duration::from_millis(1)),
                Some(error) => bail!("resize-only: {error}"),
            }
        }
    }
    let final_view = wait_for(|| {
        let view = host.snapshot(&spec)?;
        Ok((view.sequence >= base + 1100).then_some(view))
    })?;
    let last = latest()?.context("checkpoint disappeared")?;
    ensure!(
        last.sequence > first.sequence,
        "resize-only stream bypassed checkpoint cadence"
    );
    ensure!(
        final_view.sequence - last.sequence < 1024,
        "resize-only recovery tail exceeded event bound"
    );
    assert!(pigade_session_host::verify_recovery(&path)?.complete_state_equal);
    Ok(())
}

#[test]
fn burst_query_responses_are_chunked_without_loss_or_false_degradation() -> Result<()> {
    let mut host = TestHost::new()?;
    let python = r#"import os,select,time,tty
# A synchronous flush can release many queries together. Replies exceed one
# input-writer chunk and must retain exact order/count without stalling Stop.
tty.setraw(0)
queries=b'\x1b[?2026h'+b'\x1b[6n'*20000+b'\x1b[?2026l'
view=memoryview(queries)
while view:
 n=os.write(1,view);view=view[n:]
expected=b'\x1b[1;1R'*20000
received=bytearray()
deadline=time.monotonic()+6
while len(received)<len(expected):
 if time.monotonic()>deadline: raise RuntimeError('response deadline')
 if select.select([0],[],[],.1)[0]: received.extend(os.read(0,16384))
assert bytes(received)==expected,(len(received),len(expected))
os.write(1,b'QUERY_BURST_OK\r\n')
"#;
    let spec = host.launch(
        "/usr/bin/python3",
        vec!["-u".into(), "-c".into(), python.into()],
    )?;
    host.wait_text(&spec, "QUERY_BURST_OK")?;
    let final_view = wait_for(|| {
        let view = host.snapshot(&spec)?;
        Ok((view.state == "exited").then_some(view))
    })?;
    assert!(final_view.storage_error.is_none());
    let path = host.root().join(format!("{}.journal", spec.generation));
    assert!(pigade_session_host::verify_recovery(&path)?.complete_state_equal);
    Ok(())
}
