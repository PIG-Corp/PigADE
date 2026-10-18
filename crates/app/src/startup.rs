//! Local host discovery and startup for the Linux application.
//!
//! Explicit sockets are caller-owned and are never started or modified. The
//! default path reuses a healthy protocol-v2 host or starts the installed
//! session-host binary without cleaning up an existing endpoint.

use std::{
    env,
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::UnixStream,
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const PROTOCOL: u64 = 2;
const IPC_LIMIT: usize = 8 * 1024 * 1024;
const READY_TIMEOUT: Duration = Duration::from_secs(3);
const READY_POLL: Duration = Duration::from_millis(40);
const STOPPING_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_UNIX_SOCKET_PATH: usize = 107;

fn profile_name() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let version = version.strip_suffix("-dev").unwrap_or(version);
    format!("dev-{version}")
}

/// Return the chosen socket path, starting the default local host when needed.
/// An explicit path is returned untouched and never causes host startup.
pub(crate) fn resolve_socket(explicit_socket: Option<PathBuf>) -> Result<PathBuf, String> {
    let (socket, child) = resolve_socket_inner(explicit_socket, false)?;
    if let Some(child) = child {
        reap_in_background(child);
    }
    Ok(socket)
}

/// Compare only; explicit-socket ownership is separately retained by the UI.
pub(crate) fn is_default_socket(socket: &Path) -> Result<bool, String> {
    Ok(runtime_root(&state_root()?)?.join("host.sock") == socket)
}

fn resolve_socket_inner(
    explicit_socket: Option<PathBuf>,
    retain_started_child: bool,
) -> Result<(PathBuf, Option<Child>), String> {
    if let Some(socket) = explicit_socket {
        return Ok((socket, None));
    }

    let state_root = state_root()?;
    let runtime_root = runtime_root(&state_root)?;
    let state_parent = state_root.parent().ok_or("invalid state root")?;
    ensure_private_directory(state_parent)?;
    ensure_private_directory(&state_root)?;
    ensure_private_directory(&runtime_root)?;
    let socket = runtime_root.join("host.sock");
    if socket.as_os_str().as_bytes().len() > MAX_UNIX_SOCKET_PATH {
        return Err("host socket path is too long even after the state-directory fallback".into());
    }

    let prior_endpoint_error = match probe_existing_endpoint(&socket, READY_TIMEOUT)? {
        Endpoint::Healthy => return Ok((socket, None)),
        Endpoint::Stopping => match wait_for_stopping_endpoint(
            &socket,
            &runtime_root.join("endpoint.lock"),
            STOPPING_TIMEOUT,
        )? {
            StoppingResolution::Healthy => return Ok((socket, None)),
            StoppingResolution::Released => Some("the prior host completed shutdown".into()),
        },
        Endpoint::Unresponsive(reason) => Some(reason),
        Endpoint::Missing => match wait_for_missing_socket_lock(
            &socket,
            &runtime_root.join("endpoint.lock"),
            STOPPING_TIMEOUT,
        )? {
            Some(StoppingResolution::Healthy) => return Ok((socket, None)),
            Some(StoppingResolution::Released) => {
                // The endpoint was intentionally removed while the previous
                // host still owned endpoint.lock. Reprobe before launching.
                match probe_existing_endpoint(&socket, READY_TIMEOUT)? {
                    Endpoint::Healthy => return Ok((socket, None)),
                    Endpoint::Stopping => match wait_for_stopping_endpoint(
                        &socket,
                        &runtime_root.join("endpoint.lock"),
                        STOPPING_TIMEOUT,
                    )? {
                        StoppingResolution::Healthy => return Ok((socket, None)),
                        StoppingResolution::Released => {
                            Some("the prior host completed shutdown".into())
                        }
                    },
                    Endpoint::Unresponsive(reason) => Some(reason),
                    Endpoint::Missing => Some("the prior host completed shutdown".into()),
                }
            }
            None => None,
        },
    };

    let binary = resolve_host_binary()?;
    let log = open_private_log(&state_root.join("host.log"))?;
    let log_stderr = log
        .try_clone()
        .map_err(|error| format!("clone host log: {error}"))?;
    let mut child = Command::new(&binary)
        .arg("serve")
        .arg(&state_root)
        .arg("--runtime")
        .arg(&runtime_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_stderr))
        .process_group(0)
        .spawn()
        .map_err(|error| format!("start session host {}: {error}", binary.display()))?;

    let deadline = Instant::now() + READY_TIMEOUT;
    let mut exit_status = None;
    loop {
        let endpoint = match probe_existing_endpoint(&socket, Duration::from_millis(100)) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                reap_in_background(child);
                return Err(error);
            }
        };
        match endpoint {
            Endpoint::Healthy => {
                if retain_started_child {
                    return Ok((socket, Some(child)));
                }
                reap_in_background(child);
                return Ok((socket, None));
            }
            Endpoint::Stopping => (),
            // A newly bound socket may precede the fully initialized serve
            // loop. Continue bounded polling without ever replacing it.
            Endpoint::Unresponsive(_) => (),
            Endpoint::Missing if exit_status.is_some() => {
                return Err(format!(
                    "session host exited during startup with {}; log: {}",
                    exit_status.expect("checked above"),
                    state_root.join("host.log").display()
                ));
            }
            Endpoint::Missing => (),
        }

        if exit_status.is_none() {
            exit_status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    reap_in_background(child);
                    return Err(format!("check session host startup: {error}"));
                }
            };
        }
        if Instant::now() >= deadline {
            reap_in_background(child);
            return Err(match exit_status {
                Some(status) => format!(
                    "session host exited during startup with {status}; prior endpoint check: {}; log: {}",
                    prior_endpoint_error
                        .as_deref()
                        .unwrap_or("socket was absent"),
                    state_root.join("host.log").display(),
                ),
                None => format!(
                    "session host did not become ready within three seconds; prior endpoint check: {}; log: {}",
                    prior_endpoint_error
                        .as_deref()
                        .unwrap_or("socket was absent"),
                    state_root.join("host.log").display()
                ),
            });
        }
        thread::sleep(READY_POLL);
    }
}

