#!/usr/bin/env python3
"""Plan or run a network-isolated, no-prompt provider TUI PTY diagnostic.

Default behavior is planning only. A live run needs --run and
--acknowledge-disposable-home; it creates a fresh empty provider home, denies
network/desktop IPC, sends no input, never resumes a saved conversation, and
keeps raw terminal output only in a private host state directory. Successful
runs remove it; failed runs preserve it for diagnosis. This is not provider
task, authentication, or resume testing.
"""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]
PROTOCOL = 2
MAX_IPC = 8 * 1024 * 1024
PROVIDER_ARGS = {
    "codex": ["--sandbox", "read-only", "--ask-for-approval", "never", "--no-daemon"],
    "claude": [],
    "opencode": ["--pure"],
}
PROVIDER_COMMANDS = {"codex": "codex", "claude": "claude", "opencode": "opencode"}
FORBIDDEN_DESKTOP_ENV = (
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_ACTIVATION_TOKEN",
    "XDG_RUNTIME_DIR",
)
QUERY_PROBE = r'''
import os, select, termios, time, tty
tty.setraw(0, termios.TCSANOW)
os.write(1, b"\x1b[6n")
deadline = time.monotonic() + 2.0
reply = bytearray()
while time.monotonic() < deadline and b"R" not in reply:
    ready, _, _ = select.select([0], [], [], max(0, deadline - time.monotonic()))
    if not ready:
        break
    data = os.read(0, 64)
    if not data:
        break
    reply.extend(data)
os.write(1, b"\r\nPIGADE_DSR_OK\r\n" if b"\x1b[1;1R" in reply else b"\r\nPIGADE_DSR_FAIL\r\n")
'''.strip()


class DiagnosticError(RuntimeError):
    def __init__(self, message, *, failure_code="diagnostic_error", host_state=None,
                 host_rejection=None):
        super().__init__(message)
        self.failure_code = failure_code
        self.host_state = host_state
        self.host_rejection = host_rejection


class IpcFailure(DiagnosticError):
    def __init__(self, message, *, rejected_before_admission=None,
                 failure_code="ipc_outcome_uncertain", transport_error_type=None):
        super().__init__(message, failure_code=failure_code,
                         host_rejection=rejected_before_admission)
        self.rejected_before_admission = rejected_before_admission
        self.transport_error_type = transport_error_type


def digest(path):
    hasher = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def freeze_host_binary(source, destination, *, expected_sha256=None):
    """Copy an identified host binary and fail if source/copy bytes diverge."""
    source_sha256 = expected_sha256 or digest(source)
    shutil.copyfile(source, destination)
    Path(destination).chmod(0o700)
    frozen_sha256 = digest(destination)
    if frozen_sha256 != source_sha256:
        raise DiagnosticError("host binary changed while creating its frozen private copy")
    return source_sha256, frozen_sha256


def failure_metadata(provider, harness_source_sha256, provider_sha256, host_source_sha256,
                     frozen_host_sha256, stage, error, frame_summaries,
                     *, provider_launcher_sha256=None, provider_selection=None):
    """Redacted retained-run metadata; never serialize exception text/output."""
    return {
        "status": "failed",
        "provider": provider,
        "harness_source_sha256": harness_source_sha256,
        "provider_sha256": provider_sha256,
        "provider_launcher_sha256": provider_launcher_sha256,
        "provider_selection": provider_selection,
        "host_source_sha256": host_source_sha256,
        "frozen_host_sha256": frozen_host_sha256,
        "stage": stage,
        "error_type": type(error).__name__,
        "failure_code": getattr(error, "failure_code", "unexpected_error"),
        "host_state": getattr(error, "host_state", None),
        "host_rejection": getattr(error, "host_rejection", None),
        "transport_error_type": getattr(error, "transport_error_type", None),
        "last_frame_summaries": frame_summaries[-8:],
        "raw_provider_output_included": False,
    }


def has_elf_magic(path):
    with Path(path).open("rb") as source:
        return source.read(4) == b"\x7fELF"


def provider_selection_metadata(launcher, selected, explicit):
    """Hash both PATH launcher and selected executable without exposing paths."""
    return {
        "mode": "explicit_elf_override" if explicit else "path_default",
        "launcher_sha256": digest(launcher),
        "launcher_kind": "elf" if has_elf_magic(launcher) else "script_or_other",
        "selected_sha256": digest(selected),
        "selected_kind": "elf" if has_elf_magic(selected) else "script_or_other",
        "single_file_mount": bool(explicit),
    }


