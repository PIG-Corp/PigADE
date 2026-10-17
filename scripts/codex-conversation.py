#!/usr/bin/env python3
"""Plan or run a bounded native Codex conversation/resume continuity check.

Default operation is plan-only. A live run requires --run and an explicit,
new private --directory outside the repository. It uses the installed Codex
CLI and its existing native CODEX_HOME authentication/history directly; it
does not inspect, copy, or export those files. Only redacted metadata is saved.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import sys
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]
MAX_STDOUT = 1024 * 1024
MAX_STDERR = 64 * 1024
MAX_LINE = 128 * 1024
MAX_EVENTS = 2048
TIMEOUT_SECONDS = 120
ACK = "PIGADE_CONTEXT_ACK"
FORBIDDEN_DESKTOP_ENV = (
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_ACTIVATION_TOKEN",
    "XDG_RUNTIME_DIR",
)
NATIVE_AUTH_ENV = {"CODEX_API_KEY", "CODEX_ACCESS_TOKEN", "OPENAI_API_KEY"}
PRESERVED_ENV = {
    "PATH",
    "HOME",
    "CODEX_HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CODEX_CA_CERTIFICATE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
} | NATIVE_AUTH_ENV


class ConversationError(RuntimeError):
    def __init__(self, stage: str, code: str):
        super().__init__(f"{stage}:{code}")
        self.stage = stage
        self.code = code


@dataclass(frozen=True)
class Invocation:
    returncode: int
    stdout: bytes
    stderr: bytes
    timed_out: bool = False
    output_limit_exceeded: bool = False


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def initial_prompt(nonce: str) -> str:
    return (
        "This is a harmless conversation-continuity check. Do not use tools, "
        "search, browse, or inspect files. Remember the exact synthetic token "
        f"{nonce} for my next turn. Reply with exactly {ACK}."
    )


def resume_prompt() -> str:
    return (
        "This is the follow-up to the prior continuity check. Do not use tools, "
        "search, browse, or inspect files. Reply with only the exact synthetic "
        "token I asked you to remember in the previous turn."
    )


def build_initial_argv(executable: str, cwd: Path, prompt: str) -> list[str]:
    return [
        executable,
        "--no-daemon",
        "exec",
        "--json",
        "--color",
        "never",
        "--sandbox",
        "read-only",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--cd",
        str(cwd),
        prompt,
    ]


def build_resume_argv(executable: str, cwd: Path, thread_id: str, prompt: str) -> list[str]:
    # Keep exec-level safety/config flags before the resume subcommand; this
    # ordering is accepted by the installed 0.162.1 CLI help parser.
    return [
        executable,
        "--no-daemon",
        "exec",
        "--json",
        "--color",
        "never",
        "--sandbox",
        "read-only",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--cd",
        str(cwd),
        "resume",
        thread_id,
        prompt,
    ]


def child_environment(source: dict[str, str]) -> dict[str, str]:
    """Preserve only CLI, locale, trust/network and native auth inputs."""
    environment = {name: value for name, value in source.items() if name in PRESERVED_ENV}
    environment["BROWSER"] = "/bin/false"
    environment["NO_COLOR"] = "1"
    environment["CI"] = "1"
    return environment


def parse_jsonl(payload: bytes) -> list[dict]:
    if len(payload) > MAX_STDOUT:
        raise ConversationError("parse", "stdout_limit")
    events = []
    for line in payload.splitlines():
        if not line.strip():
            continue
        if len(line) > MAX_LINE:
            raise ConversationError("parse", "line_limit")
        try:
            event = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise ConversationError("parse", "malformed_jsonl") from None
        if not isinstance(event, dict):
            raise ConversationError("parse", "event_not_object")
        events.append(event)
        if len(events) > MAX_EVENTS:
            raise ConversationError("parse", "event_limit")
    return events


def _thread_ids(events: list[dict]) -> list[str]:
    ids = []
    for event in events:
        if event.get("type") == "thread.started":
            thread_id = event.get("thread_id")
            if not isinstance(thread_id, str):
                raise ConversationError("events", "thread_id_missing")
            try:
                uuid.UUID(thread_id)
            except (ValueError, AttributeError):
                raise ConversationError("events", "thread_id_invalid") from None
            ids.append(thread_id)
    return ids


def _completed_turn(events: list[dict]) -> None:
    if any(event.get("type") in {"turn.failed", "error"} for event in events):
        raise ConversationError("events", "turn_failed")
    if sum(event.get("type") == "turn.completed" for event in events) != 1:
        raise ConversationError("events", "turn_completion_missing_or_ambiguous")


def _assistant_messages(events: list[dict]) -> list[str]:
    messages = []
    for event in events:
        if not str(event.get("type", "")).startswith("item."):
            continue
        item = event.get("item")
        if not isinstance(item, dict):
            continue
        # Reasoning is a non-action stream item. Do not retain or expose its
        # content; only reject items that represent tools or other side effects.
        if item.get("type") == "reasoning":
            continue
        # This probe is intentionally tool-free. A non-message item means the
        # CLI/model attempted an action; read-only sandbox remains the backstop.
        if item.get("type") != "agent_message":
            raise ConversationError("events", "unexpected_non_message_item")
        if event.get("type") == "item.completed":
            text = item.get("text")
            if not isinstance(text, str) or len(text.encode("utf-8")) > MAX_LINE:
                raise ConversationError("events", "assistant_message_invalid")
            messages.append(text)
    if len(messages) != 1:
        raise ConversationError("events", "assistant_message_missing_or_ambiguous")
    return messages


def validate_initial_output(payload: bytes) -> tuple[str, bool]:
    events = parse_jsonl(payload)
    ids = _thread_ids(events)
    if len(ids) != 1:
        raise ConversationError("initial", "thread_started_missing_or_ambiguous")
    _completed_turn(events)
    if _assistant_messages(events)[-1].strip() != ACK:
        raise ConversationError("initial", "ack_mismatch")
    return ids[0], True


def validate_resume_output(payload: bytes, expected_thread_id: str, nonce: str) -> bool | None:
    events = parse_jsonl(payload)
    ids = _thread_ids(events)
    if len(ids) > 1:
        raise ConversationError("resume", "thread_started_ambiguous")
    if ids and ids[0] != expected_thread_id:
        raise ConversationError("resume", "thread_id_changed")
    _completed_turn(events)
    if _assistant_messages(events)[-1].strip() != nonce:
        raise ConversationError("resume", "context_nonce_mismatch")
    return ids[0] == expected_thread_id if ids else None


def invoke(argv: list[str], cwd: Path, environment: dict[str, str], *,
           timeout: float = TIMEOUT_SECONDS) -> Invocation:
    """Run one bounded non-interactive CLI process; never retry on uncertainty."""
    process = subprocess.Popen(
        argv,
        cwd=cwd,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        close_fds=True,
    )
    assert process.stdout is not None and process.stderr is not None
    selector = None
    outputs = {"stdout": bytearray(), "stderr": bytearray()}
    limits = {"stdout": MAX_STDOUT, "stderr": MAX_STDERR}
    deadline = time.monotonic() + timeout
    timed_out = False
    output_limit_exceeded = False

    try:
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ, "stdout")
        selector.register(process.stderr, selectors.EVENT_READ, "stderr")
        # Drain both streams through EOF even after the direct child exits: its
        # final JSONL event may still be buffered in either pipe.
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
                break
            for key, _ in selector.select(min(remaining, 0.25)):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    key.fileobj.close()
                    continue
                buffer = outputs[key.data]
                if len(buffer) + len(chunk) > limits[key.data]:
                    output_limit_exceeded = True
                    break
                buffer.extend(chunk)
            if output_limit_exceeded:
                break

        if not timed_out and not output_limit_exceeded:
            remaining = max(0., deadline - time.monotonic())
            try:
                process.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                timed_out = True

        if timed_out or output_limit_exceeded:
            if process.poll() is None:
                try:
                    process.terminate()
                except ProcessLookupError:
                    pass
            try:
                process.wait(timeout=1.0)
            except subprocess.TimeoutExpired:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                process.wait()

        return Invocation(
            returncode=process.wait(),
            stdout=bytes(outputs["stdout"]),
            stderr=bytes(outputs["stderr"]),
            timed_out=timed_out,
            output_limit_exceeded=output_limit_exceeded,
        )
    finally:
        if selector is not None:
            selector.close()
        for pipe in (process.stdout, process.stderr):
            if pipe is not None and not pipe.closed:
                pipe.close()
        # On exceptional cleanup, terminate only the exact direct child owned
        # by this Popen object; never signal a process group or a PID fallback.
        if process.poll() is None:
            try:
                process.terminate()
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=1.0)
            except subprocess.TimeoutExpired:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                process.wait()


def require_success(stage: str, invocation: Invocation) -> None:
    if invocation.timed_out:
        raise ConversationError(stage, "timeout")
    if invocation.output_limit_exceeded:
        raise ConversationError(stage, "output_limit")
    if invocation.returncode != 0:
        raise ConversationError(stage, "nonzero_exit")


def run_conversation(executable: str, executable_sha256: str, cwd: Path,
                     environment: dict[str, str], invoker=None,
                     nonce: str | None = None, phase_callback=None) -> dict:
    invoker = invoker or invoke
    nonce = nonce or f"PIGADECTX-{uuid.uuid4().hex}"
    nonce_hash = sha256_bytes(nonce.encode("ascii"))
    first_argv = build_initial_argv(executable, cwd, initial_prompt(nonce))
    if phase_callback is not None:
        phase_callback("initial_spawn_intent", None)
    try:
        first = invoker(first_argv, cwd, environment)
    except OSError:
        raise ConversationError("initial", "spawn_failed") from None
    require_success("initial", first)
    thread_id, initial_event_seen = validate_initial_output(first.stdout)

    # Exact-ID resume only. If this phase fails or yields uncertainty, do not
    # retry and never fall back to --last or a new conversation.
    resume_argv = build_resume_argv(executable, cwd, thread_id, resume_prompt())
    if phase_callback is not None:
        phase_callback("resume_spawn_intent", sha256_bytes(thread_id.encode("ascii")))
    try:
        resumed = invoker(resume_argv, cwd, environment)
    except OSError:
        raise ConversationError("resume", "spawn_failed") from None
    require_success("resume", resumed)
    same_thread_event = validate_resume_output(resumed.stdout, thread_id, nonce)
    return {
        "status": "passed",
        "provider": "codex",
        "cli_sha256": executable_sha256,
        "thread_id_sha256": sha256_bytes(thread_id.encode("ascii")),
        "context_nonce_sha256": nonce_hash,
        "initial_thread_started_event": initial_event_seen,
        "resume_thread_started_same_id": same_thread_event,
        "initial_stdout_sha256": sha256_bytes(first.stdout),
        "resume_stdout_sha256": sha256_bytes(resumed.stdout),
        "raw_stdout_saved": False,
        "raw_stderr_saved": False,
        "used_exact_resume_id": True,
        "used_last_fallback": False,
        "native_codex_home_policy": "inherited_directly_not_copied",
        "cwd_is_private_run_directory": True,
    }


def write_private_json(path: Path, payload: dict) -> None:
    encoded = (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode()
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as target:
        target.write(encoded)
        target.flush()
        os.fsync(target.fileno())


def replace_private_json(path: Path, payload: dict) -> None:
    """Durably replace an existing private result without exposing partial JSON."""
    encoded = (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode()
    temporary = path.parent / f".{path.name}.{uuid.uuid4().hex}.tmp"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as target:
            target.write(encoded)
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def prepare_run_directory(path: Path) -> tuple[Path, Path]:
    resolved = path.expanduser().resolve()
    if resolved == ROOT or ROOT in resolved.parents:
        raise ConversationError("preflight", "run_directory_inside_repository")
    if resolved.exists():
        raise ConversationError("preflight", "run_directory_must_be_new")
    resolved.mkdir(parents=True, mode=0o700)
    resolved.chmod(0o700)
    cwd = resolved / "cwd"
    cwd.mkdir(mode=0o700)
    return resolved, cwd


def plan_payload(executable: str | None, directory: Path | None) -> dict:
    cwd = (directory or Path("<required-private-run-directory>")) / "cwd"
    initial = build_initial_argv(executable or "codex", cwd, initial_prompt("<synthetic-token>"))
    resume = build_resume_argv(executable or "codex", cwd, "<thread.started.thread_id>", resume_prompt())
    return {
        "mode": "plan_only",
        "spawned_processes": False,
        "executable_resolved": executable is not None,
        "initial_argv": initial,
        "resume_argv": resume,
        "requires_new_private_directory_outside_repository": True,
        "live_run_requires_flags": ["--run", "--acknowledge-native-auth-and-network"],
        "uses_native_codex_home_directly": True,
        "harness_copies_auth_files": False,
        "cli_auth_access": "native Codex behavior",
        "native_rollouts_are_isolated": False,
        "raw_provider_output_saved": False,
    }


def run_mode(directory: Path) -> dict:
    resolved, cwd = prepare_run_directory(directory)
    executable = shutil.which("codex")
    if executable is None:
        raise ConversationError("preflight", "codex_not_on_path")
    executable_path = Path(executable).resolve(strict=True)
    if not executable_path.is_file() or not os.access(executable_path, os.X_OK):
        raise ConversationError("preflight", "codex_executable_invalid")
    executable_sha256 = sha256_file(executable_path)
    harness_sha256 = sha256_file(Path(__file__).resolve())
    metadata = {
        "status": "running",
        "phase": "version_probe_spawn_intent",
        "provider": "codex",
        "cli_sha256": executable_sha256,
        "harness_source_sha256": harness_sha256,
        "raw_stdout_saved": False,
        "raw_stderr_saved": False,
        "native_codex_home_policy": "inherited_directly_not_copied",
        "native_rollouts_are_isolated": False,
        "cwd_is_private_run_directory": True,
    }
    result_path = resolved / "result.json"
    write_private_json(result_path, metadata)
    try:
        version = invoke([str(executable_path), "--version"], cwd, child_environment(dict(os.environ)), timeout=5.)
        require_success("version_probe", version)
        cli_version = version.stdout.decode("utf-8", errors="replace").strip()
        if not cli_version or len(cli_version) > 128 or "\n" in cli_version:
            raise ConversationError("version_probe", "version_output_invalid")
        metadata["cli_version"] = cli_version
        metadata["phase"] = "initial_spawn_intent"
        replace_private_json(result_path, metadata)

        def persist_phase(phase: str, thread_hash: str | None) -> None:
            metadata["phase"] = phase
            if thread_hash is not None:
                metadata["thread_id_sha256"] = thread_hash
            replace_private_json(result_path, metadata)

        metadata.update(
            run_conversation(
                str(executable_path),
                executable_sha256,
                cwd,
                child_environment(dict(os.environ)),
                phase_callback=persist_phase,
            )
        )
    except ConversationError as error:
        metadata["status"] = "failed"
        metadata["failure_stage"] = error.stage
        metadata["failure_code"] = error.code
    except OSError:
        metadata["status"] = "failed"
        metadata["failure_stage"] = "spawn"
        metadata["failure_code"] = "os_error"
    metadata["created_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
    replace_private_json(result_path, metadata)
    return metadata


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="execute one native exec + exact-ID resume pair")
    parser.add_argument("--directory", type=Path, help="new private run directory outside the repository")
    parser.add_argument(
        "--acknowledge-native-auth-and-network",
        action="store_true",
        help="acknowledge use of existing Codex auth and provider API for two fixed harmless turns",
    )
    args = parser.parse_args(argv)
    if not args.run:
        if args.acknowledge_native_auth_and_network:
            parser.error("--acknowledge-native-auth-and-network requires --run")
        executable = shutil.which("codex")
        print(json.dumps(plan_payload(executable, args.directory), indent=2, sort_keys=True))
        return 0
    if not args.acknowledge_native_auth_and_network:
        parser.error("--run requires --acknowledge-native-auth-and-network")
    if args.directory is None:
        parser.error("--run requires a unique new --directory outside the repository")
    result = run_mode(args.directory)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
