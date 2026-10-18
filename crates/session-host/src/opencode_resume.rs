//! Exact-ID OpenCode resume metadata preflight.
//!
//! OpenCode's TUI routes session requests using the session's stored directory,
//! so the process cwd alone does not prove where a resumed session will run.
//! This module starts a short-lived loopback-only server and reads only the
//! public `GET /session/:id` metadata endpoint before a resume is allowed.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use uuid::Uuid;

const OPENCODE_READY_PREFIX: &str = "opencode server listening on http://127.0.0.1:";
const DEADLINE: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 64 * 1024;
const HTTP_HEADER_LIMIT: usize = 16 * 1024;
const HTTP_BODY_LIMIT: usize = 64 * 1024;
const HTTP_WIRE_LIMIT: usize = HTTP_HEADER_LIMIT + HTTP_BODY_LIMIT * 2;

/// Verify that `conversation_id` resolves to a local OpenCode session whose
/// persisted working directory is exactly `cwd` and which is not workspace-
/// routed. This function blocks and must only run on the host's probe worker.
pub(crate) fn verify(executable: &Path, cwd: &Path, conversation_id: &str) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (executable, cwd, conversation_id);
        bail!("OpenCode metadata preflight requires a Unix process group")
    }

    #[cfg(target_os = "linux")]
    {
        ensure!(
            crate::adapters::is_opencode_session_id(conversation_id),
            "invalid OpenCode conversation ID"
        );
        let expected_directory = std::fs::canonicalize(cwd).with_context(|| {
            format!(
                "cannot resolve OpenCode working directory: {}",
                cwd.display()
            )
        })?;
        ensure!(
            expected_directory.is_dir(),
            "OpenCode working directory is unavailable"
        );
        let expected_directory_text = expected_directory
            .to_str()
            .context("OpenCode working directory is not valid UTF-8")?;

        let credential = format!("pigade-{}", Uuid::new_v4());
        let password = Uuid::new_v4().to_string();
        let authorization = format!(
            "Basic {}",
            base64_encode(format!("{credential}:{password}").as_bytes())
        );
        // A set workspace override makes the v1.19.3 routing middleware stay
        // on its local request plan instead of resolving/proxying the stored
        // session workspace. The GET response still carries the persisted
        // workspaceID, which this probe rejects below. `wrk` is the exact
        // WorkspaceV2.ID prefix in the supported source version.
        let local_route_id = format!("wrk_pigade_{}", Uuid::new_v4().simple());
        let mut command = Command::new(executable);
        command
            .args([
                OsStr::new("serve"),
                OsStr::new("--pure"),
                OsStr::new("--hostname"),
                OsStr::new("127.0.0.1"),
                OsStr::new("--port"),
                OsStr::new("0"),
                OsStr::new("--no-mdns"),
            ])
            .current_dir(&expected_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_child_environment(&mut command, &credential, &password, &local_route_id);
        set_process_group(&mut command);

        let mut child = command
            .spawn()
            .context("cannot start OpenCode metadata server")?;
        let deadline = Instant::now() + DEADLINE;
        let result = (|| {
            let port = wait_for_server(&mut child, deadline)?;
            let body = get_session_metadata(
                port,
                expected_directory_text,
                conversation_id,
                &authorization,
                deadline,
            )?;
            validate_session_metadata(&body, conversation_id, &expected_directory)
        })();
        terminate_process_group(&mut child);
        result
    }
}

#[cfg(target_os = "linux")]
fn configure_child_environment(
    command: &mut Command,
    username: &str,
    password: &str,
    local_route_id: &str,
) {
    // Do not forward provider credentials, arbitrary OpenCode flags, workspace
    // routing overrides, custom config contents, or server overrides. Keep
    // only the native state/config roots needed to inspect the same local DB.
    command.env_clear();
    for key in [
        "PATH",
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_DB",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("OPENCODE_SERVER_USERNAME", username)
        .env("OPENCODE_SERVER_PASSWORD", password)
        .env("OPENCODE_WORKSPACE_ID", local_route_id);
}

#[cfg(target_os = "linux")]
fn set_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    let parent = unsafe { libc::getpid() };
    command.process_group(0);
    // SAFETY: the hook uses only async-signal-safe syscalls after fork. The
    // parent check closes the race where this process exits just before the
    // kernel installs the parent-death signal.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(127);
            }
            Ok(())
        });
    }
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct CapturedOutput {
    bytes: Vec<u8>,
    closed: bool,
}