def process_identity(pid):
    """Linux identity tuple used to scope signal cleanup to this exact process."""
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
    boot_id = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    return boot_id, pid, int(fields[19])


def process_identity_if_present(pid):
    """Capture identity if still available; never signal by PID on failure."""
    try:
        return process_identity(pid)
    except (OSError, ValueError, IndexError, TypeError):
        return None


def identity_matches(pid, expected):
    try:
        return process_identity(pid) == expected
    except (OSError, ValueError, IndexError):
        return False


def make_host_environment(private_home):
    """Return an allowlisted environment; never forward desktop or auth env."""
    private_home = str(private_home)
    return {
        "PATH": "/usr/bin:/bin",
        "HOME": private_home,
        "XDG_CONFIG_HOME": f"{private_home}/.config",
        "XDG_DATA_HOME": f"{private_home}/.local/share",
        "XDG_CACHE_HOME": f"{private_home}/.cache",
        "XDG_STATE_HOME": f"{private_home}/.local/state",
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "TERM": "xterm-256color",
        "COLORTERM": "truecolor",
    }


def _is_relative_to(path, root):
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def provider_executable(name, path_lookup=shutil.which, explicit_path=None):
    if explicit_path is not None:
        resolved = Path(explicit_path).resolve(strict=True)
        if not resolved.is_file() or not os.access(resolved, os.X_OK):
            raise DiagnosticError("explicit provider executable must be a regular executable",
                                  failure_code="provider_executable_invalid")
        if not has_elf_magic(resolved):
            raise DiagnosticError("explicit provider executable must be a standalone ELF file",
                                  failure_code="provider_override_not_elf")
        return resolved
    command = PROVIDER_COMMANDS[name]
    found = path_lookup(command)
    if found is None:
        raise DiagnosticError(f"{name} executable is not on PATH")
    resolved = Path(found).resolve(strict=True)
    if not resolved.is_file() or not os.access(resolved, os.X_OK):
        raise DiagnosticError(f"{name} executable is not a regular executable")
    return resolved


def build_bwrap_argv(provider, executable, home, workspace, *, passwd_file, group_file,
                     bwrap="/usr/bin/bwrap", exact_binary=False):
    """Build an argv vector for an empty-home, read-only, offline TUI launch."""
    executable = Path(executable).resolve(strict=True)
    home = Path(home).resolve(strict=True)
    workspace = Path(workspace).resolve(strict=True)
    args = [
        bwrap,
        "--unshare-all",
        "--die-with-parent",
        "--clearenv",
        "--ro-bind", "/usr", "/usr",
        "--symlink", "usr/bin", "/bin",
        "--symlink", "usr/lib", "/lib",
        "--symlink", "usr/lib", "/lib64",
        "--proc", "/proc",
        "--dev", "/dev",
        "--tmpfs", "/tmp",
        "--dir", "/run",
        "--dir", "/home",
        "--dir", "/etc",
        "--ro-bind", "/etc/ld.so.cache", "/etc/ld.so.cache",
        "--ro-bind", str(passwd_file), "/etc/passwd",
        "--ro-bind", str(group_file), "/etc/group",
        "--dir", "/pigade",
        "--dir", "/home/pigade",
        "--bind", str(home), "/home/pigade",
        "--dir", "/workspace",
        "--ro-bind", str(workspace), "/workspace",
        "--chdir", "/workspace",
        "--setenv", "HOME", "/home/pigade",
        "--setenv", "XDG_CONFIG_HOME", "/home/pigade/.config",
        "--setenv", "XDG_DATA_HOME", "/home/pigade/.local/share",
        "--setenv", "XDG_CACHE_HOME", "/home/pigade/.cache",
        "--setenv", "XDG_STATE_HOME", "/home/pigade/.local/state",
        "--setenv", "TMPDIR", "/tmp",
        "--setenv", "PATH", "/usr/bin:/bin:/pigade",
        "--setenv", "LANG", "C.UTF-8",
        "--setenv", "LC_ALL", "C.UTF-8",
        "--setenv", "TERM", "xterm-256color",
        "--setenv", "COLORTERM", "truecolor",
    ]
    if _is_relative_to(executable, Path("/usr")) and not exact_binary:
        inner_executable = str(executable)
    else:
        # Standalone binaries are exposed as one read-only file, never by
        # mounting a user's home, toolchain, or package directory.
        if not has_elf_magic(executable):
            raise DiagnosticError("non-system provider must be a standalone ELF binary")
        args.extend(["--ro-bind", str(executable), "/pigade/provider-cli"])
        inner_executable = "/pigade/provider-cli"
    args.extend(["--", inner_executable, *PROVIDER_ARGS[provider]])
    return args


