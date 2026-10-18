//! Real-PTY integration coverage for immutable paged history snapshots.
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{
    Command, Launch, PROTOCOL, Request, Response, View, history_view, receive, send,
};
use pigade_terminal::Geometry;
use std::{
    fs::{self, File},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(30);
const FIXTURE: &str = r#"
import sys
sys.stdout.write("\x1b[38;2;12;34;56m\x1b]8;;https://example.test/history\x07COLD_OLD_MARKER_界\x1b]8;;\x07\x1b[0m\r\n")
sys.stdout.write("".join(f"FILLER_{i:05d}\r\n" for i in range(10050)))
sys.stdout.write("SOFTWRAP_UTF8_界_MARKER\r\nCURRENT_HOT_MARKER\r\nREADY_HIST_VIEW\r\n")
sys.stdout.flush()
sys.stdin.readline()
print("AFTER_SNAPSHOT_MARKER", flush=True)
sys.stdin.readline()
"#;

fn exchange(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
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

fn success(socket: &Path, command: Command) -> Result<Response> {
    let response = exchange(socket, command)?;
    if let Some(error) = response.error.as_deref() {
        bail!("host command failed: {error}");
    }
    Ok(response)
}

fn history_reply(response: Response) -> Result<history_view::Reply> {
    response.history_view.context("history-view reply missing")
}

struct TestHost {
    directory: TempDir,
    child: Option<Child>,
    active: Option<(Uuid, Uuid)>,
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
            active: None,
        };
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = host
                .child
                .as_mut()
                .context("host child missing")?
                .try_wait()?
            {
                bail!(
                    "host exited {status}: {}",
                    fs::read_to_string(host.root().join("host.log"))?
                );
            }
            if success(&host.socket(), Command::List).is_ok() {
                return Ok(host);
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

    fn launch(&mut self) -> Result<Launch> {
        let spec = Launch {
            session: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            operation: Uuid::new_v4(),
            executable: PathBuf::from("/usr/bin/python3"),
            args: vec!["-u".into(), "-c".into(), FIXTURE.into()],
            env: Default::default(),
            cwd: self.root().canonicalize()?,
            geometry: Geometry::new(3, 16)?,
        };
        let reply = success(&self.socket(), Command::Launch { spec: spec.clone() })?;
        ensure!(
            reply.views.first().and_then(|view| view.pid).is_some(),
            "fixture PTY PID missing"
        );
        self.active = Some((spec.session, spec.generation));
        Ok(spec)
    }

    fn view(&self, spec: &Launch) -> Result<View> {
        success(
            &self.socket(),
            Command::Snapshot {
                session: spec.session,
                generation: spec.generation,
            },
        )?
        .views
        .into_iter()
        .next()
        .context("fixture snapshot missing")
    }

    fn wait_view(&self, spec: &Launch, predicate: impl Fn(&View) -> bool) -> Result<View> {
        let deadline = Instant::now() + WAIT;
        loop {
            let view = self.view(spec)?;
            if predicate(&view) {
                return Ok(view);
            }
            ensure!(
                Instant::now() < deadline,
                "PTY fixture did not reach requested publication; sequence={}, history={:?}, frame={:?}",
                view.sequence,
                view.history,
                view.frame.text()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn open(&self, spec: &Launch) -> Result<history_view::Page> {
        match history_reply(success(
            &self.socket(),
            Command::HistoryView {
                query: history_view::Query::Open {
                    session: spec.session,
                    generation: spec.generation,
                },
            },
        )?)? {
            history_view::Reply::Page { page } => Ok(page),
            other => bail!("open returned unexpected history reply: {other:?}"),
        }
    }

    fn page(&self, snapshot: Uuid, start: history_view::Cursor) -> Result<history_view::Page> {
        match history_reply(success(
            &self.socket(),
            Command::HistoryView {
                query: history_view::Query::Page { snapshot, start },
            },
        )?)? {
            history_view::Reply::Page { page } => Ok(page),
            other => bail!("page returned unexpected history reply: {other:?}"),
        }
    }

    fn search(
        &self,
        snapshot: Uuid,
        text: &str,
        cursor: Option<history_view::Cursor>,
    ) -> Result<history_view::SearchResult> {
        match history_reply(success(
            &self.socket(),
            Command::HistoryView {
                query: history_view::Query::Search {
                    snapshot,
                    text: text.into(),
                    cursor,
                },
            },
        )?)? {
            history_view::Reply::Search { result } => Ok(result),
            other => bail!("search returned unexpected history reply: {other:?}"),
        }
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        if let Some((session, generation)) = self.active {
            let _ = exchange(
                &self.socket(),
                Command::Stop {
                    session,
                    generation,
                },
            );
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn search_until_found(
    host: &TestHost,
    snapshot: Uuid,
    text: &str,
) -> Result<history_view::SearchResult> {
    let deadline = Instant::now() + WAIT;
    let mut cursor = None;
    for _ in 0..256 {
        let result = host.search(snapshot, text, cursor)?;
        if result.found.is_some() {
            return Ok(result);
        }
        let Some(next) = result.next else {
            bail!("history search exhausted without finding {text:?}");
        };
        cursor = Some(next);
        ensure!(
            Instant::now() < deadline,
            "history search continuation timed out"
        );
    }
    bail!("history search exceeded the bounded continuation count")
}

fn page_text(page: &history_view::Page) -> String {
    page.rows
        .iter()
        .map(|row| row.chunk.text.as_str())
        .collect()
}

fn compact_frame(view: &View) -> String {
    view.frame.text().lines().map(str::trim_end).collect()
}

#[test]
fn real_pty_history_pages_searches_and_keeps_open_snapshots_immutable() -> Result<()> {
    let mut host = TestHost::start()?;
    let spec = host.launch()?;
    let published = host.wait_view(&spec, |view| {
        view.history
            .as_ref()
            .is_some_and(|history| history.retired_rows > 0)
            && compact_frame(view).contains("READY_HIST_VIEW")
    })?;

    // An unowned pair cannot open an arbitrary journal, and the caller never
    // supplies the source path or sequence.
    let unknown = exchange(
        &host.socket(),
        Command::HistoryView {
            query: history_view::Query::Open {
                session: Uuid::new_v4(),
                generation: Uuid::new_v4(),
            },
        },
    )?;
    ensure!(
        unknown
            .error
            .as_deref()
            .is_some_and(|error| error.contains("unknown session generation"))
    );
    let unknown_generation = exchange(
        &host.socket(),
        Command::HistoryView {
            query: history_view::Query::Open {
                session: spec.session,
                generation: Uuid::new_v4(),
            },
        },
    )?;
    ensure!(
        unknown_generation
            .error
            .as_deref()
            .is_some_and(|error| error.contains("unknown session generation"))
    );

    let first_page = host.open(&spec)?;
    ensure!(
        first_page.sequence == published.sequence,
        "open did not freeze the published sequence"
    );
    ensure!(
        first_page.total_rows > 10_000,
        "expected cold and hot rows in the snapshot"
    );
    ensure!(
        page_text(&first_page).contains("CURRENT_HOT_MARKER"),
        "open page omitted current normal-screen rows"
    );
    let snapshot = first_page.snapshot;
    let initial_page = host.page(snapshot, first_page.start)?;
    ensure!(
        initial_page == first_page,
        "opening page did not remain stable on reread"
    );

    let old = search_until_found(&host, snapshot, "COLD_OLD_MARKER_界")?;
    ensure!(
        old.found
            .is_some_and(|cursor| cursor.row < first_page.total_rows - 100),
        "old marker was not retained in cold history"
    );

    let wrapped = search_until_found(&host, snapshot, "SOFTWRAP_UTF8_界_MARKER")?;
    let found = wrapped
        .found
        .context("soft-wrapped UTF-8 search omitted its match cursor")?;
    let wrapped_page = host.page(snapshot, found)?;
    ensure!(
        wrapped_page.start == found,
        "page did not honor the exact match cursor"
    );
    ensure!(!wrapped_page.rows.is_empty(), "match page has no rows");
    ensure!(
        wrapped_page.rows[0].chunk.offset == found.offset,
        "first text chunk did not start at the exact byte offset"
    );
    let joined: String = wrapped_page
        .rows
        .iter()
        .take(3)
        .map(|row| row.chunk.text.as_str())
        .collect();
    ensure!(
        joined.contains("SOFTWRAP_UTF8_界_MARKER"),
        "page chunks did not losslessly reconstruct the wrapped UTF-8 marker: {joined:?}"
    );
    ensure!(
        wrapped_page.rows[0].chunk.wrap,
        "fixture marker did not occupy a soft-wrapped row"
    );
    ensure!(
        wrapped_page
            .rows
            .iter()
            .all(|row| !row.chunk.text.contains('\u{1b}')),
        "history text exposed escape/control bytes"
    );

    // Continuation tokens are bound to the active query state. No prior search
    // state exists for this new text, so an arbitrary cursor must be rejected.
    let stale = exchange(
        &host.socket(),
        Command::HistoryView {
            query: history_view::Query::Search {
                snapshot,
                text: "COLD_OLD_MARKER_界".into(),
                cursor: Some(history_view::Cursor { row: 1, offset: 1 }),
            },
        },
    )?;
    ensure!(
        stale
            .error
            .as_deref()
            .is_some_and(|error| error.contains("stale history search cursor"))
    );

    let input = exchange(
        &host.socket(),
        Command::Input {
            session: spec.session,
            generation: spec.generation,
            bytes: b"mutate\n".to_vec(),
        },
    )?;
    ensure!(
        input.error.is_none(),
        "fixture mutation input failed: {:?}",
        input.error
    );
    let mutated = host.wait_view(&spec, |view| {
        compact_frame(view).contains("AFTER_SNAPSHOT_MARKER")
    })?;
    ensure!(
        mutated.sequence > published.sequence,
        "mutation did not advance the publication sequence"
    );
    let unchanged = host.page(snapshot, first_page.start)?;
    ensure!(
        unchanged == first_page,
        "old history snapshot changed after live PTY output"
    );
    ensure!(
        unchanged.sequence == published.sequence,
        "old history sequence changed after mutation"
    );

    let refreshed = host.open(&spec)?;
    ensure!(
        refreshed.sequence == mutated.sequence,
        "refreshed history did not use the newer publication"
    );
    let refreshed_text = page_text(&refreshed);
    ensure!(
        refreshed_text.contains("AFTER_SNAPSHOT_MARKER"),
        "refreshed page omitted new live output"
    );

    let close = success(
        &host.socket(),
        Command::HistoryView {
            query: history_view::Query::Close { snapshot },
        },
    )?;
    ensure!(
        matches!(history_reply(close)?, history_view::Reply::Closed { snapshot: closed } if closed == snapshot)
    );
    let closed_page = exchange(
        &host.socket(),
        Command::HistoryView {
            query: history_view::Query::Page {
                snapshot,
                start: first_page.start,
            },
        },
    )?;
    ensure!(
        closed_page
            .error
            .as_deref()
            .is_some_and(|error| error.contains("expired or closed"))
    );
    Ok(())
}

#[test]
fn colored_grid_is_bounded_readonly_and_frozen_across_new_output() -> Result<()> {
    let mut host = TestHost::start()?;
    let spec = host.launch()?;
    let before = host.wait_view(&spec, |view| {
        view.history
            .as_ref()
            .is_some_and(|summary| summary.retired_rows > 0)
            && compact_frame(view).contains("READY_HIST_VIEW")
    })?;
    let reply = success(
        &host.socket(),
        Command::HistoryView {
            query: history_view::Query::OpenGrid {
                session: spec.session,
                generation: spec.generation,
                rows: 8,
                cols: 16,
                back: 0,
            },
        },
    )?;
    let history_view::Reply::Grid { page: opened } = history_reply(reply)? else {
        bail!("expected colored history page")
    };
    ensure!(opened.sequence == before.sequence && opened.total_rows > 10000);
    let query = history_view::Query::Grid {
        snapshot: opened.snapshot,
        start: 0,
        rows: 4,
        cols: 16,
    };
    let history_view::Reply::Grid { page: first } = history_reply(success(
        &host.socket(),
        Command::HistoryView {
            query: query.clone(),
        },
    )?)?
    else {
        bail!("expected cold cells")
    };
    ensure!(first.start == 0 && first.rows[0].row == 0);
    ensure!(
        first
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .any(|cell| cell.fg == "Spec(Rgb { r: 12, g: 34, b: 56 })")
    );
    ensure!(
        first
            .hyperlinks
            .iter()
            .any(|(_, uri)| uri == "https://example.test/history")
    );
    let readonly = host.view(&spec)?;
    ensure!(
        readonly.sequence == before.sequence
            && readonly.frame == before.frame
            && readonly.pid == before.pid
    );
    success(
        &host.socket(),
        Command::Input {
            session: spec.session,
            generation: spec.generation,
            bytes: vec![b'\n'],
        },
    )?;
    host.wait_view(&spec, |view| {
        compact_frame(view).contains("AFTER_SNAPSHOT_MARKER")
    })?;
    let reread = history_reply(success(&host.socket(), Command::HistoryView { query })?)?;
    ensure!(matches!(reread,history_view::Reply::Grid{page} if page==first));
    let invalid = exchange(
        &host.socket(),
        Command::HistoryView {
            query: history_view::Query::Grid {
                snapshot: opened.snapshot,
                start: 0,
                rows: 128,
                cols: 1024,
            },
        },
    )?;
    ensure!(invalid.error.is_some());
    success(
        &host.socket(),
        Command::HistoryView {
            query: history_view::Query::Close {
                snapshot: opened.snapshot,
            },
        },
    )?;
    Ok(())
}