enum Endpoint {
    Missing,
    Healthy,
    Stopping,
    Unresponsive(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoppingResolution {
    Released,
    Healthy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EndpointLockIdentity {
    dev: u64,
    ino: u64,
}

fn wait_for_stopping_endpoint(
    socket: &Path,
    endpoint_lock: &Path,
    timeout: Duration,
) -> Result<StoppingResolution, String> {
    let expected_lock = endpoint_lock_identity(endpoint_lock)?;
    let deadline = Instant::now() + timeout;
    loop {
        match probe_existing_endpoint(socket, Duration::from_millis(100))? {
            Endpoint::Healthy => return Ok(StoppingResolution::Healthy),
            Endpoint::Missing if endpoint_lock_available(endpoint_lock, expected_lock)? => {
                return Ok(StoppingResolution::Released);
            }
            Endpoint::Stopping | Endpoint::Unresponsive(_) | Endpoint::Missing => (),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "session host is still shutting down; its endpoint was preserved and no replacement was started: {}",
                socket.display()
            ));
        }
        thread::sleep(READY_POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// If shutdown has already unlinked host.sock but still holds endpoint.lock,
/// wait for that exact lock to be released before allowing a fresh launch.
/// An absent or already-released lock leaves ordinary startup unchanged.
fn wait_for_missing_socket_lock(
    socket: &Path,
    endpoint_lock: &Path,
    timeout: Duration,
) -> Result<Option<StoppingResolution>, String> {
    let expected_lock = match fs::symlink_metadata(endpoint_lock) {
        Ok(_) => endpoint_lock_identity(endpoint_lock)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "inspect endpoint ownership lock {}: {error}",
                endpoint_lock.display()
            ));
        }
    };
    if endpoint_lock_available(endpoint_lock, expected_lock)? {
        return Ok(None);
    }

    let deadline = Instant::now() + timeout;
    loop {
        if matches!(
            probe_existing_endpoint(socket, Duration::from_millis(100))?,
            Endpoint::Healthy
        ) {
            return Ok(Some(StoppingResolution::Healthy));
        }
        if endpoint_lock_available(endpoint_lock, expected_lock)? {
            return Ok(Some(StoppingResolution::Released));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "previous session host still owns endpoint.lock; no replacement was started: {}",
                socket.display()
            ));
        }
        thread::sleep(READY_POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn endpoint_lock_identity(path: &Path) -> Result<EndpointLockIdentity, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "inspect endpoint ownership lock {}: {error}",
            path.display()
        )
    })?;
    validate_endpoint_lock_metadata(path, &metadata)?;
    Ok(EndpointLockIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

fn validate_endpoint_lock_metadata(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file()
        || metadata.uid() != current_uid()?
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(format!(
            "endpoint ownership lock {} is not a private owned file; it was preserved",
            path.display()
        ));
    }
    Ok(())
}