def build_query_probe_argv(home, workspace, *, passwd_file, group_file, bwrap="/usr/bin/bwrap"):
    """Tiny deterministic DSR child; no provider, task, or keyboard input."""
    args = build_bwrap_argv("opencode", "/usr/bin/python3", home, workspace,
                            passwd_file=passwd_file, group_file=group_file, bwrap=bwrap)
    # Replace the interactive provider with a bounded child that emits DSR and
    # records only whether the host replied at initial cursor position.
    args[-3:] = ["--", "/usr/bin/python3", "-c", QUERY_PROBE]
    return args


def request(endpoint, command, timeout=3):
    data = json.dumps({"protocol": PROTOCOL, "command": command}, separators=(",", ":")).encode()
    if len(data) > MAX_IPC:
        raise DiagnosticError("IPC request exceeds its size bound")
    try:
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(timeout)
            connection.connect(str(endpoint))
            connection.sendall(struct.pack("<I", len(data)) + data)

            def read(size):
                result = bytearray()
                while len(result) < size:
                    chunk = connection.recv(size - len(result))
                    if not chunk:
                        raise ValueError("truncated host response")
                    result.extend(chunk)
                return result

            length, = struct.unpack("<I", read(4))
            if not 0 < length <= MAX_IPC:
                raise ValueError("invalid host response size")
            response = json.loads(read(length))
    except TimeoutError as error:
        raise IpcFailure("IPC transport outcome is uncertain: timeout",
                         failure_code="socket_timeout", transport_error_type=type(error).__name__) from error
    except OSError as error:
        raise IpcFailure("IPC transport outcome is uncertain: socket failure",
                         failure_code="socket_transport_failure", transport_error_type=type(error).__name__) from error
    except json.JSONDecodeError as error:
        raise IpcFailure("IPC transport outcome is uncertain: malformed JSON response",
                         failure_code="malformed_json_response", transport_error_type=type(error).__name__) from error
    except (ValueError, TypeError) as error:
        raise IpcFailure("IPC transport outcome is uncertain: malformed response frame",
                         failure_code="malformed_response_frame", transport_error_type=type(error).__name__) from error
    if not isinstance(response, dict):
        raise IpcFailure("host response is malformed", failure_code="malformed_response")
    if response.get("protocol") != PROTOCOL:
        raise IpcFailure("host protocol mismatch", failure_code="protocol_mismatch")
    if response.get("error") is not None:
        rejection = response.get("rejected_before_admission")
        raise IpcFailure(
            "host returned an error",
            rejected_before_admission=rejection if rejection == "queue_full" else None,
            failure_code="typed_pre_admission_rejection" if rejection == "queue_full" else "host_error_response",
        )
    views = response.get("views")
    if not isinstance(views, list) or any(not isinstance(view, dict) for view in views):
        raise IpcFailure("host response views are malformed", failure_code="malformed_views")
    return views


def launch_once(endpoint, spec, *, timeout=3, reconcile_seconds=3, sleep=time.sleep):
    """Issue exactly one mutating launch; reconcile only with read-only List."""
    command = {"command": "launch", "spec": spec}
    try:
        views = request(endpoint, command, timeout=timeout)
        matches = [view for view in views if view.get("session") == spec["session"]]
        if len(matches) == 1 and matches[0].get("generation") == spec["generation"]:
            return matches[0], "launch_response"
        if matches:
            raise DiagnosticError("launch response identity mismatch")
    except IpcFailure as error:
        if error.rejected_before_admission is not None:
            raise DiagnosticError("host rejected launch before admission; no retry performed",
                                  failure_code="launch_rejected_before_admission",
                                  host_rejection=error.rejected_before_admission) from error
    deadline = time.monotonic() + reconcile_seconds
    while time.monotonic() < deadline:
        try:
            views = request(endpoint, {"command": "list"}, timeout=timeout)
        except IpcFailure:
            sleep(0.05)
            continue
        matches = [view for view in views if view.get("session") == spec["session"]]
        if matches:
            if len(matches) == 1 and matches[0].get("generation") == spec["generation"]:
                return matches[0], "launch_reconciled_by_identity"
            raise DiagnosticError("session exists with unexpected generation; no launch retry performed")
        sleep(0.05)
    raise DiagnosticError("launch outcome unknown after exact-identity reconciliation; no retry performed",
                          failure_code="launch_outcome_unknown")


