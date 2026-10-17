//! Real-host coverage for project creation and bounded provider detection.
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{Command, PROTOCOL, Request, Response, receive, send};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct HostFixture {
    state: TempDir,
    bin: TempDir,
    child: Option<Child>,
}

impl HostFixture {
    fn new() -> Result<Self> {
        let state = tempfile::tempdir()?;
        let bin = tempfile::tempdir()?;
        let mut fixture = Self {
            state,
            bin,
            child: None,
        };
        fixture.write_cli("claude", "2.1.259")?;
        fixture.write_cli("codex", "0.162.1")?;
        fixture.write_cli("opencode", "fixture-1")?;
        fixture.start()?;
        Ok(fixture)
    }

    fn state(&self) -> &Path {
        self.state.path()
    }

    fn socket(&self) -> PathBuf {
        self.state().join("host.sock")
    }

    fn write_cli(&self, name: &str, version: &str) -> Result<()> {
        let path = self.bin.path().join(name);
        fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", version))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        let path =
            std::env::join_paths([self.bin.path(), Path::new("/usr/bin"), Path::new("/bin")])?;
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"));
        child
            .arg("serve")
            .arg(self.state())
            .env("PATH", path)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.state().join("host.log"))?);
        self.child = Some(child.spawn()?);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.as_mut().context("host missing")?.try_wait()? {
                bail!(
                    "host exited {status}: {}",
                    fs::read_to_string(self.state().join("host.log"))?
                );
            }
            if exchange(&self.socket(), Command::List).is_ok() {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "host did not expose its socket");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn restart(&mut self) -> Result<()> {
        self.stop()?;
        self.start()
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            child.kill()?;
            child.wait()?;
        }
        Ok(())
    }
}

impl Drop for HostFixture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn exchange(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
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
    if let Some(error) = response.error.as_deref() {
        bail!("host command failed: {error}");
    }
    Ok(response)
}

#[test]
fn create_project_is_git_initialized_and_persisted_across_host_restart() -> Result<()> {
    let mut host = HostFixture::new()?;
    let parent = host.state().join("selected-parent");
    fs::create_dir(&parent)?;

    let response = exchange(
        &host.socket(),
        Command::CreateProject {
            parent: parent.clone(),
            name: "BridgeMind".into(),
        },
    )?;
    let project = response
        .catalog
        .context("create_project did not return catalog")?
        .projects
        .into_iter()
        .next()
        .context("created project missing")?;
    let root = parent.join("BridgeMind");
    ensure!(project.canonical_path == fs::canonicalize(&root)?);
    ensure!(root.join(".git").is_dir());
    ensure!(project.name == "BridgeMind");
    ensure!(
        fs::read_to_string(root.join(".git/HEAD"))? == "ref: refs/heads/main\n",
        "new repository should use main as the initial branch"
    );
    ensure!(
        fs::read_dir(&root)?.count() == 1,
        "no additional files expected"
    );

    let before = fs::read(root.join(".git/HEAD"))?;
    let failed = exchange(
        &host.socket(),
        Command::CreateProject {
            parent: parent.clone(),
            name: "BridgeMind".into(),
        },
    );
    ensure!(failed.is_err(), "existing target must not be reused");
    ensure!(fs::read(root.join(".git/HEAD"))? == before);

    host.restart()?;
    let catalog = exchange(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing after host restart")?;
    ensure!(catalog.projects.iter().any(|entry| entry.id == project.id));
    Ok(())
}

#[test]
fn provider_detection_reports_installed_clis_without_starting_them() -> Result<()> {
    let host = HostFixture::new()?;
    let response = exchange(&host.socket(), Command::DetectProviders)?;
    let providers = response.providers.context("provider list missing")?;
    ensure!(
        providers.len() == 4,
        "expected the four supported CLI choices"
    );
    for expected in ["shell", "claude", "codex", "opencode"] {
        ensure!(
            providers
                .iter()
                .any(|provider| provider.provider == expected)
        );
    }
    for expected in ["claude", "codex", "opencode"] {
        let provider = providers
            .iter()
            .find(|provider| provider.provider == expected)
            .context("fixture provider missing")?;
        ensure!(provider.available, "fixture {expected} should be detected");
        ensure!(
            provider.version.is_some(),
            "fixture {expected} version missing"
        );
        ensure!(
            provider
                .executable
                .as_ref()
                .is_some_and(|path| path.starts_with(host.bin.path()))
        );
    }
    let shell = providers
        .iter()
        .find(|provider| provider.provider == "shell")
        .context("shell record missing")?;
    ensure!(shell.available && shell.executable.is_some());
    Ok(())
}

#[test]
fn create_project_rejects_path_components_before_creating_anything() -> Result<()> {
    let host = HostFixture::new()?;
    let parent = host.state().join("parent");
    fs::create_dir(&parent)?;
    let names = ["..", "../escape", "a/b", "line\nfeed"]
        .into_iter()
        .map(str::to_owned)
        .chain(["é".repeat(49)])
        .collect::<Vec<_>>();
    for name in names {
        let response = exchange(
            &host.socket(),
            Command::CreateProject {
                parent: parent.clone(),
                name: name.clone(),
            },
        );
        ensure!(
            response.is_err(),
            "unsafe name should be rejected: {name:?}"
        );
    }
    ensure!(fs::read_dir(&parent)?.next().is_none());
    ensure!(!host.state().join("escape").exists());
    Ok(())
}