fn endpoint_lock_available(path: &Path, expected: EndpointLockIdentity) -> Result<bool, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    let lock = match options.open(path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "open endpoint ownership lock {}: {error}",
                path.display()
            ));
        }
    };
    let metadata = lock.metadata().map_err(|error| {
        format!(
            "inspect endpoint ownership lock {}: {error}",
            path.display()
        )
    })?;
    validate_endpoint_lock_metadata(path, &metadata)?;
    if metadata.dev() != expected.dev || metadata.ino() != expected.ino {
        return Err(format!(
            "endpoint ownership lock {} changed during host shutdown; it was preserved",
            path.display()
        ));
    }
    match lock.try_lock() {
        Ok(()) => (),
        Err(TryLockError::WouldBlock) => return Ok(false),
        Err(TryLockError::Error(error)) => {
            return Err(format!(
                "probe endpoint ownership lock {}: {error}",
                path.display()
            ));
        }
    }
    let current = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "recheck endpoint ownership lock {}: {error}",
            path.display()
        )
    })?;
    validate_endpoint_lock_metadata(path, &current)?;
    if current.dev() != expected.dev || current.ino() != expected.ino {
        let _ = lock.unlock();
        return Err(format!(
            "endpoint ownership lock {} changed during host shutdown; it was preserved",
            path.display()
        ));
    }
    if let Err(error) = lock.unlock() {
        return Err(format!(
            "release endpoint ownership probe {}: {error}",
            path.display(),
        ));
    }
    Ok(true)
}