def frame_summary(view):
    """Return geometry/mode counters only; never expose terminal text or title."""
    frame = view["frame"]
    geometry = frame["geometry"]
    return {
        "state": view["state"],
        "sequence": view["sequence"],
        "geometry": {"rows": geometry["rows"], "cols": geometry["cols"]},
        "cell_count": len(frame["cells"]),
        "mode_bits": frame["modes"],
        "output_bytes": view.get("metrics", {}).get("output_bytes"),
        "events": view.get("metrics", {}).get("events"),
        "storage_error_present": view.get("storage_error") is not None,
    }


def wait_for(endpoint, session, generation, predicate, timeout, *, interval=0.05,
             allowed_states=("running",)):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        views = request(endpoint, {"command": "snapshot", "session": session, "generation": generation})
        if len(views) != 1:
            raise DiagnosticError("host snapshot missing exact session")
        last = views[0]
        if last.get("state") not in allowed_states:
            raise DiagnosticError(f"session stopped during diagnostic ({last.get('state')})",
                                  failure_code="session_stopped", host_state=last.get("state"))
        if last.get("storage_error") is not None:
            raise DiagnosticError("host reported a PTY storage error",
                                  failure_code="host_storage_error", host_state=last.get("state"))
        if predicate(last):
            return last
        time.sleep(interval)
    raise DiagnosticError(f"bounded PTY observation timed out; last={frame_summary(last) if last else None}",
                          failure_code="observation_timeout",
                          host_state=last.get("state") if last else None)


def signal_owned(identity, sig):
    _boot, pid, _started = identity
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        return False
    pidfd = None
    try:
        pidfd = os.pidfd_open(pid, 0)
        if not identity_matches(pid, identity):
            return False
        signal.pidfd_send_signal(pidfd, sig)
        return True
    except (OSError, ValueError, IndexError):
        return False
    finally:
        if pidfd is not None:
            os.close(pidfd)


def cleanup_child(endpoint, session, generation, child_identity, host, host_identity, *, wait=1.0, stop_host=True):
    """Stop the owned PTY through host, then signal only the captured child identity."""
    try:
        request(endpoint, {"command": "stop", "session": session, "generation": generation}, timeout=2)
    except IpcFailure:
        pass
    deadline = time.monotonic() + wait
    while time.monotonic() < deadline:
        if host.poll() is not None:
            break
        try:
            views = request(endpoint, {"command": "snapshot", "session": session, "generation": generation}, timeout=0.5)
            if views and views[0].get("state") == "exited":
                break
        except IpcFailure:
            break
        time.sleep(0.05)
    if child_identity is not None:
        signal_owned(child_identity, signal.SIGTERM)
        time.sleep(0.1)
        signal_owned(child_identity, signal.SIGKILL)
    if stop_host and host.poll() is None:
        # Popen is the direct unreaped child. While poll() is None, its PID
        # cannot be reused, so signaling this exact child is race-safe.
        host.terminate()
        try:
            host.wait(timeout=3)
        except subprocess.TimeoutExpired:
            host.kill()
            host.wait(timeout=3)


def check_private_directory(directory):
    resolved = directory.resolve(strict=False)
    if _is_relative_to(resolved, ROOT.resolve()):
        raise DiagnosticError("diagnostic directory must be outside the repository")
    existed = resolved.exists()
    resolved.mkdir(parents=True, exist_ok=True, mode=0o700)
    resolved = resolved.resolve(strict=True)
    if resolved.stat().st_uid != os.getuid():
        raise DiagnosticError("diagnostic directory must be owned by the current user")
    if existed and resolved.stat().st_mode & 0o077:
        raise DiagnosticError("existing diagnostic directory must already be private (mode 0700)")
    return resolved


