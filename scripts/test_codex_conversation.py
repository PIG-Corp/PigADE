import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "codex_conversation", ROOT / "scripts/codex-conversation.py"
)
codex_conversation = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = codex_conversation
SPEC.loader.exec_module(codex_conversation)


THREAD_ID = "0199a213-81c0-7800-8aa1-bbab2a035a53"
NONCE = "PIGADECTX-test-fixture-001"


def jsonl(*events):
    return b"".join(json.dumps(event, separators=(",", ":")).encode() + b"\n" for event in events)


def completed_events(message, thread_id=None):
    events = []
    if thread_id is not None:
        events.append({"type": "thread.started", "thread_id": thread_id})
    events.extend(
        [
            {"type": "turn.started"},
            {"type": "item.completed", "item": {"id": "item_1", "type": "agent_message", "text": message}},
            {"type": "turn.completed"},
        ]
    )
    return jsonl(*events)


class CodexConversationTests(unittest.TestCase):
    def test_default_plan_does_not_spawn_cli(self):
        output = io.StringIO()
        with mock.patch.object(
            codex_conversation.subprocess, "Popen", side_effect=AssertionError("spawned")
        ), contextlib.redirect_stdout(output):
            self.assertEqual(codex_conversation.main([]), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan["mode"], "plan_only")
        self.assertFalse(plan["spawned_processes"])
        self.assertTrue(plan["requires_new_private_directory_outside_repository"])

    def test_run_requires_explicit_network_auth_ack_and_new_directory(self):
        with self.assertRaises(SystemExit):
            codex_conversation.main(["--run"])
        with self.assertRaises(SystemExit):
            codex_conversation.main(["--run", "--acknowledge-native-auth-and-network"])

    def test_builders_use_json_readonly_and_exact_thread_resume(self):
        cwd = Path("/private/pigade/run/cwd")
        first = codex_conversation.build_initial_argv("/usr/bin/codex", cwd, "safe first")
        resume = codex_conversation.build_resume_argv(
            "/usr/bin/codex", cwd, THREAD_ID, "safe followup"
        )
        for argv in (first, resume):
            self.assertEqual(argv[1:3], ["--no-daemon", "exec"])
            self.assertIn("--json", argv)
            self.assertIn("read-only", argv)
            self.assertIn("--ignore-user-config", argv)
            self.assertIn("--ignore-rules", argv)
            self.assertNotIn("--ephemeral", argv)
            self.assertNotIn("--last", argv)
        self.assertEqual(resume[resume.index("resume") + 1], THREAD_ID)
        self.assertNotIn("fork", resume)

    def test_child_environment_removes_desktop_routes_but_preserves_native_auth_env(self):
        source = {
            "PATH": "/usr/bin:/bin",
            "HOME": "/private/home",
            "CODEX_HOME": "/native/codex-home",
            "CODEX_API_KEY": "never-print-or-copy",
            "HTTPS_PROXY": "https://proxy.invalid",
            "XDG_CONFIG_HOME": "/private/config",
            "CODEX_SESSION_ID": "session-control",
            "CODEX_APP_SERVER_SOCKET": "/tmp/codex.sock",
            "CODEX_DAEMON_SOCKET": "/tmp/daemon.sock",
            "XAUTHORITY": "/tmp/xauth",
            "SESSION_MANAGER": "desktop-session",
            "SSH_AUTH_SOCK": "/tmp/ssh-agent.sock",
            "MIR_SOCKET": "/tmp/mir.sock",
            "WAYLAND_SOCKET": "42",
            **{name: "desktop-value" for name in codex_conversation.FORBIDDEN_DESKTOP_ENV},
        }
        child = codex_conversation.child_environment(source)
        self.assertFalse(set(source) - codex_conversation.PRESERVED_ENV & child.keys())
        self.assertFalse(set(codex_conversation.FORBIDDEN_DESKTOP_ENV) & child.keys())
        self.assertEqual(child["CODEX_HOME"], source["CODEX_HOME"])
        self.assertEqual(child["CODEX_API_KEY"], source["CODEX_API_KEY"])
        self.assertEqual(child["HTTPS_PROXY"], source["HTTPS_PROXY"])
        self.assertEqual(child["XDG_CONFIG_HOME"], source["XDG_CONFIG_HOME"])
        self.assertEqual(child["BROWSER"], "/bin/false")
        self.assertNotIn("CODEX_SESSION_ID", child)
        self.assertNotIn("CODEX_APP_SERVER_SOCKET", child)
        self.assertNotIn("CODEX_DAEMON_SOCKET", child)
        self.assertNotIn("SSH_AUTH_SOCK", child)

    @unittest.skipUnless(shutil.which("/usr/bin/python3"), "requires /usr/bin/python3")
    def test_invoke_drains_all_pipe_bytes_after_fast_child_exit_and_closes_fds(self):
        python = "/usr/bin/python3"
        script = (
            "import os; "
            "[(os.write(1, chunk), os.write(2, b'e'+bytes([48+i]))) "
            "for i, chunk in enumerate((b'first-', b'second-', b'final\\n'))]"
        )
        before = len(os.listdir("/proc/self/fd")) if Path("/proc/self/fd").exists() else None
        result = codex_conversation.invoke(
            [python, "-c", script], Path("/tmp"), codex_conversation.child_environment({}), timeout=2
        )
        after = len(os.listdir("/proc/self/fd")) if before is not None else None
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b"first-second-final\n")
        self.assertEqual(result.stderr, b"e0e1e2")
        if before is not None:
            self.assertEqual(after, before)

    def test_phase_intent_persists_before_each_exact_spawn(self):
        calls = []
        phases = []

        def persist(phase, thread_hash):
            phases.append((phase, thread_hash))

        def fake_invoker(argv, cwd, environment):
            calls.append(argv)
            self.assertEqual(len(phases), len(calls))
            if len(calls) == 1:
                self.assertEqual(phases[-1], ("initial_spawn_intent", None))
                return codex_conversation.Invocation(0, completed_events(codex_conversation.ACK, THREAD_ID), b"")
            self.assertEqual(phases[-1][0], "resume_spawn_intent")
            self.assertEqual(phases[-1][1], codex_conversation.sha256_bytes(THREAD_ID.encode()))
            return codex_conversation.Invocation(0, completed_events(NONCE, THREAD_ID), b"")

        codex_conversation.run_conversation(
            "codex", "d" * 64, Path("/private/cwd"), {}, invoker=fake_invoker,
            nonce=NONCE, phase_callback=persist,
        )
        self.assertEqual(len(calls), 2)
        self.assertNotIn(THREAD_ID, json.dumps(phases))

    def test_run_mode_persists_hash_version_and_intent_before_each_spawn(self):
        with tempfile.TemporaryDirectory(prefix="pigade-run-mode-") as parent:
            destination = Path(parent) / "run"
            observed = []
            nonce = None

            def fake_invoke(argv, cwd, environment, **kwargs):
                nonlocal nonce
                result_path = destination / "result.json"
                saved = json.loads(result_path.read_text())
                observed.append((argv, saved.copy()))
                if argv[-1] == "--version":
                    return codex_conversation.Invocation(0, b"codex-cli test-version\n", b"")
                if "resume" in argv:
                    return codex_conversation.Invocation(0, completed_events(nonce, THREAD_ID), b"")
                nonce = argv[-1].split("token ", 1)[1].split(" for my", 1)[0]
                return codex_conversation.Invocation(
                    0, completed_events(codex_conversation.ACK, THREAD_ID), b""
                )

            with mock.patch.object(codex_conversation.shutil, "which", return_value=sys.executable), \
                 mock.patch.object(codex_conversation, "invoke", side_effect=fake_invoke):
                result = codex_conversation.run_mode(destination)
            self.assertEqual([saved["phase"] for _, saved in observed], [
                "version_probe_spawn_intent", "initial_spawn_intent", "resume_spawn_intent"
            ])
            self.assertEqual(result["status"], "passed", result)
            self.assertEqual(result["cli_version"], "codex-cli test-version")
            self.assertEqual(result["harness_source_sha256"], codex_conversation.sha256_file(Path(codex_conversation.__file__)))
            self.assertEqual(result["thread_id_sha256"], codex_conversation.sha256_bytes(THREAD_ID.encode()))
            self.assertNotIn("thread_id", result)
            self.assertEqual((destination / "result.json").stat().st_mode & 0o777, 0o600)

    def test_initial_jsonl_requires_one_uuid_thread_and_ack(self):
        thread, seen = codex_conversation.validate_initial_output(
            completed_events(codex_conversation.ACK, THREAD_ID)
        )
        self.assertEqual(thread, THREAD_ID)
        self.assertTrue(seen)
        for payload in (
            completed_events(codex_conversation.ACK),
            completed_events(codex_conversation.ACK, "not-a-uuid"),
            jsonl(
                {"type": "thread.started", "thread_id": THREAD_ID},
                {"type": "turn.failed"},
            ),
            completed_events("wrong answer", THREAD_ID),
        ):
            with self.assertRaises(codex_conversation.ConversationError):
                codex_conversation.validate_initial_output(payload)

    def test_resume_requires_context_nonce_and_rejects_new_thread(self):
        self.assertTrue(
            codex_conversation.validate_resume_output(
                completed_events(NONCE, THREAD_ID), THREAD_ID, NONCE
            )
        )
        self.assertIsNone(
            codex_conversation.validate_resume_output(
                completed_events(NONCE), THREAD_ID, NONCE
            )
        )
        with self.assertRaisesRegex(codex_conversation.ConversationError, "context_nonce_mismatch"):
            codex_conversation.validate_resume_output(
                completed_events("new thread", THREAD_ID), THREAD_ID, NONCE
            )
        with self.assertRaisesRegex(codex_conversation.ConversationError, "thread_id_changed"):
            codex_conversation.validate_resume_output(
                completed_events(NONCE, "0199a213-81c0-7800-8aa1-bbab2a035a54"),
                THREAD_ID,
                NONCE,
            )

    def test_jsonl_parser_enforces_size_event_and_object_bounds(self):
        self.assertEqual(codex_conversation.parse_jsonl(b'{"type":"turn.completed"}\n'), [{"type": "turn.completed"}])
        cases = (
            (b"not-json\n", "malformed_jsonl"),
            (b"[]\n", "event_not_object"),
            (b"x" * (codex_conversation.MAX_LINE + 1) + b"\n", "line_limit"),
            (b" " * (codex_conversation.MAX_STDOUT + 1), "stdout_limit"),
        )
        for payload, code in cases:
            with self.subTest(code=code), self.assertRaises(codex_conversation.ConversationError) as ctx:
                codex_conversation.parse_jsonl(payload)
            self.assertEqual(ctx.exception.code, code)
        too_many = b'{"type":"unknown"}\n' * (codex_conversation.MAX_EVENTS + 1)
        with self.assertRaisesRegex(codex_conversation.ConversationError, "event_limit"):
            codex_conversation.parse_jsonl(too_many)

    def test_tool_item_is_rejected_even_if_turn_completes(self):
        payload = jsonl(
            {"type": "thread.started", "thread_id": THREAD_ID},
            {"type": "item.completed", "item": {"type": "command_execution", "command": "pwd"}},
            {"type": "turn.completed"},
        )
        with self.assertRaisesRegex(codex_conversation.ConversationError, "unexpected_non_message_item"):
            codex_conversation.validate_initial_output(payload)

    def test_reasoning_items_are_ignored_but_reply_must_be_unique(self):
        payload = jsonl(
            {"type": "thread.started", "thread_id": THREAD_ID},
            {"type": "item.completed", "item": {"type": "reasoning", "summary": "private"}},
            {"type": "item.completed", "item": {"type": "agent_message", "text": codex_conversation.ACK}},
            {"type": "turn.completed"},
        )
        self.assertEqual(codex_conversation.validate_initial_output(payload), (THREAD_ID, True))
        duplicate = jsonl(
            {"type": "thread.started", "thread_id": THREAD_ID},
            {"type": "item.completed", "item": {"type": "agent_message", "text": codex_conversation.ACK}},
            {"type": "item.completed", "item": {"type": "agent_message", "text": codex_conversation.ACK}},
            {"type": "turn.completed"},
        )
        with self.assertRaisesRegex(codex_conversation.ConversationError, "assistant_message_missing_or_ambiguous"):
            codex_conversation.validate_initial_output(duplicate)

    def test_unknown_initial_launch_never_retries_or_resumes(self):
        invocations = []

        def fake_invoker(argv, cwd, environment):
            invocations.append(argv)
            return codex_conversation.Invocation(124, b"", b"", timed_out=True)

        with self.assertRaises(codex_conversation.ConversationError):
            codex_conversation.run_conversation(
                "codex", "a" * 64, Path("/private/cwd"), {}, invoker=fake_invoker, nonce=NONCE
            )
        self.assertEqual(len(invocations), 1)
        self.assertNotIn("resume", invocations[0])

    def test_successful_flow_uses_same_native_id_and_nonce_only_from_prior_turn(self):
        invocations = []

        def fake_invoker(argv, cwd, environment):
            invocations.append(argv)
            if len(invocations) == 1:
                return codex_conversation.Invocation(
                    0, completed_events(codex_conversation.ACK, THREAD_ID), b""
                )
            return codex_conversation.Invocation(0, completed_events(NONCE, THREAD_ID), b"")

        result = codex_conversation.run_conversation(
            "codex", "c" * 64, Path("/private/cwd"), {}, invoker=fake_invoker, nonce=NONCE
        )
        self.assertEqual(result["status"], "passed")
        self.assertEqual(len(invocations), 2)
        self.assertEqual(invocations[1][invocations[1].index("resume") + 1], THREAD_ID)
        self.assertNotIn(NONCE, invocations[1][-1])
        self.assertEqual(result["thread_id_sha256"], codex_conversation.sha256_bytes(THREAD_ID.encode()))
        self.assertNotIn("thread_id", result)
        self.assertFalse(result["raw_stdout_saved"])

    def test_failed_resume_is_not_retried_or_replaced_with_new_conversation(self):
        invocations = []

        def fake_invoker(argv, cwd, environment):
            invocations.append(argv)
            if len(invocations) == 1:
                return codex_conversation.Invocation(
                    0, completed_events(codex_conversation.ACK, THREAD_ID), b""
                )
            return codex_conversation.Invocation(70, b"", b"sensitive output must not be retained")

        with self.assertRaisesRegex(codex_conversation.ConversationError, "nonzero_exit"):
            codex_conversation.run_conversation(
                "codex", "b" * 64, Path("/private/cwd"), {}, invoker=fake_invoker, nonce=NONCE
            )
        self.assertEqual(len(invocations), 2)
        self.assertEqual(invocations[1][invocations[1].index("resume") + 1], THREAD_ID)

    def test_run_directory_is_private_unique_and_outside_repository(self):
        with tempfile.TemporaryDirectory(prefix="pigade-codex-") as parent:
            destination = Path(parent) / "new-run"
            resolved, cwd = codex_conversation.prepare_run_directory(destination)
            self.assertEqual(resolved.stat().st_mode & 0o777, 0o700)
            self.assertEqual(cwd.stat().st_mode & 0o777, 0o700)
            self.assertFalse(codex_conversation.ROOT in resolved.parents)
            with self.assertRaisesRegex(codex_conversation.ConversationError, "must_be_new"):
                codex_conversation.prepare_run_directory(destination)

    def test_result_file_is_exclusive_and_private(self):
        with tempfile.TemporaryDirectory(prefix="pigade-codex-result-") as parent:
            path = Path(parent) / "result.json"
            codex_conversation.write_private_json(path, {"status": "passed"})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(path.read_text()), {"status": "passed"})
            with self.assertRaises(FileExistsError):
                codex_conversation.write_private_json(path, {"status": "failed"})


if __name__ == "__main__":
    unittest.main()