fn probe_existing_endpoint(socket: &Path, timeout: Duration) -> Result<Endpoint, String> {
    let metadata = match fs::symlink_metadata(socket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Endpoint::Missing),
        Err(error) => {
            return Err(format!(
                "inspect host endpoint {}: {error}",
                socket.display()
            ));
        }
    };
    if !metadata.file_type().is_socket()
        || metadata.uid() != current_uid()?
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "existing host endpoint {} is not an owned private socket; it was preserved",
            socket.display()
        ));
    }
    match query_catalog(socket, timeout) {
        Ok(CatalogProbe::Healthy) => Ok(Endpoint::Healthy),
        Ok(CatalogProbe::Stopping) => Ok(Endpoint::Stopping),
        Err(error) => Ok(Endpoint::Unresponsive(format!(
            "existing host endpoint {} did not answer a protocol-v2 catalog request; it was preserved: {error}",
            socket.display()
        ))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogProbe {
    Healthy,
    Stopping,
}

fn query_catalog(socket: &Path, timeout: Duration) -> Result<CatalogProbe, String> {
    let mut stream = UnixStream::connect(socket).map_err(|error| format!("connect: {error}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("set read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| format!("set write timeout: {error}"))?;
    let request = serde_json::to_vec(&serde_json::json!({
        "protocol": PROTOCOL,
        "command": { "command": "catalog" }
    }))
    .map_err(|error| format!("encode catalog request: {error}"))?;
    stream
        .write_all(&(request.len() as u32).to_le_bytes())
        .and_then(|_| stream.write_all(&request))
        .map_err(|error| format!("write catalog request: {error}"))?;

    let mut length = [0; 4];
    stream
        .read_exact(&mut length)
        .map_err(|error| format!("read catalog response header: {error}"))?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > IPC_LIMIT {
        return Err("invalid catalog response length".into());
    }
    let mut payload = vec![0; length];
    stream
        .read_exact(&mut payload)
        .map_err(|error| format!("read catalog response: {error}"))?;
    let response: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|error| format!("decode catalog response: {error}"))?;
    if response.get("protocol").and_then(serde_json::Value::as_u64) == Some(PROTOCOL)
        && response
            .get("error")
            .and_then(serde_json::Value::as_str)
            .is_some()
        && response
            .get("rejected_before_admission")
            .and_then(serde_json::Value::as_str)
            == Some("host_shutting_down")
    {
        return Ok(CatalogProbe::Stopping);
    }
    if response.get("protocol").and_then(serde_json::Value::as_u64) != Some(PROTOCOL)
        || !response.get("error").is_none_or(serde_json::Value::is_null)
        || !response
            .get("catalog")
            .is_some_and(serde_json::Value::is_object)
    {
        return Err("host response is not a successful protocol-v2 catalog".into());
    }
    Ok(CatalogProbe::Healthy)
}

fn state_root() -> Result<PathBuf, String> {
    let state_home = match env::var_os("XDG_STATE_HOME") {
        Some(path) if !path.is_empty() => absolute_path(PathBuf::from(path), "XDG_STATE_HOME")?,
        _ => {
            let home = env::var_os("HOME").ok_or("HOME is unavailable")?;
            absolute_path(PathBuf::from(home).join(".local").join("state"), "HOME")?
        }
    };
    Ok(state_home.join("pigade").join(profile_name()))
}

fn runtime_root(state_root: &Path) -> Result<PathBuf, String> {
    let candidate = match env::var_os("XDG_RUNTIME_DIR") {
        Some(path) if !path.is_empty() => absolute_path(PathBuf::from(path), "XDG_RUNTIME_DIR")?
            .join(format!("pigade-{}", profile_name())),
        _ => state_root.to_path_buf(),
    };
    if candidate.join("host.sock").as_os_str().as_bytes().len() <= MAX_UNIX_SOCKET_PATH {
        return Ok(candidate);
    }
    if state_root.join("host.sock").as_os_str().as_bytes().len() <= MAX_UNIX_SOCKET_PATH {
        return Ok(state_root.to_path_buf());
    }
    Err("host socket path is too long in both runtime and state directories".into())
}

fn absolute_path(path: PathBuf, name: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{name} must be an absolute path"));
    }
    Ok(path)
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create parent {}: {error}", parent.display()))?;
    }
    match fs::symlink_metadata(path) {
        Ok(_) => validate_private_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(path) {
                Ok(()) => validate_private_directory(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    validate_private_directory(path)
                }
                Err(error) => Err(format!(
                    "create private directory {}: {error}",
                    path.display()
                )),
            }
        }
        Err(error) => Err(format!("inspect directory {}: {error}", path.display())),
    }
}

fn validate_private_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect directory {}: {error}", path.display()))?;
    if !metadata.is_dir()
        || metadata.uid() != current_uid()?
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(format!(
            "directory {} must be owned by this user, a real directory, and mode 0700; existing permissions were preserved",
            path.display()
        ));
    }
    Ok(())
}