def create_short_runtime_directory(base=Path("/tmp")):
    """Make a fresh private runtime whose host.sock fits Linux sun_path."""
    runtime = Path(tempfile.mkdtemp(prefix="pga-", dir=base))
    runtime.chmod(0o700)
    endpoint = runtime / "host.sock"
    if len(os.fsencode(endpoint)) >= 108:
        shutil.rmtree(runtime)
        raise DiagnosticError("temporary runtime path is too long for a Unix socket")
    return runtime


def host_argv(binary, state_directory, runtime_directory):
    return [str(binary), "serve", str(state_directory), "--runtime", str(runtime_directory)]


def plan(provider):
    executable = shutil.which(PROVIDER_COMMANDS[provider])
    return {
        "mode": "plan_only",
        "provider": provider,
        "provider_available_on_path": executable is not None,
        "spawned_processes": False,
        "safe_boundary": "real run requires explicit flags, a new empty provider home, bwrap namespaces, no network, no desktop variables/sockets, no PTY input, and no initial prompt",
        "command_shape": ["bwrap", "--unshare-all", "--die-with-parent", "--clearenv", "--ro-bind /usr", "--dir /home /run", "--tmpfs /tmp", "--", PROVIDER_COMMANDS[provider], *PROVIDER_ARGS[provider]],
        "not_tested": ["provider authentication", "task execution", "conversation resume", "provider terminal-query receipt", "GUI/platform behavior"],
    }


