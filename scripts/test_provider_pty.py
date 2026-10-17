import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("provider_pty", ROOT / "scripts/provider-pty.py")
provider_pty = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(provider_pty)


class ProviderPtyTests(unittest.TestCase):
    def test_default_cli_only_plans_and_never_spawns(self):
        output = io.StringIO()
        with mock.patch.object(provider_pty.subprocess, "Popen", side_effect=AssertionError("spawned")), \
             contextlib.redirect_stdout(output):
            self.assertEqual(provider_pty.main(["--provider", "codex"]), 0)
        plan = __import__("json").loads(output.getvalue())
        self.assertEqual(plan["mode"], "plan_only")
        self.assertFalse(plan["spawned_processes"])

    def test_live_mode_requires_both_opt_in_flags(self):
        with mock.patch.object(provider_pty.subprocess, "Popen", side_effect=AssertionError("spawned")):
            with self.assertRaisesRegex(provider_pty.DiagnosticError, "requires --run and --acknowledge"):
                provider_pty.main(["--run"])

    def test_host_environment_is_allowlisted_and_has_no_desktop_or_secret_values(self):
        environment = provider_pty.make_host_environment("/private/host-home")
        self.assertFalse(set(provider_pty.FORBIDDEN_DESKTOP_ENV) & environment.keys())
        self.assertEqual(environment["HOME"], "/private/host-home")
        self.assertEqual(environment["PATH"], "/usr/bin:/bin")
        self.assertEqual(set(environment), {
            "PATH", "HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME",
            "XDG_STATE_HOME", "LANG", "LC_ALL", "TERM", "COLORTERM",
        })

    def test_provider_argv_has_no_prompt_resume_or_user_input_path(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home, workspace = root / "home", root / "workspace"
            home.mkdir()
            workspace.mkdir()
            passwd, group = root / "passwd", root / "group"
            passwd.write_text("pigade:x:1000:1000:test:/home/pigade:/bin/sh\n")
            group.write_text("pigade:x:1000:\n")
            for provider, executable in (("claude", "/usr/bin/claude"), ("opencode", "/usr/bin/opencode")):
                argv = provider_pty.build_bwrap_argv(
                    provider, executable, home, workspace, passwd_file=passwd, group_file=group,
                )
                separator = argv.index("--")
                self.assertEqual(argv[separator + 1:], [str(Path(executable).resolve()), *provider_pty.PROVIDER_ARGS[provider]])
                self.assertNotIn("--resume", argv[separator + 1:])
                self.assertNotIn("--last", argv[separator + 1:])
                self.assertIn("--unshare-all", argv)
                self.assertIn("--clearenv", argv)
                self.assertNotIn("--new-session", argv, "keep the inherited PTY as the controlling terminal")
                self.assertIn("/workspace", argv)
            codex = root / "codex"
            codex.write_bytes(b"\x7fELFfixture")
            codex.chmod(0o700)
            argv = provider_pty.build_bwrap_argv(
                "codex", codex, home, workspace, passwd_file=passwd, group_file=group,
            )
            separator = argv.index("--")
            self.assertEqual(argv[separator + 1:], ["/pigade/provider-cli", *provider_pty.PROVIDER_ARGS["codex"]])
            self.assertIn(str(codex.resolve()), argv)
            self.assertNotIn("--last", argv)

    def test_explicit_provider_override_is_one_exact_elf_mount(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home, workspace = root / "home", root / "workspace"
            home.mkdir()
            workspace.mkdir()
            passwd, group = root / "passwd", root / "group"
            passwd.write_text("pigade:x:1000:1000:test:/home/pigade:/bin/sh\n")
            group.write_text("pigade:x:1000:\n")
            selected = root / "provider"
            selected.write_bytes(b"\x7fELF-test-exact-binary")
            selected.chmod(0o700)
            actual = provider_pty.provider_executable("claude", explicit_path=selected)
            self.assertEqual(actual, selected.resolve())
            argv = provider_pty.build_bwrap_argv(
                "claude", actual, home, workspace, passwd_file=passwd,
                group_file=group, exact_binary=True,
            )
            self.assertIn(["--ro-bind", str(actual), "/pigade/provider-cli"],
                          [argv[index:index + 3] for index in range(len(argv) - 2)])
            self.assertEqual(argv[argv.index("--") + 1], "/pigade/provider-cli")

    def test_provider_selection_metadata_distinguishes_wrapper_from_override(self):
        with tempfile.TemporaryDirectory() as temporary:
            wrapper = Path(temporary) / "wrapper"
            actual = Path(temporary) / "actual"
            wrapper.write_text("#!/bin/sh\nexec /opt/provider\n")
            actual.write_bytes(b"\x7fELF-actual-provider")
            metadata = provider_pty.provider_selection_metadata(wrapper, actual, explicit=True)
        self.assertEqual(metadata["mode"], "explicit_elf_override")
        self.assertEqual(metadata["launcher_kind"], "script_or_other")
        self.assertEqual(metadata["selected_kind"], "elf")
        self.assertNotEqual(metadata["launcher_sha256"], metadata["selected_sha256"])
        self.assertTrue(metadata["single_file_mount"])

    def test_non_system_binary_elf_preflight_reads_only_magic_prefix(self):
        stream = mock.MagicMock()
        stream.__enter__.return_value = stream
        stream.read.return_value = b"\x7fELF"
        with mock.patch.object(Path, "open", return_value=stream):
            self.assertTrue(provider_pty.has_elf_magic(Path("large-provider")))
        stream.read.assert_called_once_with(4)

    def test_query_probe_is_a_bounded_dsr_handshake_not_keyboard_input(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home, workspace = root / "home", root / "workspace"
            home.mkdir()
            workspace.mkdir()
            passwd, group = root / "passwd", root / "group"
            passwd.write_text("pigade:x:1000:1000:test:/home/pigade:/bin/sh\n")
            group.write_text("pigade:x:1000:\n")
            argv = provider_pty.build_query_probe_argv(
                home, workspace, passwd_file=passwd, group_file=group,
            )
            self.assertEqual(argv[-3:], ["/usr/bin/python3", "-c", provider_pty.QUERY_PROBE])
            self.assertEqual(argv[-4], "--")
            self.assertIn(b"\\x1b[6n", provider_pty.QUERY_PROBE.encode())
            self.assertIn(b"PIGADE_DSR_OK", provider_pty.QUERY_PROBE.encode())
            self.assertNotIn("input", provider_pty.QUERY_PROBE.lower())

    def test_dsr_query_accepts_terminal_success_after_child_exit(self):
        view = {
            "state": "exited", "storage_error": None,
            "frame": {"geometry": {"rows": 24, "cols": 80}, "cells": [{"text": "PIGADE_DSR_OK"}], "modes": 0},
            "sequence": 4,
        }
        with mock.patch.object(provider_pty, "request", return_value=[view]):
            result = provider_pty.wait_for(
                "unused", "s", "g",
                lambda current: "PIGADE_DSR_OK" in "".join(cell["text"] for cell in current["frame"]["cells"]),
                0.1, allowed_states=("running", "exited"),
            )
        self.assertEqual(result["state"], "exited")
        with mock.patch.object(provider_pty, "request", return_value=[view]):
            with self.assertRaisesRegex(provider_pty.DiagnosticError, "session stopped"):
                provider_pty.wait_for("unused", "s", "g", lambda _: True, 0.1)

    def test_long_state_path_uses_separate_short_runtime_socket(self):
        state = Path("/tmp") / ("provider-state-" + "x" * 150)
        runtime = Path("/tmp/pga-short")
        argv = provider_pty.host_argv("/private/host", state, runtime)
        self.assertEqual(argv, ["/private/host", "serve", str(state), "--runtime", str(runtime)])
        self.assertLess(len(os.fsencode(runtime / "host.sock")), 108)
        self.assertGreater(len(os.fsencode(state / "host.sock")), 108)
        runtime_dir = provider_pty.create_short_runtime_directory()
        try:
            self.assertEqual(runtime_dir.stat().st_mode & 0o777, 0o700)
            self.assertLess(len(os.fsencode(runtime_dir / "host.sock")), 108)
        finally:
            provider_pty.shutil.rmtree(runtime_dir)

    def test_identity_capture_failure_never_returns_a_pid_fallback(self):
        with mock.patch.object(provider_pty, "process_identity", side_effect=ProcessLookupError):
            self.assertIsNone(provider_pty.process_identity_if_present(123456789))

    def test_ambiguous_launch_is_reconciled_without_reissuing_mutation(self):
        spec = {"session": "session-a", "generation": "generation-a", "operation": "op-a"}
        calls = []

        def fake_request(_endpoint, command, timeout=3):
            calls.append(command["command"])
            if command["command"] == "launch":
                raise provider_pty.IpcFailure("connection lost")
            return [{"session": "session-a", "generation": "generation-a", "pid": 42}]

        with mock.patch.object(provider_pty, "request", side_effect=fake_request):
            view, resolution = provider_pty.launch_once("unused", spec, sleep=lambda _: None)
        self.assertEqual(calls, ["launch", "list"])
        self.assertEqual(resolution, "launch_reconciled_by_identity")
        self.assertEqual(view["pid"], 42)

    def test_truncated_post_send_response_is_uncertain_and_reconciled_read_only(self):
        with tempfile.TemporaryDirectory() as temporary:
            endpoint = Path(temporary) / "fixture.sock"
            server = socket.socket(socket.AF_UNIX)
            server.bind(str(endpoint))
            server.listen(1)

            def serve_truncated():
                connection, _ = server.accept()
                with connection:
                    header = connection.recv(4)
                    length = int.from_bytes(header, "little")
                    remaining = length
                    while remaining:
                        chunk = connection.recv(remaining)
                        if not chunk:
                            break
                        remaining -= len(chunk)
                    connection.sendall((20).to_bytes(4, "little") + b"{\"protocol\":")
                server.close()

            worker = threading.Thread(target=serve_truncated, daemon=True)
            worker.start()
            with self.assertRaises(provider_pty.IpcFailure) as caught:
                provider_pty.request(endpoint, {"command": "launch", "spec": {"session": "s"}})
            self.assertEqual(caught.exception.failure_code, "malformed_response_frame")
            self.assertEqual(caught.exception.transport_error_type, "ValueError")
            worker.join(timeout=2)
            self.assertFalse(worker.is_alive())

        spec = {"session": "session-truncated", "generation": "generation-truncated", "operation": "op"}
        calls = []

        def malformed_launch_then_list(_endpoint, command, timeout=3):
            calls.append(command["command"])
            if command["command"] == "launch":
                raise provider_pty.IpcFailure("truncated post-send response")
            return [{"session": spec["session"], "generation": spec["generation"], "pid": 99}]

        with mock.patch.object(provider_pty, "request", side_effect=malformed_launch_then_list):
            view, resolution = provider_pty.launch_once("unused", spec)
        self.assertEqual(calls, ["launch", "list"])
        self.assertEqual(resolution, "launch_reconciled_by_identity")
        self.assertEqual(view["pid"], 99)

    def test_only_explicit_queue_full_marker_is_a_pre_admission_rejection(self):
        spec = {"session": "session-q", "generation": "generation-q", "operation": "op"}
        calls = []

        def full(_endpoint, command, timeout=3):
            calls.append(command["command"])
            raise provider_pty.IpcFailure("queue full", rejected_before_admission="queue_full")

        with mock.patch.object(provider_pty, "request", side_effect=full):
            with self.assertRaisesRegex(provider_pty.DiagnosticError, "rejected launch before admission"):
                provider_pty.launch_once("unused", spec)
        self.assertEqual(calls, ["launch"])

    def test_unknown_launch_times_out_without_retry(self):
        spec = {"session": "session-b", "generation": "generation-b", "operation": "op-b"}
        calls = []

        def fake_request(_endpoint, command, timeout=3):
            calls.append(command["command"])
            if command["command"] == "launch":
                raise provider_pty.IpcFailure("response uncertain")
            return []

        with mock.patch.object(provider_pty, "request", side_effect=fake_request):
            with self.assertRaisesRegex(provider_pty.DiagnosticError, "outcome unknown"):
                provider_pty.launch_once("unused", spec, reconcile_seconds=0.05, sleep=lambda _: time.sleep(0.01))
        self.assertEqual(calls[0], "launch")
        self.assertEqual(calls.count("launch"), 1)
        self.assertGreaterEqual(calls.count("list"), 1)

    @unittest.skipUnless(sys.platform == "linux" and hasattr(os, "pidfd_open")
                         and hasattr(signal, "pidfd_send_signal"),
                         "process identity cleanup requires Linux pidfd support")
    def test_cleanup_signal_targets_only_exact_synthetic_child(self):
        first = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"], start_new_session=True)
        second = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"], start_new_session=True)
        try:
            first_identity = provider_pty.process_identity(first.pid)
            second_identity = provider_pty.process_identity(second.pid)
            pidfd_open = provider_pty.os.pidfd_open
            pidfd_signal = provider_pty.signal.pidfd_send_signal
            with mock.patch.object(provider_pty.os, "pidfd_open", wraps=pidfd_open) as opened, \
                 mock.patch.object(provider_pty.signal, "pidfd_send_signal", wraps=pidfd_signal) as sent, \
                 mock.patch.object(provider_pty.os, "killpg", side_effect=AssertionError("must not signal a process group")):
                self.assertTrue(provider_pty.signal_owned(first_identity, signal.SIGTERM))
                opened.assert_called_once_with(first.pid, 0)
                sent.assert_called_once()
            first.wait(timeout=2)
            self.assertIsNone(second.poll())
            self.assertFalse(provider_pty.signal_owned(("wrong-boot", second.pid, second_identity[2]), signal.SIGTERM))
            self.assertIsNone(second.poll())
        finally:
            for process in (first, second):
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=2)

    def test_frozen_host_copy_is_verified_against_captured_source_hash(self):
        with tempfile.TemporaryDirectory() as temporary:
            source, destination = Path(temporary) / "host", Path(temporary) / "frozen"
            source.write_bytes(b"host bytes captured")
            expected = hashlib.sha256(source.read_bytes()).hexdigest()
            source_hash, frozen_hash = provider_pty.freeze_host_binary(source, destination, expected_sha256=expected)
            self.assertEqual(source_hash, expected)
            self.assertEqual(frozen_hash, expected)
            self.assertEqual(destination.stat().st_mode & 0o777, 0o700)

            def raced_copy(_source, target):
                Path(target).write_bytes(b"different host bytes")

            with mock.patch.object(provider_pty.shutil, "copyfile", side_effect=raced_copy):
                with self.assertRaisesRegex(provider_pty.DiagnosticError, "changed while creating"):
                    provider_pty.freeze_host_binary(source, destination, expected_sha256=expected)

    def test_failure_metadata_keeps_only_redacted_frame_summary(self):
        summary = {
            "state": "running", "sequence": 7,
            "geometry": {"rows": 24, "cols": 80}, "cell_count": 1920,
            "mode_bits": 0, "output_bytes": 128, "events": 4,
            "storage_error_present": False,
        }
        error = provider_pty.DiagnosticError(
            "private provider output must not be serialized",
            failure_code="session_stopped", host_state="exited",
        )
        selection = {
            "mode": "explicit_elf_override", "launcher_sha256": "wrapper-hash",
            "launcher_kind": "script_or_other", "selected_sha256": "actual-hash",
            "selected_kind": "elf", "single_file_mount": True,
        }
        metadata = provider_pty.failure_metadata(
            "codex", "harness-hash", "p-hash", "h-source", "h-copy", "provider_startup",
            error,
            [{"stage": "provider_startup", **summary}],
            provider_launcher_sha256="wrapper-hash", provider_selection=selection,
        )
        encoded = json.dumps(metadata)
        self.assertIn("provider_startup", encoded)
        self.assertIn("output_bytes", encoded)
        self.assertNotIn("private provider output", encoded)
        self.assertNotIn("text", metadata["last_frame_summaries"][0])
        self.assertFalse(metadata["raw_provider_output_included"])
        self.assertEqual(metadata["failure_code"], "session_stopped")
        self.assertEqual(metadata["host_state"], "exited")
        self.assertEqual(metadata["provider_selection"]["selected_sha256"], "actual-hash")
        self.assertEqual(metadata["harness_source_sha256"], "harness-hash")

    def test_request_size_rejection_is_known_pre_send_not_ipc_ambiguity(self):
        oversized = {"command": "x" * (provider_pty.MAX_IPC + 1)}
        with mock.patch.object(provider_pty.socket, "socket", side_effect=AssertionError("socket must not open")):
            with self.assertRaisesRegex(provider_pty.DiagnosticError, "request exceeds"):
                provider_pty.request("unused", oversized)

    def test_private_directory_refuses_repo_path_before_creating_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            root.mkdir()
            candidate = root / "provider-pty"
            with mock.patch.object(provider_pty, "ROOT", root):
                with self.assertRaisesRegex(provider_pty.DiagnosticError, "outside the repository"):
                    provider_pty.check_private_directory(candidate)
            self.assertFalse(candidate.exists())


if __name__ == "__main__":
    unittest.main()