fn open_private_log(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.append(true).write(true).mode(0o600);
    let file = match fs::symlink_metadata(path) {
        Ok(before) => {
            validate_log_metadata(path, &before)?;
            let file = options
                .open(path)
                .map_err(|error| format!("open host log {}: {error}", path.display()))?;
            let after = file
                .metadata()
                .map_err(|error| format!("inspect host log {}: {error}", path.display()))?;
            if before.dev() != after.dev() || before.ino() != after.ino() {
                return Err(format!("host log {} changed while opening", path.display()));
            }
            file
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => options
            .create_new(true)
            .open(path)
            .map_err(|error| format!("create private host log {}: {error}", path.display()))?,
        Err(error) => return Err(format!("inspect host log {}: {error}", path.display())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect host log {}: {error}", path.display()))?;
    validate_log_metadata(path, &metadata)?;
    Ok(file)
}

fn validate_log_metadata(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file()
        || metadata.uid() != current_uid()?
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(format!(
            "host log {} must be an owned regular 0600 file; existing permissions were preserved",
            path.display()
        ));
    }
    Ok(())
}

pub(crate) fn resolve_host_binary() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("PIGADE_HOST_BINARY") {
        return executable(PathBuf::from(path))
            .ok_or_else(|| "PIGADE_HOST_BINARY does not name an executable regular file".into());
    }
    let mut candidates = Vec::new();
    if let Ok(current) = env::current_exe()
        && let Some(parent) = current.parent()
    {
        candidates.push(parent.join("pigade-session-host"));
    }
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    candidates.push(repository.join("target/release/pigade-session-host"));
    candidates.push(repository.join("target/debug/pigade-session-host"));
    candidates
        .into_iter()
        .find_map(executable)
        .ok_or_else(|| "pigade-session-host executable was not found beside the app or in the development target directories".into())
}

fn executable(path: PathBuf) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    let metadata = path.metadata().ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(path)
}

fn current_uid() -> Result<u32, String> {
    fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .map_err(|error| format!("read current user identity: {error}"))
}

