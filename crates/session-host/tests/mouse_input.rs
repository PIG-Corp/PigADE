//! Real PTY wire test for mode-aware terminal mouse input.
#[path = "../../app/src/mouse.rs"]
#[allow(dead_code)]
mod mouse;

use anyhow::{Context, Result, bail, ensure};
use mouse::{Button, Encoder, Modifiers, Position, Wheel};
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
    child: Child,
    active_generation: Option<(Uuid, Uuid)>,
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
            active_generation: None,
        };
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = host.child.try_wait()? {
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
                "PTY mouse fixture did not make progress; frame text {:?}, modes {:#x}, geometry {:?}",
                view.frame.text(),
                view.frame.modes,
                view.frame.geometry
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn send_input(&self, spec: &Launch, bytes: Vec<u8>) -> Result<()> {
        let response = exchange(
            &self.socket(),
            Command::Input {
                session: spec.session,
                generation: spec.generation,
                bytes,
            },
        )?;
        if let Some(error) = response.error {
            bail!("host input: {error}");
        }
        Ok(())
    }

    fn resize(&self, spec: &Launch, geometry: Geometry) -> Result<()> {
        let response = exchange(
            &self.socket(),
            Command::Resize {
                session: spec.session,
                generation: spec.generation,
                geometry,
            },
        )?;
        if let Some(error) = response.error {
            bail!("host resize: {error}");
        }
        Ok(())
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        if let Some((session, generation)) = self.active_generation {
            let _ = exchange(
                &self.socket(),
                Command::Stop {
                    session,
                    generation,
                },
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let stopped = checked(
                    &self.socket(),
                    Command::Snapshot {
                        session,
                        generation,
                    },
                )
                .ok()
                .and_then(|views| views.into_iter().next())
                .is_some_and(|view| view.state == "exited");
                if stopped {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn acknowledged_mouse_modes_encode_bytes_received_by_the_generation_pty() -> Result<()> {
    ensure!(
        Path::new("/usr/bin/python3").is_file(),
        "Python 3 fixture dependency missing"
    );
    let mut host = TestHost::start()?;
    let program = concat!(
        "import os,tty\n",
        "tty.setraw(0)\n",
        "os.write(1,b'\\x1b[?1002h\\x1b[?1006h__MOUSE_READY__')\n",
        "for i,n in enumerate((10,11,10,11,10,10)):\n",
        " data=b''\n",
        " while len(data)<n:\n",
        "  part=os.read(0,n-len(data))\n",
        "  if not part: raise SystemExit(2)\n",
        "  data+=part\n",
        " os.write(1,b'\\r\\n'+(f'__ACK{i}__'+data.hex()).encode()+b'\\n')\n",
        "os.write(1,b'\\x1b[?1002l\\x1b[?1006l\\r\\n__MOUSE_DISABLED__')\n",
        "while True: os.read(0,1)\n",
    );
    let spec = Launch {
        env: Default::default(),
        session: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        operation: Uuid::new_v4(),
        executable: PathBuf::from("/usr/bin/python3"),
        args: vec!["-u".into(), "-c".into(), program.into()],
        cwd: host.directory.path().canonicalize()?,
        geometry: Geometry::new(12, 60)?,
    };
    host.active_generation = Some((spec.session, spec.generation));
    let launched = checked(&host.socket(), Command::Launch { spec: spec.clone() })?;
    ensure!(
        launched.first().and_then(|view| view.pid).is_some(),
        "PTY child PID missing"
    );
    let ready = host.wait_for(&spec, |view| view.frame.text().contains("__MOUSE_READY__"))?;
    let modes = ready.frame.modes;
    ensure!(
        modes & mouse::MOUSE_DRAG != 0,
        "1002 drag mode was not acknowledged: {modes:#x}"
    );
    ensure!(
        modes & mouse::SGR_MOUSE != 0,
        "1006 SGR mode was not acknowledged: {modes:#x}"
    );

    let mut encoder = Encoder::default();
    let position = Position { row: 4, column: 9 };
    let next = Position { row: 4, column: 10 };
    let events = [
        encoder
            .press(Button::Left, position, Modifiers::default(), modes)
            .context("mouse press not encoded")?,
        encoder
            .motion(next, Modifiers::default(), modes)
            .context("drag not encoded")?,
        encoder
            .release(Button::Left, position, Modifiers::default(), modes)
            .context("mouse release not encoded")?,
        encoder
            .wheel(Wheel::Up, position, Modifiers::default(), modes)
            .context("wheel not encoded")?,
    ];
    let expected = [
        b"\x1b[<0;10;5M".as_slice(),
        b"\x1b[<32;11;5M".as_slice(),
        b"\x1b[<0;10;5m".as_slice(),
        b"\x1b[<64;10;5M".as_slice(),
    ];
    for (index, (event, expected)) in events.iter().zip(expected).enumerate() {
        assert_eq!(event.as_slice(), expected);
        if index == 1 {
            let stale = exchange(
                &host.socket(),
                Command::Input {
                    session: spec.session,
                    generation: Uuid::new_v4(),
                    bytes: b"BAD".to_vec(),
                },
            )?;
            ensure!(
                stale
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("unknown session generation")),
                "stale-generation input was not rejected: {:?}",
                stale.error
            );
        }
        host.send_input(&spec, event.clone())?;
        let marker = format!("__ACK{index}__{}", hex(expected));
        host.wait_for(&spec, |view| view.frame.text().contains(&marker))?;
    }

    let resized = Geometry::new(8, 40)?;
    host.resize(&spec, resized)?;
    host.wait_for(&spec, |view| view.frame.geometry == resized)?;
    let edge = Position { row: 7, column: 39 };
    let press = encoder
        .press(Button::Left, edge, Modifiers::default(), modes)
        .context("resized press not encoded")?;
    assert_eq!(press, b"\x1b[<0;40;8M");
    host.send_input(&spec, press.clone())?;
    host.wait_for(&spec, |view| {
        view.frame
            .text()
            .contains(&format!("__ACK4__{}", hex(&press)))
    })?;
    let release = encoder
        .release(Button::Left, edge, Modifiers::default(), modes)
        .context("resized release not encoded")?;
    assert_eq!(release, b"\x1b[<0;40;8m");
    host.send_input(&spec, release.clone())?;
    host.wait_for(&spec, |view| {
        view.frame
            .text()
            .contains(&format!("__ACK5__{}", hex(&release)))
    })?;

    let disabled = host.wait_for(&spec, |view| {
        view.frame.text().contains("__MOUSE_DISABLED__")
            && view.frame.modes & (mouse::MOUSE_DRAG | mouse::SGR_MOUSE) == 0
    })?;
    assert!(
        encoder
            .press(
                Button::Left,
                position,
                Modifiers::default(),
                disabled.frame.modes
            )
            .is_none()
    );
    assert!(
        encoder
            .wheel(
                Wheel::Down,
                position,
                Modifiers::default(),
                disabled.frame.modes
            )
            .is_none()
    );
    Ok(())
}

#[path = "../../app/src/input.rs"]
#[allow(dead_code)]
mod input;

#[test]
fn explicit_paste_bracketing_reaches_raw_pty_and_follows_acknowledged_mode() -> Result<()> {
    let bracketed = "\x1b[200~Привет\n日本😀\tend\x1b[201~".as_bytes();
    let ordinary = b"line1\rline2\r";
    let mut host = TestHost::start()?;
    let program = format!(
        "import os,tty\ntty.setraw(0)\nos.write(1,b'\\x1b[?2004h__PASTE_READY__')\n\
         for i,n in enumerate(({},{})):\n\
         \x20data=b''\n\
         \x20while len(data)<n:\n\
         \x20 part=os.read(0,n-len(data))\n\
         \x20 if not part: raise SystemExit(2)\n\
         \x20 data+=part\n\
         \x20os.write(1,b'\\r\\n'+(f'__PASTE_ACK{{i}}__'+data.hex()).encode()+b'\\n')\n\
         \x20if i==0: os.write(1,b'\\x1b[?2004l__PASTE_MODE_OFF__')\n\
         while True: os.read(0,1)\n",
        bracketed.len(),
        ordinary.len()
    );
    let spec = Launch {
        env: Default::default(),
        session: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        operation: Uuid::new_v4(),
        executable: PathBuf::from("/usr/bin/python3"),
        args: vec!["-u".into(), "-c".into(), program],
        cwd: host.directory.path().canonicalize()?,
        geometry: Geometry::new(12, 120)?,
    };
    checked(&host.socket(), Command::Launch { spec: spec.clone() })?;
    host.active_generation = Some((spec.session, spec.generation));
    let ready = host.wait_for(&spec, |view| view.frame.text().contains("__PASTE_READY__"))?;
    ensure!(ready.frame.modes & input::BRACKETED_PASTE != 0);
    let packet =
        input::paste("Привет\r\n日本😀\tend", ready.frame.modes).map_err(anyhow::Error::msg)?;
    ensure!(packet == bracketed);
    host.send_input(&spec, packet)?;
    let ack = format!("__PASTE_ACK0__{}", hex(bracketed));
    let changed = host.wait_for(&spec, |view| {
        view.frame.text().contains(&ack)
            && view.frame.text().contains("__PASTE_MODE_OFF__")
            && view.frame.modes & input::BRACKETED_PASTE == 0
    })?;
    let packet =
        input::paste("line1\nline2\r\n", changed.frame.modes).map_err(anyhow::Error::msg)?;
    ensure!(packet == ordinary);
    host.send_input(&spec, packet)?;
    let ack = format!("__PASTE_ACK1__{}", hex(ordinary));
    host.wait_for(&spec, |view| view.frame.text().contains(&ack))?;
    Ok(())
}