def run_diagnostic(args):
    if sys.platform != "linux":
        raise DiagnosticError("live provider PTY diagnostic currently requires Linux bubblewrap")
    if not args.run or not args.acknowledge_disposable_home:
        raise DiagnosticError("live run requires --run and --acknowledge-disposable-home")
    directory = check_private_directory(args.directory)
    bwrap = shutil.which("bwrap")
    if not bwrap:
        raise DiagnosticError("bubblewrap is required for live mode")
    host_source = args.host_binary.resolve(strict=True)
    provider_launcher = provider_executable(args.provider)
    provider_source = provider_executable(
        args.provider, explicit_path=getattr(args, "provider_executable", None)
    )
    provider_selection = provider_selection_metadata(
        provider_launcher, provider_source,
        explicit=getattr(args, "provider_executable", None) is not None,
    )
    if not os.access(host_source, os.X_OK):
        raise DiagnosticError("host binary must be executable")

    host_source_sha256 = digest(host_source)
    harness_source_sha256 = digest(__file__)
    result = {
        "captured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "status": "running",
        "provider": args.provider,
        "harness_source_sha256": harness_source_sha256,
        "scope": "one live provider TUI started with no prompt; network-off bwrap namespace; no PTY input; fresh ephemeral HOME",
        "limits": [
            "not provider task/authentication/resume testing",
            "provider output text/title are never copied into result; failed private runtime is retained, successful runtime is deleted",
            "provider TUI query replies are not separately attributed; the companion DSR probe verifies host reply path",
            "resize is sent through host IPC and observed in the terminal frame; no desktop UI is opened",
        ],
        "provider_sha256": digest(provider_source),
        "provider_launcher_sha256": provider_selection["launcher_sha256"],
        "provider_selection": provider_selection,
        "host_source_sha256": host_source_sha256,
        "binary_paths_recorded": False,
        "network_namespace": "unshared by bubblewrap --unshare-all",
        "desktop_environment_forwarded": False,
        "provider_input_bytes_sent": 0,
    }
    state = directory / f"provider-pty-run-{datetime.datetime.now(datetime.timezone.utc):%Y%m%dT%H%M%SZ}-{uuid.uuid4().hex[:12]}"
    state.mkdir(mode=0o700)
    state.chmod(0o700)
    host = None
    host_identity = None
    child_identity = None
    runtime = None
    session = generation = None
    log = None
    stage = "prepare_private_runtime"
    frame_summaries = []
    try:
        host_home = state / "host-home"
        provider_home = state / "provider-home"
        workspace = state / "empty-workspace"
        passwd_file, group_file = state / "passwd", state / "group"
        for path in (host_home, provider_home, workspace):
            path.mkdir(mode=0o700)
        passwd_file.write_text(f"pigade:x:{os.getuid()}:{os.getgid()}:Temporary provider test:/home/pigade:/usr/bin/bash\n")
        group_file.write_text(f"pigade:x:{os.getgid()}:\n")
        passwd_file.chmod(0o600)
        group_file.chmod(0o600)
        for base in (host_home, provider_home):
            for child in (".config", ".local/share", ".cache", ".local/state"):
                (base / child).mkdir(parents=True, mode=0o700, exist_ok=True)
        frozen_host = state / "pigade-session-host"
        _source_hash, frozen_hash = freeze_host_binary(
            host_source, frozen_host, expected_sha256=result["host_source_sha256"]
        )
        result["frozen_host_sha256"] = frozen_hash
        runtime = create_short_runtime_directory()
        endpoint = runtime / "host.sock"
        env = make_host_environment(host_home)
        log_path = state / "host.log"
        log = open(log_path, "wb")
        stage = "start_private_host"
        host = subprocess.Popen(
                host_argv(frozen_host, state, runtime),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=log,
                close_fds=True,
                start_new_session=True,
                env=env,
            )
        host_identity = process_identity(host.pid)
        deadline = time.monotonic() + 8
        while True:
            if host.poll() is not None:
                raise DiagnosticError("private host exited before IPC became ready")
            try:
                request(endpoint, {"command": "list"}, timeout=0.5)
                break
            except IpcFailure:
                if time.monotonic() >= deadline:
                    raise DiagnosticError("private host did not become ready before deadline")
                time.sleep(0.05)

        # Verify a concrete terminal query reply through the actual host/PTY
        # path using a small fixed child that receives no keyboard input.
        stage = "host_dsr_query_probe"
        query_session, query_generation = str(uuid.uuid4()), str(uuid.uuid4())
        query_argv = build_query_probe_argv(
                provider_home, workspace, passwd_file=passwd_file,
                group_file=group_file, bwrap=bwrap,
            )
        query_spec = {
                "session": query_session,
                "generation": query_generation,
                "operation": str(uuid.uuid4()),
                "executable": query_argv[0],
                "args": query_argv[1:],
                "cwd": str(state),
                "geometry": {"rows": 24, "cols": 80},
            }
        query_view, query_launch = launch_once(endpoint, query_spec)
        query_view = wait_for(
                endpoint, query_session, query_generation,
                lambda view: "PIGADE_DSR_OK" in "".join(cell["text"] for cell in view["frame"]["cells"]),
                args.startup_timeout,
                allowed_states=("running", "exited"),
            )
        query_summary = frame_summary(query_view)
        frame_summaries.append({"stage": stage, **query_summary})
        result["host_dsr_query_probe"] = {
                "reply_observed": True,
                "sequence": query_view["sequence"],
                "launch_resolution": query_launch,
            }
        cleanup_child(endpoint, query_session, query_generation, None, host, host_identity,
                      wait=0.5, stop_host=False)
        session = generation = child_identity = None

        # This one launch has no positional prompt or resume ID. Any lost
        # response is reconciled by the exact fresh session/generation;
        # this function never repeats the mutating launch request.
        stage = "provider_tui_launch_and_observe"
        session, generation = str(uuid.uuid4()), str(uuid.uuid4())
        provider_argv = build_bwrap_argv(
                args.provider, provider_source, provider_home, workspace,
                passwd_file=passwd_file, group_file=group_file, bwrap=bwrap,
                exact_binary=provider_selection["single_file_mount"],
            )
        provider_spec = {
                "session": session,
                "generation": generation,
                "operation": str(uuid.uuid4()),
                "executable": provider_argv[0],
                "args": provider_argv[1:],
                "cwd": str(state),
                "geometry": {"rows": 24, "cols": 80},
            }
        view, launch_resolution = launch_once(endpoint, provider_spec, timeout=3, reconcile_seconds=4)
        view = wait_for(
                endpoint, session, generation,
                lambda current: current.get("metrics", {}).get("output_bytes", 0) > 0,
                args.startup_timeout,
            )
        child_identity = process_identity_if_present(view["pid"])
        startup = frame_summary(view)
        frame_summaries.append({"stage": "provider_startup", **startup})
        resize_observations = []
        for rows, cols in ((30, 100), (24, 80)):
            stage = f"provider_resize_request_{rows}x{cols}"
            prior_output_bytes = view.get("metrics", {}).get("output_bytes", 0)
            request(endpoint, {"command": "resize", "session": session, "generation": generation,
                                   "geometry": {"rows": rows, "cols": cols}})
            stage = f"provider_resize_observation_{rows}x{cols}"
            view = wait_for(
                    endpoint, session, generation,
                    lambda current, rows=rows, cols=cols, prior=prior_output_bytes: (
                        current["frame"]["geometry"] == {"rows": rows, "cols": cols}
                        and current.get("metrics", {}).get("output_bytes", 0) > prior
                    ),
                    args.resize_timeout,
                )
            summary = frame_summary(view)
            resize_observations.append(summary)
            frame_summaries.append({"stage": f"resize_{rows}x{cols}", **summary})
        time.sleep(args.observe_seconds)
        stage = "provider_final_snapshot"
        views = request(endpoint, {"command": "snapshot", "session": session, "generation": generation})
        if len(views) != 1:
            raise DiagnosticError("host snapshot missing exact provider session")
        view = views[0]
        if view.get("state") != "running":
            raise DiagnosticError("provider TUI exited during observation window")
        final_summary = frame_summary(view)
        frame_summaries.append({"stage": "final_observation", **final_summary})
        result["provider_tui"] = {
                "launch_resolution": launch_resolution,
                "startup_frame": startup,
                "resizes": resize_observations,
                "final_frame": final_summary,
                "remained_running_during_observation": True,
            }
        result["status"] = "pty_startup_resize_and_host_dsr_pass"
    except BaseException as error:
        metadata = failure_metadata(
            args.provider, result.get("harness_source_sha256"),
            result.get("provider_sha256"),
            result.get("host_source_sha256"), result.get("frozen_host_sha256"),
            stage, error, frame_summaries,
            provider_launcher_sha256=result.get("provider_launcher_sha256"),
            provider_selection=result.get("provider_selection"),
        )
        failure_path = state / "failure.json"
        with open(failure_path, "x", encoding="utf-8") as failure_file:
            os.chmod(failure_path, 0o600)
            json.dump(metadata, failure_file, indent=2)
            failure_file.write("\n")
        if isinstance(error, KeyboardInterrupt):
            raise
        raise DiagnosticError(f"diagnostic failed at {stage}; private runtime retained at {state}") from error
    finally:
        if host is not None:
            if session is not None and generation is not None:
                cleanup_child(endpoint, session, generation, child_identity, host, host_identity, wait=1)
            elif host.poll() is None:
                # Exact direct Popen child is unreaped while poll() is None.
                host.terminate()
                try:
                    host.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    host.kill()
                    host.wait(timeout=3)
        if log is not None:
            log.close()
        if runtime is not None and (host is None or host.poll() is not None):
            shutil.rmtree(runtime)
    shutil.rmtree(state)
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provider", choices=sorted(PROVIDER_COMMANDS), default="codex")
    parser.add_argument("--run", action="store_true", help="execute the isolated diagnostic; default only prints a plan")
    parser.add_argument("--acknowledge-disposable-home", action="store_true",
                        help="confirm that this run uses a new empty temporary provider home, not existing credentials/history")
    parser.add_argument("--directory", type=Path,
                        default=Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "pigade/provider-pty")
    parser.add_argument("--host-binary", type=Path, default=ROOT / "target/release/pigade-session-host")
    parser.add_argument("--provider-executable", type=Path,
                        help="opt in to one standalone ELF provider executable; it is mounted as one exact read-only file")
    parser.add_argument("--startup-timeout", type=float, default=10)
    parser.add_argument("--resize-timeout", type=float, default=3)
    parser.add_argument("--observe-seconds", type=float, default=1)
    args = parser.parse_args(argv)
    if args.startup_timeout <= 0 or args.startup_timeout > 60:
        parser.error("--startup-timeout must be in (0, 60]")
    if args.resize_timeout <= 0 or args.resize_timeout > 20:
        parser.error("--resize-timeout must be in (0, 20]")
    if args.observe_seconds < 0 or args.observe_seconds > 10:
        parser.error("--observe-seconds must be in [0, 10]")
    if not args.run:
        print(json.dumps(plan(args.provider), indent=2))
        return 0
    result = run_diagnostic(args)
    print(json.dumps(result, indent=2))
    return 0 if result["status"] == "pty_startup_resize_and_host_dsr_pass" else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except DiagnosticError as error:
        print(json.dumps({"status": "blocked_or_failed", "reason": str(error), "initial_prompt_sent": False}, indent=2), file=sys.stderr)
        raise SystemExit(2)