fn reap_in_background(mut child: Child) {
    let _ = thread::Builder::new()
        .name("pigade-host-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    use std::process::Command as ProcessCommand;

    struct OwnedChild(Option<Child>);

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    struct PrivateTree(PathBuf);

    impl Drop for PrivateTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn startup_child_entry() -> Result<(), String> {
        if env::var_os("PIGADE_STARTUP_TEST_CHILD").is_none() {
            return Ok(());
        }
        let runtime_home = env::var_os("XDG_RUNTIME_DIR").ok_or("missing test runtime home")?;
        let runtime = PathBuf::from(runtime_home).join(format!("pigade-{}", profile_name()));
        ensure_private_directory(&runtime)?;
        let stale_socket = runtime.join("host.sock");
        let listener = UnixListener::bind(&stale_socket)
            .map_err(|error| format!("create synthetic stale socket: {error}"))?;
        drop(listener);
        fs::set_permissions(&stale_socket, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("secure synthetic stale socket: {error}"))?;

        let (socket, host) = resolve_socket_inner(None, true)?;
        let mut host = OwnedChild(Some(
            host.ok_or("startup did not return its owned host child")?,
        ));
        let (reused_socket, second_child) = resolve_socket_inner(None, false)?;
        if socket != reused_socket || second_child.is_some() {
            return Err("second resolution did not reuse the running host".into());
        }
        let sessions = match crate::protocol::product_request(
            &socket,
            serde_json::json!({ "command": "list" }),
        ) {
            Ok(response) => response,
            Err(error) => return Err(format!("query the started host: {}", error.message)),
        };
        if sessions.sessions.is_none_or(|views| !views.is_empty()) {
            return Err("fresh host unexpectedly has CLI sessions".into());
        }
        if host
            .0
            .as_mut()
            .expect("owned host child")
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("owned host exited before cleanup".into());
        }
        let child = host.0.as_mut().expect("owned host child");
        child.kill().map_err(|error| error.to_string())?;
        child.wait().map_err(|error| error.to_string())?;
        host.0.take();
        Ok(())
    }

    #[test]
    fn stopping_host_marker_waits_and_preserves_unresponsive_endpoint() {
        let directory = env::temp_dir().join(format!("pigade-stopping-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&directory).expect("private test directory");
        let _cleanup = PrivateTree(directory.clone());
        let socket = directory.join("host.sock");
        let endpoint_lock = directory.join("endpoint.lock");
        let listener = UnixListener::bind(&socket).expect("fake private listener");
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&endpoint_lock)
            .expect("private endpoint lock file");

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("catalog probe connection");
            let mut length = [0; 4];
            stream.read_exact(&mut length).expect("read request length");
            let mut request = vec![0; u32::from_le_bytes(length) as usize];
            stream.read_exact(&mut request).expect("read request");
            let payload = serde_json::to_vec(&serde_json::json!({
                "protocol": PROTOCOL,
                "error": "host is shutting down",
                "rejected_before_admission": "host_shutting_down"
            }))
            .expect("encode fake host response");
            stream
                .write_all(&(payload.len() as u32).to_le_bytes())
                .and_then(|()| stream.write_all(&payload))
                .expect("write fake host response");
            drop(stream);
            drop(listener);
        });

        assert_eq!(
            query_catalog(&socket, Duration::from_secs(1)).expect("stopping marker probe"),
            CatalogProbe::Stopping
        );
        server.join().expect("fake host thread");
        let error = wait_for_stopping_endpoint(&socket, &endpoint_lock, Duration::from_millis(120))
            .expect_err("a present stopping endpoint must not start a replacement");
        assert!(error.contains("no replacement was started"));
        assert!(fs::symlink_metadata(&socket).is_ok());
        drop(lock);
    }

    #[test]
    fn stopping_wait_requires_endpoint_lock_release_after_socket_disappears() {
        let directory = env::temp_dir().join(format!("pigade-lock-probe-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&directory).expect("private test directory");
        let _cleanup = PrivateTree(directory.clone());
        let lock_path = directory.join("endpoint.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock_path)
            .expect("private endpoint lock");
        let lock_identity = endpoint_lock_identity(&lock_path).expect("endpoint lock identity");
        lock.try_lock().expect("acquire test endpoint lock");
        assert!(
            !endpoint_lock_available(&lock_path, lock_identity).expect("locked endpoint probe")
        );
        lock.unlock().expect("release test endpoint lock");
        assert!(
            endpoint_lock_available(&lock_path, lock_identity).expect("released endpoint probe")
        );
    }

    #[test]
    fn missing_socket_waits_for_held_shutdown_lock_before_startup() {
        let directory =
            env::temp_dir().join(format!("pigade-release-wait-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&directory).expect("private test directory");
        let _cleanup = PrivateTree(directory.clone());
        let socket = directory.join("host.sock");
        let lock_path = directory.join("endpoint.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock_path)
            .expect("private endpoint lock");
        lock.try_lock().expect("acquire test endpoint lock");
        let unlock = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            lock.unlock().expect("release test endpoint lock");
        });
        assert_eq!(
            wait_for_missing_socket_lock(&socket, &lock_path, Duration::from_secs(1))
                .expect("bounded wait for lock release"),
            Some(StoppingResolution::Released)
        );
        unlock.join().expect("unlock test endpoint");
        assert!(!socket.exists());
    }

    #[test]
    fn default_startup_launches_and_reuses_release_host_without_cli_sessions() {
        let host_binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/release/pigade-session-host")
            .canonicalize()
            .expect("fresh release host binary");
        let directory = env::temp_dir().join(format!("pigade-startup-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&directory).expect("private test parent");
        let _cleanup = PrivateTree(directory.clone());
        let state_home = directory.join("state-home");
        let runtime_home = directory.join("runtime-home");
        let test_binary = env::current_exe().expect("test executable");
        let output = ProcessCommand::new(test_binary)
            .arg("--exact")
            .arg("startup::tests::startup_child_entry")
            .arg("--nocapture")
            .env("PIGADE_STARTUP_TEST_CHILD", "1")
            .env("PIGADE_HOST_BINARY", host_binary)
            .env("XDG_STATE_HOME", state_home)
            .env("XDG_RUNTIME_DIR", runtime_home)
            .output()
            .expect("spawn isolated startup test subprocess");
        assert!(
            output.status.success(),
            "startup child failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
