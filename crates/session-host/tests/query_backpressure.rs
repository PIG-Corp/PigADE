//! Output must keep draining while the actor is waiting to admit PTY query replies.
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{Command, Launch, PROTOCOL, Request, Response, View, receive, send};
use pigade_storage::{EventKind, JournalReader};
use pigade_terminal::Geometry;
use std::{
    fs::{self, File},
    os::unix::net::UnixStream,
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

struct TestHost {
    directory: TempDir,
    process: Child,
}

impl TestHost {
    fn start() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let log = File::create(directory.path().join("host.log"))?;
        let process = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"))
            .arg("serve")
            .arg(directory.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        let mut host = Self { directory, process };
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = host.process.try_wait()? {
                bail!(
                    "host exited {status}: {}",
                    fs::read_to_string(host.directory.path().join("host.log"))?
                );
            }
            if checked(&host.socket(), Command::List).is_ok() {
                return Ok(host);
            }
            ensure!(Instant::now() < deadline, "host did not expose socket");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn socket(&self) -> PathBuf {
        self.directory.path().join("host.sock")
    }

    fn launch(&self, executable: &str, source: &str) -> Result<Launch> {
        let spec = Launch {
            session: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            operation: Uuid::new_v4(),
            executable: PathBuf::from(executable),
            args: vec!["-u".into(), "-c".into(), source.into()],
            cwd: self.directory.path().canonicalize()?,
            geometry: Geometry::new(24, 80)?,
        };
        checked(&self.socket(), Command::Launch { spec: spec.clone() })?;
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

    fn wait_text(&self, spec: &Launch, needle: &str) -> Result<View> {
        let deadline = Instant::now() + WAIT;
        loop {
            let view = self.snapshot(spec)?;
            ensure!(
                view.storage_error.is_none(),
                "actor degraded: {:?}",
                view.storage_error
            );
            if view.frame.text().contains(needle) {
                return Ok(view);
            }
            ensure!(
                Instant::now() < deadline,
                "output/query backpressure stalled; last sequence {}",
                view.sequence
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_degraded(&self, spec: &Launch) -> Result<View> {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let view = self.snapshot(spec)?;
            if view.state == "degraded" {
                return Ok(view);
            }
            ensure!(
                Instant::now() < deadline,
                "response budget was not enforced; state {}, sequence {}",
                view.state,
                view.sequence
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminate_server(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        // The fixture has its own short alarm, so a host killed during a failed
        // regression cannot leave the exact test child spinning or blocked.
    }
}

#[test]
fn pending_query_replies_do_not_stop_actor_from_draining_child_output() -> Result<()> {
    ensure!(
        Path::new("/usr/bin/python3").is_file(),
        "Python 3 fixture missing"
    );
    let host = TestHost::start()?;
    let source = r#"import os,select,signal,tty,time
signal.alarm(7)  # bounded self-cleanup if the regression deadlocks
tty.setraw(0)
count=50000
queries=b'\x1b[6n'*count
expected=b'\x1b[1;1R'*count
def writeall(data):
 view=memoryview(data)
 while view:
  n=os.write(1,view); view=view[n:]
writeall(queries)
# Force the PTY output queue to apply backpressure before the child begins
# reading the >144 KiB of query responses generated by this query burst.
writeall(b'Q'*2*1024*1024)
received=bytearray(); deadline=time.monotonic()+6
while len(received)<len(expected):
 if time.monotonic()>deadline: raise RuntimeError(('response deadline',len(received)))
 if select.select([0],[],[],.05)[0]: received.extend(os.read(0,16384))
assert bytes(received)==expected,(len(received),len(expected))
writeall(b'QUERY_BACKPRESSURE_OK\r\n')
"#;
    let spec = host.launch("/usr/bin/python3", source)?;
    let view = host.wait_text(&spec, "QUERY_BACKPRESSURE_OK")?;
    assert!(view.storage_error.is_none());
    let stopped = exchange(
        &host.socket(),
        Command::Stop {
            session: spec.session,
            generation: spec.generation,
        },
    )?;
    ensure!(
        stopped.error.is_none(),
        "stop rejected: {:?}",
        stopped.error
    );
    Ok(())
}

#[test]
fn response_budget_failure_preserves_live_view_and_replays_durable_raw_tail() -> Result<()> {
    ensure!(
        Path::new("/usr/bin/python3").is_file(),
        "Python 3 fixture missing"
    );
    let mut host = TestHost::start()?;
    let source = r#"import os,signal,tty
signal.alarm(30)  # bounded cleanup if the host is killed during this fixture
tty.setraw(0)
queries=b'\x1b[6n'*10000
def writeall(data):
 view=memoryview(data)
 while view:
  n=os.write(1,view); view=view[n:]
for _ in range(300): writeall(queries)
# The child intentionally does not read stdin. The response FIFO reaches its
# fixed byte cap while output is still being durably consumed by the actor.
while True: writeall(b'Z'*16384)
"#;
    let spec = host.launch("/usr/bin/python3", source)?;
    let degraded = host.wait_degraded(&spec)?;
    assert!(
        degraded
            .storage_error
            .as_deref()
            .is_some_and(|error| error.contains("query response budget")),
        "unexpected degradation: {:?}",
        degraded.storage_error
    );
    assert!(degraded.sequence > 0, "fixture did not publish any prefix");
    let journal_path = host
        .directory
        .path()
        .join(format!("{}.journal", spec.generation));
    let mut reader = JournalReader::open(&journal_path)?;
    let mut last_sequence = 0;
    let mut last_kind = None;
    while let Some(event) = reader.next_event()? {
        last_sequence = event.sequence;
        last_kind = Some(event.kind);
    }
    assert_eq!(last_kind, Some(EventKind::Output));
    assert_eq!(
        last_sequence,
        degraded.sequence + 1,
        "failed event should be durable while the published view stays at its prior prefix"
    );

    // Recovery treats the raw event as accepted durable output and replays it
    // as model state; it does not replay query replies as external effects.
    host.terminate_server();
    let recovered = pigade_session_host::replay_journal(&journal_path)?;
    assert_eq!(recovered.sequence, last_sequence);
    assert_eq!(recovered.state, "history");
    assert!(
        recovered.storage_error.is_none(),
        "{:?}",
        recovered.storage_error
    );
    Ok(())
}