#[cfg(target_os = "linux")]
fn wait_for_server(child: &mut Child, deadline: Instant) -> Result<u16> {
    use std::os::fd::AsRawFd;

    let stdout = child
        .stdout
        .as_ref()
        .context("OpenCode stdout unavailable")?;
    let stderr = child
        .stderr
        .as_ref()
        .context("OpenCode stderr unavailable")?;
    let stdout_fd = stdout.as_raw_fd();
    let stderr_fd = stderr.as_raw_fd();
    set_nonblocking(stdout_fd)?;
    set_nonblocking(stderr_fd)?;
    let mut out = CapturedOutput::default();
    let mut err = CapturedOutput::default();

    loop {
        drain_fd(stdout_fd, &mut out)?;
        drain_fd(stderr_fd, &mut err)?;
        if let Some(port) = readiness_port(&out.bytes) {
            return Ok(port);
        }
        if child_exited_unreaped(child)? {
            bail!("OpenCode metadata server exited before readiness");
        }
        let now = Instant::now();
        if now >= deadline {
            bail!("OpenCode metadata server startup timed out");
        }
        let millis = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(20))
            .as_millis()
            .max(1) as i32;
        let mut fds = [
            libc::pollfd {
                fd: if out.closed { -1 } else { stdout_fd },
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            },
            libc::pollfd {
                fd: if err.closed { -1 } else { stderr_fd },
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            },
        ];
        // SAFETY: `fds` points to two initialized pollfd values for the call.
        let polled = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
        if polled < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error).context("cannot wait for OpenCode metadata server");
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn child_exited_unreaped(child: &Child) -> Result<bool> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // WNOWAIT observes the exact direct child without reaping it. Keeping the
    // group leader unreaped prevents PID/PGID reuse before teardown kills any
    // descendants which inherited the captured pipe descriptors.
    // SAFETY: `info` is a valid writable siginfo_t and the PID is our child.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error()).context("cannot inspect OpenCode server exit");
    }
    // SAFETY: successful waitid initialized the siginfo_t value.
    let info = unsafe { info.assume_init() };
    // SAFETY: waitid initialized this siginfo_t; si_pid is valid for WEXITED.
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(target_os = "linux")]
fn set_nonblocking(fd: std::os::fd::RawFd) -> Result<()> {
    // SAFETY: `fd` is an open pipe descriptor owned by the spawned child.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error()).context("cannot inspect OpenCode output pipe");
    }
    // SAFETY: `fd` is valid and the new status flag preserves existing bits.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error()).context("cannot configure OpenCode output pipe");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn drain_fd(fd: std::os::fd::RawFd, capture: &mut CapturedOutput) -> Result<()> {
    if capture.closed {
        return Ok(());
    }
    let mut buffer = [0_u8; 4096];
    loop {
        // SAFETY: `buffer` is writable for its full length and `fd` remains
        // owned by ChildStdout/ChildStderr while this function runs.
        let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count == 0 {
            capture.closed = true;
            return Ok(());
        }
        if count < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::WouldBlock => return Ok(()),
                io::ErrorKind::Interrupted => continue,
                _ => return Err(error).context("cannot read OpenCode server output"),
            }
        }
        let count = count as usize;
        let available = OUTPUT_LIMIT.saturating_sub(capture.bytes.len());
        ensure!(
            count <= available,
            "OpenCode metadata server output exceeded its bound"
        );
        capture.bytes.extend_from_slice(&buffer[..count]);
    }
}

#[cfg(target_os = "linux")]
fn readiness_port(output: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(output).ok()?;
    text.lines().find_map(|line| {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let port = line.strip_prefix(OPENCODE_READY_PREFIX)?;
        let port = port.parse::<u16>().ok()?;
        (port != 0).then_some(port)
    })
}

#[cfg(target_os = "linux")]
fn terminate_process_group(child: &mut Child) {
    let group = child.id() as libc::pid_t;
    if group > 0 {
        // SAFETY: the child was started in a new process group whose ID is its
        // PID. The child remains unreaped until the wait below, preventing PID
        // reuse while the exact owned group is terminated.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_os = "linux")]
fn get_session_metadata(
    port: u16,
    directory: &str,
    session_id: &str,
    authorization: &str,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let address = ("127.0.0.1", port)
        .to_socket_addrs()
        .context("cannot resolve loopback metadata server")?
        .next()
        .context("loopback metadata server has no address")?;
    let timeout = deadline.saturating_duration_since(Instant::now());
    ensure!(!timeout.is_zero(), "OpenCode metadata request timed out");
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .context("cannot connect to OpenCode metadata server")?;
    stream
        .set_read_timeout(Some(timeout))
        .context("cannot set OpenCode metadata read deadline")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("cannot set OpenCode metadata write deadline")?;

    let path = format!(
        "/session/{}?directory={}",
        encode_path_segment(session_id),
        encode_query(directory)
    );
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: {authorization}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .context("cannot send OpenCode metadata request")?;

    let mut response = Vec::with_capacity(4096);
    let mut buffer = [0_u8; 4096];
    loop {
        ensure!(
            Instant::now() < deadline,
            "OpenCode metadata request timed out"
        );
        stream
            .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .context("cannot update OpenCode metadata read deadline")?;
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                ensure!(
                    response.len().saturating_add(count) <= HTTP_WIRE_LIMIT,
                    "OpenCode metadata response exceeds the size limit"
                );
                response.extend_from_slice(&buffer[..count]);
                if let Some(parsed) = parse_http_response(&response, false)? {
                    ensure!(
                        parsed.status == 200,
                        "OpenCode session metadata request was rejected"
                    );
                    return Ok(parsed.body);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                bail!("OpenCode metadata request timed out")
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("cannot read OpenCode metadata response"),
        }
    }
    let parsed =
        parse_http_response(&response, true)?.context("incomplete OpenCode metadata response")?;
    ensure!(
        parsed.status == 200,
        "OpenCode session metadata request was rejected"
    );
    Ok(parsed.body)
}

#[cfg(target_os = "linux")]
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

#[cfg(target_os = "linux")]
fn parse_http_response(bytes: &[u8], eof: bool) -> Result<Option<HttpResponse>> {
    let Some(header_end) = find_bytes(bytes, b"\r\n\r\n") else {
        ensure!(
            bytes.len() <= HTTP_HEADER_LIMIT,
            "OpenCode metadata headers exceed the size limit"
        );
        return Ok(None);
    };
    ensure!(
        header_end <= HTTP_HEADER_LIMIT,
        "OpenCode metadata headers exceed the size limit"
    );
    let header_text = std::str::from_utf8(&bytes[..header_end])
        .context("OpenCode metadata response has invalid headers")?;
    let mut lines = header_text.split("\r\n");
    let status_line = lines
        .next()
        .context("OpenCode metadata response has no status")?;
    let mut status_fields = status_line.split_ascii_whitespace();
    ensure!(
        status_fields
            .next()
            .is_some_and(|protocol| protocol.starts_with("HTTP/1.")),
        "OpenCode metadata response has an invalid protocol"
    );
    let status = status_fields
        .next()
        .context("OpenCode metadata response has no status code")?
        .parse::<u16>()
        .context("OpenCode metadata response has an invalid status code")?;

    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .context("OpenCode metadata response has a malformed header")?;
        if name.eq_ignore_ascii_case("content-length") {
            ensure!(
                content_length.is_none(),
                "duplicate OpenCode metadata content length"
            );
            let length = value
                .trim()
                .parse::<usize>()
                .context("invalid OpenCode metadata content length")?;
            ensure!(
                length <= HTTP_BODY_LIMIT,
                "OpenCode metadata body exceeds the size limit"
            );
            content_length = Some(length);
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("chunked"));
        }
    }
    ensure!(
        !(content_length.is_some() && chunked),
        "ambiguous OpenCode metadata framing"
    );
    let body = &bytes[header_end + 4..];
    if let Some(length) = content_length {
        if body.len() < length {
            return Ok(None);
        }
        ensure!(
            body.len() == length,
            "extra bytes after OpenCode metadata response"
        );
        return Ok(Some(HttpResponse {
            status,
            body: body.to_vec(),
        }));
    }
    if chunked {
        return Ok(decode_chunked(body)?.map(|body| HttpResponse { status, body }));
    }
    if eof {
        ensure!(
            body.len() <= HTTP_BODY_LIMIT,
            "OpenCode metadata body exceeds the size limit"
        );
        return Ok(Some(HttpResponse {
            status,
            body: body.to_vec(),
        }));
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
fn decode_chunked(input: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut cursor = 0;
    let mut output = Vec::new();
    loop {
        let Some(line_end) = find_bytes(&input[cursor..], b"\r\n") else {
            ensure!(
                cursor <= HTTP_WIRE_LIMIT,
                "OpenCode chunk framing exceeds the size limit"
            );
            return Ok(None);
        };
        let line_end = cursor + line_end;
        let size_text = std::str::from_utf8(&input[cursor..line_end])
            .context("invalid OpenCode chunk size")?
            .split(';')
            .next()
            .unwrap_or_default();
        let size =
            usize::from_str_radix(size_text.trim(), 16).context("invalid OpenCode chunk size")?;
        cursor = line_end + 2;
        if size == 0 {
            if input.get(cursor..cursor + 2) == Some(b"\r\n") {
                return Ok(Some(output));
            }
            let Some(_trailers_end) = find_bytes(&input[cursor..], b"\r\n\r\n") else {
                return Ok(None);
            };
            return Ok(Some(output));
        }
        ensure!(
            output.len().saturating_add(size) <= HTTP_BODY_LIMIT,
            "OpenCode metadata body exceeds the size limit"
        );
        let Some(chunk_end) = cursor.checked_add(size).and_then(|end| end.checked_add(2)) else {
            bail!("invalid OpenCode chunk size");
        };
        if input.len() < chunk_end {
            return Ok(None);
        }
        ensure!(
            &input[chunk_end - 2..chunk_end] == b"\r\n",
            "invalid OpenCode chunk terminator"
        );
        output.extend_from_slice(&input[cursor..cursor + size]);
        cursor = chunk_end;
    }
}

#[cfg(target_os = "linux")]
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(target_os = "linux")]
fn validate_session_metadata(
    body: &[u8],
    expected_id: &str,
    expected_directory: &Path,
) -> Result<()> {
    ensure!(
        body.len() <= HTTP_BODY_LIMIT,
        "OpenCode session metadata exceeds the size limit"
    );
    let value: serde_json::Value =
        serde_json::from_slice(body).context("invalid OpenCode session metadata")?;
    let object = value
        .as_object()
        .context("OpenCode session metadata is not an object")?;
    ensure!(
        object.get("id").and_then(serde_json::Value::as_str) == Some(expected_id),
        "OpenCode returned metadata for a different session"
    );
    let directory = object
        .get("directory")
        .and_then(serde_json::Value::as_str)
        .context("OpenCode session metadata has no directory")?;
    let stored_directory =
        std::fs::canonicalize(directory).context("OpenCode session directory is unavailable")?;
    ensure!(
        stored_directory == expected_directory,
        "OpenCode session directory does not match the selected workspace"
    );
    match object.get("workspaceID") {
        None | Some(serde_json::Value::Null) => Ok(()),
        Some(serde_json::Value::String(value)) if value.is_empty() => {
            bail!("OpenCode session has an invalid workspace routing ID")
        }
        Some(serde_json::Value::String(_)) => {
            bail!("OpenCode session uses a workspace route that cannot be verified")
        }
        Some(_) => bail!("OpenCode session has invalid workspace routing metadata"),
    }
}

#[cfg(target_os = "linux")]
fn encode_path_segment(value: &str) -> String {
    percent_encode(value.as_bytes(), false)
}

#[cfg(target_os = "linux")]
fn encode_query(value: &str) -> String {
    percent_encode(value.as_bytes(), true)
}

#[cfg(target_os = "linux")]
fn percent_encode(value: &[u8], slash_allowed: bool) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value {
        if byte.is_ascii_alphanumeric()
            || matches!(*byte, b'-' | b'_' | b'.' | b'~')
            || (slash_allowed && *byte == b'/')
        {
            output.push(*byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(output, "%{byte:02X}");
        }
    }
    output
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    #[test]
    fn basic_encoding_uses_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn exact_ready_line_is_required() {
        assert_eq!(
            readiness_port(b"opencode server listening on http://127.0.0.1:41923\n"),
            Some(41923)
        );
        assert_eq!(
            readiness_port(b"opencode server listening on http://0.0.0.0:41923\n"),
            None
        );
        assert_eq!(
            readiness_port(b"opencode server listening on http://127.0.0.1:0\n"),
            None
        );
    }

    #[test]
    fn fixed_length_and_chunked_http_are_bounded_and_decoded() {
        let body = br#"{"id":"ses_test","directory":"/tmp","workspaceID":null}"#;
        let fixed = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        assert_eq!(
            parse_http_response(fixed.as_bytes(), false)
                .unwrap()
                .unwrap()
                .body,
            body
        );

        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ntest\r\n0\r\n\r\n";
        assert_eq!(
            parse_http_response(chunked, false).unwrap().unwrap().body,
            b"test"
        );
        assert!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 70000\r\n\r\n", false)
                .is_err()
        );
    }

    #[test]
    fn metadata_requires_exact_id_directory_and_local_route() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().canonicalize().unwrap();
        let metadata = |id: &str, path: &Path, workspace: serde_json::Value| {
            serde_json::json!({
                "id": id,
                "directory": path,
                "workspaceID": workspace,
            })
            .to_string()
        };
        assert!(
            validate_session_metadata(
                metadata("ses_expected", &directory, serde_json::Value::Null).as_bytes(),
                "ses_expected",
                &directory,
            )
            .is_ok()
        );
        assert!(
            validate_session_metadata(
                metadata("ses_other", &directory, serde_json::Value::Null).as_bytes(),
                "ses_expected",
                &directory,
            )
            .is_err()
        );
        assert!(
            validate_session_metadata(
                metadata("ses_expected", Path::new("/"), serde_json::Value::Null).as_bytes(),
                "ses_expected",
                &directory,
            )
            .is_err()
        );
        assert!(
            validate_session_metadata(
                metadata(
                    "ses_expected",
                    &directory,
                    serde_json::json!("workspace_remote")
                )
                .as_bytes(),
                "ses_expected",
                &directory,
            )
            .is_err()
        );
    }

    fn fake_executable(
        temp: &tempfile::TempDir,
        session: &str,
        directory: &str,
        workspace: Option<&str>,
    ) -> PathBuf {
        let executable = temp.path().join("opencode-fake");
        let workspace_value =
            workspace.map_or_else(|| "None".to_owned(), |value| format!("{value:?}"));
        let script = format!(
            "#!/usr/bin/python3\nimport base64,json,os,socket\ns=socket.socket()\ns.bind(('127.0.0.1',0))\ns.listen(1)\nprint('opencode server listening on http://127.0.0.1:%d' % s.getsockname()[1], flush=True)\nc,_=s.accept()\ndata=c.recv(8192).decode('ascii','replace')\nexpected=base64.b64encode((os.environ['OPENCODE_SERVER_USERNAME']+':'+os.environ['OPENCODE_SERVER_PASSWORD']).encode()).decode()\nassert 'Authorization: Basic '+expected in data\nassert os.environ['OPENCODE_WORKSPACE_ID'].startswith('wrk_pigade_')\nassert 'ANTHROPIC_API_KEY' not in os.environ and 'OPENAI_API_KEY' not in os.environ\nbody=json.dumps({{'id':{session:?},'directory':{directory:?},'workspaceID':{workspace_value}}}).encode()\nc.sendall(b'HTTP/1.1 200 OK\\r\\nContent-Type: application/json\\r\\nContent-Length: '+str(len(body)).encode()+b'\\r\\nConnection: close\\r\\n\\r\\n'+body)\nc.close()\ns.close()\n"
        );
        fs::write(&executable, script).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).unwrap();
        executable
    }

    #[test]
    fn preflight_uses_authenticated_loopback_metadata_and_rejects_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let executable = fake_executable(&temp, "ses_exact", cwd.to_str().unwrap(), None);
        verify(&executable, &cwd, "ses_exact").unwrap();

        let mismatched = fake_executable(&temp, "ses_exact", "/", None);
        assert!(verify(&mismatched, &cwd, "ses_exact").is_err());

        let routed = fake_executable(
            &temp,
            "ses_exact",
            cwd.to_str().unwrap(),
            Some("workspace-id"),
        );
        assert!(verify(&routed, &cwd, "ses_exact").is_err());
    }

    #[test]
    fn continuous_probe_output_hits_the_bound_instead_of_starving_its_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("opencode-noisy-fake");
        fs::write(
            &executable,
            "#!/usr/bin/python3\nimport os\nwhile True: os.write(1,b'x'*8192)\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let error = verify(&executable, temp.path(), "ses_exact").unwrap_err();
        assert!(error.to_string().contains("output exceeded"), "{error:#}");
    }

    #[test]
    fn parent_death_signal_cleans_temporary_server_process() {
        if let Some(executable) = std::env::var_os("PIGADE_PDEATH_EXECUTABLE") {
            let cwd = PathBuf::from(std::env::var_os("PIGADE_PDEATH_CWD").unwrap());
            let _ = verify(Path::new(&executable), &cwd, "ses_exact");
            panic!("parent fixture should be terminated during metadata GET");
        }

        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let pid_file = temp.path().join("server.pid");
        let executable = temp.path().join("opencode-hanging-fake");
        let script = format!(
            "#!/usr/bin/python3\nimport os,socket,time\ns=socket.socket()\ns.bind(('127.0.0.1',0))\ns.listen(1)\nopen({:?},'w').write(str(os.getpid()))\nprint('opencode server listening on http://127.0.0.1:%d' % s.getsockname()[1], flush=True)\nc,_=s.accept()\ntime.sleep(30)\n",
            pid_file.to_string_lossy()
        );
        fs::write(&executable, script).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).unwrap();

        let mut parent = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "opencode_resume::tests::parent_death_signal_cleans_temporary_server_process",
                "--nocapture",
            ])
            .env_clear()
            .env("PIGADE_PDEATH_EXECUTABLE", &executable)
            .env("PIGADE_PDEATH_CWD", &cwd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let until = Instant::now() + Duration::from_secs(2);
        let server_pid = loop {
            if let Ok(pid) = fs::read_to_string(&pid_file)
                && let Ok(pid) = pid.parse::<libc::pid_t>()
            {
                break pid;
            }
            if Instant::now() >= until || parent.try_wait().unwrap().is_some() {
                let _ = parent.kill();
                let _ = parent.wait();
                panic!("temporary server did not start for parent-death fixture");
            }
            std::thread::sleep(Duration::from_millis(10));
        };

        parent.kill().unwrap();
        let _ = parent.wait();
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: signal zero only probes existence of the exact fake
            // server PID recorded by this test's own child fixture.
            let alive = unsafe { libc::kill(server_pid, 0) } == 0;
            if !alive {
                break;
            }
            if Instant::now() >= until {
                // SAFETY: this is still the exact child PID created by this
                // test; kill it before failing so no test process leaks.
                unsafe {
                    libc::kill(server_pid, libc::SIGKILL);
                }
                panic!("temporary server survived parent death");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
