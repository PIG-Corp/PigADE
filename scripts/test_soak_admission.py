"""Retry safety at the boundary between rejection and an unknown outcome."""
import io
import importlib.util
import json
import os
import pty
from pathlib import Path
import select
import subprocess
import sys
import time
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "soak_admission", Path(__file__).with_name("soak-pty.py"))
SOAK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SOAK)


class Clock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class AdmissionRetryTests(unittest.TestCase):
    def stats(self):
        return {"attempts": 0, "retries": 0,
                "total_wait_seconds": 0.0, "max_wait_seconds": 0.0}

    def test_only_proven_resize_rejection_can_be_retried(self):
        stats, clock = self.stats(), Clock()
        rejection = SOAK.STRESS.HostRejection("queue_full", "not admitted")
        diagnostics = []
        with patch.object(SOAK.STRESS, "request", side_effect=[rejection, []]) as request, \
                patch.object(SOAK.time, "monotonic", clock.monotonic), \
                patch.object(SOAK.time, "sleep", clock.sleep):
            SOAK.resize_with_admission_retry(Path("/private/host.sock"),
                                             {"command": "resize"}, stats,
                                             on_queue_full=lambda *args: diagnostics.append(args))
        self.assertEqual(request.call_count, 2)
        self.assertEqual(stats["attempts"], 2)
        self.assertEqual(stats["retries"], 1)
        self.assertEqual(stats["total_wait_seconds"], .025)
        self.assertEqual(len(diagnostics), 1)
        self.assertEqual(diagnostics[0][1], 1)

    def test_unknown_or_transport_outcome_is_never_retried(self):
        for error in (TimeoutError("may have been applied"),
                      ConnectionResetError("reply lost"),
                      RuntimeError("unmarked host error"),
                      SOAK.STRESS.HostRejection("future_code", "unknown proof")):
            with self.subTest(error=type(error).__name__):
                on_queue_full = unittest.mock.Mock()
                with patch.object(SOAK.STRESS, "request", side_effect=error) as request, \
                        patch.object(SOAK.time, "sleep") as sleep:
                    with self.assertRaises(type(error)):
                        SOAK.resize_with_admission_retry(Path("/private/host.sock"),
                                                         {"command": "resize"}, self.stats(),
                                                         on_queue_full=on_queue_full)
                self.assertEqual(request.call_count, 1)
                sleep.assert_not_called()
                on_queue_full.assert_not_called()

    def test_input_launch_and_stop_cannot_enter_retry_path(self):
        with patch.object(SOAK.STRESS, "request") as request:
            for command in ("input", "launch", "stop"):
                with self.subTest(command=command):
                    with self.assertRaisesRegex(RuntimeError, "restricted to resize"):
                        SOAK.resize_with_admission_retry(Path("/private/host.sock"),
                                                         {"command": command}, self.stats())
            request.assert_not_called()

    def test_repeated_explicit_rejection_stops_at_deadline(self):
        stats, clock = self.stats(), Clock()
        with patch.object(SOAK.STRESS, "request", side_effect=
                          SOAK.STRESS.HostRejection("queue_full", "not admitted")) as request, \
                patch.object(SOAK.time, "monotonic", clock.monotonic), \
                patch.object(SOAK.time, "sleep", clock.sleep):
            with self.assertRaisesRegex(RuntimeError, "deadline exceeded"):
                SOAK.resize_with_admission_retry(Path("/private/host.sock"),
                                                 {"command": "resize"}, stats,
                                                 deadline_seconds=.04)
        self.assertEqual(request.call_count, 2)
        self.assertEqual(stats["retries"], 2)
        self.assertEqual(clock.now, .04)
        self.assertTrue(all(call.kwargs["timeout"] <= .04
                            for call in request.call_args_list))


class ProducerCommandQueueTests(unittest.TestCase):
    def test_release_and_next_ping_in_one_pty_read_preserve_next_command(self):
        master, slave = pty.openpty()
        child = subprocess.Popen(
            [sys.executable, "-I", "-u", "-c", SOAK.PRODUCER, "0", "30", "257"],
            stdin=slave,
            stdout=slave,
            stderr=subprocess.DEVNULL,
            close_fds=True,
        )
        os.close(slave)
        output = bytearray()

        def wait_for(marker, timeout=3):
            deadline = time.monotonic() + timeout
            while marker not in output:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    self.fail(f"producer did not emit {marker!r}; exit={child.poll()}")
                ready, _, _ = select.select([master], [], [], remaining)
                if ready:
                    output.extend(os.read(master, 8192))

        try:
            wait_for(b"READY:0")
            os.write(master, b"PING:first\n")
            wait_for(b"ACK:first")

            # A fast ACK loop can put the prior token's release and the next
            # token in one PTY read. The consumer must retain lines after the
            # release rather than dropping the following PING.
            os.write(master, b"RELEASE:first\nPING:next\n")
            wait_for(b"ACK:next")

            os.write(master, b"RELEASE:next\nFINISH\n")
            self.assertEqual(child.wait(timeout=3), 0)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=3)
            os.close(master)

    def test_pty_eof_during_ack_wait_exits_instead_of_spinning(self):
        master, slave = pty.openpty()
        child = subprocess.Popen(
            [sys.executable, "-I", "-u", "-c", SOAK.PRODUCER, "0", "30", "257"],
            stdin=slave,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            close_fds=True,
        )
        os.close(slave)
        output = bytearray()

        def wait_for(marker, timeout=3):
            deadline = time.monotonic() + timeout
            while marker not in output:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    self.fail(f"producer did not emit {marker!r}; exit={child.poll()}")
                ready, _, _ = select.select([child.stdout.fileno()], [], [], remaining)
                if ready:
                    output.extend(os.read(child.stdout.fileno(), 8192))

        try:
            wait_for(b"READY:0")
            os.write(master, b"PING:eof\n")
            wait_for(b"ACK:eof")
            os.close(master)
            master = -1
            tail, _ = child.communicate(timeout=3)
            output.extend(tail)
            self.assertEqual(child.returncode, 0)
            self.assertIn(b"PIGADE_ENDCRC:0:", output)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=3)
            if master >= 0:
                os.close(master)


class ResizeAdmissionDiagnosticTests(unittest.TestCase):
    def test_diagnostic_is_throttled_and_excludes_frame_contents(self):
        clock = Clock()
        output = io.StringIO()
        recorder = SOAK.ResizeAdmissionDiagnostics(output, 0.0, 123, 100, 4096)
        command = {"command": "resize", "session": "session-id", "generation": "generation-id",
                   "geometry": {"rows": 32, "cols": 112}}
        response = [{"generation": "generation-id", "state": "running", "sequence": 9,
                     "storage_error": None,
                     "actor_diagnostic": {"version": 1, "sample_available": True,
                         "stage": "history_commit", "candidate_sequence": 10,
                         "entered_monotonic_ns": 100,
                         "reader": {"read_calls": 2, "bytes_read": 20, "last_read_bytes": 10,
                                    "enqueued_calls": 1, "bytes_enqueued": 10,
                                    "last_read_monotonic_ns": 80, "last_enqueued_monotonic_ns": 70},
                         "arbitrary_terminal_text": "SENSITIVE PROBE FIELD"},
                     "metrics": {"events": 5, "output_bytes": 500, "checkpoint_ns": 12,
                                 "secret_metric": "hidden"},
                     "frame": {"cells": [{"text": "SENSITIVE FRAME TEXT"}]}}]
        with patch.object(SOAK.time, "monotonic", clock.monotonic), \
                patch.object(SOAK, "resource_sample", return_value={"host_rss_bytes": 4096,
                                                                     "host_cpu_seconds": 1.25}), \
                patch.object(SOAK.STRESS, "request", return_value=response) as request:
            self.assertTrue(recorder.observe(Path("/private/host.sock"), 2, command, 1, 5.0))
            clock.now = .5
            self.assertFalse(recorder.observe(Path("/private/host.sock"), 2, command, 2, 5.0))
            clock.now = 1.0
            self.assertTrue(recorder.observe(Path("/private/host.sock"), 2, command, 3, 4.0))
        self.assertEqual(request.call_count, 2)
        self.assertEqual(request.call_args.kwargs["timeout"], SOAK.RESIZE_DIAGNOSTIC_SNAPSHOT_TIMEOUT)
        records = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(len(records), 2)
        self.assertEqual(records[-1]["elapsed_monotonic_seconds"], 1.0)
        self.assertEqual(records[-1]["retry_count"], 3)
        self.assertEqual(records[-1]["host_rss_bytes"], 4096)
        self.assertEqual(records[-1]["snapshot"]["metrics"],
                         {"events": 5, "output_bytes": 500, "checkpoint_ns": 12})
        self.assertNotIn("frame", records[-1])
        self.assertNotIn("cells", json.dumps(records))
        self.assertNotIn("SENSITIVE FRAME TEXT", output.getvalue())
        self.assertNotIn("SENSITIVE PROBE FIELD", output.getvalue())
        self.assertEqual(records[-1]["snapshot"]["actor_diagnostic"]["stage"], "history_commit")
        self.assertEqual(records[-1]["snapshot"]["actor_diagnostic"]["candidate_sequence"], 10)

    def test_diagnostic_record_cap_and_omitted_counter_are_hard_bounded(self):
        clock = Clock()
        output = io.StringIO()
        recorder = SOAK.ResizeAdmissionDiagnostics(output, 0.0, 123, 100, 4096)
        command = {"command": "resize", "session": "session-id", "generation": "generation-id",
                   "geometry": {"rows": 32, "cols": 112}}
        with patch.object(SOAK, "MAX_RESIZE_ADMISSION_DIAGNOSTIC_RECORDS", 2), \
                patch.object(SOAK.time, "monotonic", clock.monotonic), \
                patch.object(SOAK, "resource_sample", side_effect=OSError("gone")), \
                patch.object(SOAK.STRESS, "request", return_value=[]):
            for second in range(4):
                clock.now = float(second)
                recorder.observe(Path("/private/host.sock"), 0, command, second + 1, 5.0)
        self.assertEqual(len(output.getvalue().splitlines()), 2)
        summary = recorder.summary()
        self.assertEqual(summary["records_written"], 2)
        self.assertEqual(summary["omitted_records"], 2)
        self.assertEqual(summary["last_diagnostic"]["elapsed_monotonic_seconds"], 3.0)
        self.assertEqual(summary["last_diagnostic"]["host_rss_bytes"], None)

    def test_diagnostic_time_is_taken_from_backoff_without_changing_retry_cadence(self):
        stats, clock = AdmissionRetryTests().stats(), Clock()
        command = {"command": "resize"}
        rejection = SOAK.STRESS.HostRejection("queue_full", "not admitted")

        def diagnostic(_command, _retry_count, _remaining):
            clock.now += .020

        with patch.object(SOAK.STRESS, "request", side_effect=[rejection, []]) as request, \
                patch.object(SOAK.time, "monotonic", clock.monotonic), \
                patch.object(SOAK.time, "sleep", clock.sleep):
            SOAK.resize_with_admission_retry(Path("/private/host.sock"), command, stats,
                                             on_queue_full=diagnostic)
        self.assertEqual(request.call_count, 2)
        self.assertAlmostEqual(clock.now, .025)


class OwnedProcessSignalTests(unittest.TestCase):
    def test_signal_requires_pidfd_and_matching_captured_identity(self):
        identity = ("boot", 1234, 5678)
        with patch.object(SOAK.os, "pidfd_open", return_value=91, create=True) as open_fd, \
                patch.object(SOAK.os, "close") as close_fd, \
                patch.object(SOAK.STRESS, "identity", return_value=identity), \
                patch.object(SOAK.signal, "pidfd_send_signal", create=True) as send:
            self.assertTrue(SOAK.signal_owned_identity(identity, SOAK.signal.SIGTERM))
        open_fd.assert_called_once_with(1234, 0)
        send.assert_called_once_with(91, SOAK.signal.SIGTERM)
        close_fd.assert_called_once_with(91)

    def test_identity_change_or_missing_pidfd_never_signals_by_pid(self):
        identity = ("boot", 1234, 5678)
        with patch.object(SOAK.os, "pidfd_open", return_value=91, create=True), \
                patch.object(SOAK.os, "close") as close_fd, \
                patch.object(SOAK.STRESS, "identity", return_value=("boot", 1234, 9999)), \
                patch.object(SOAK.signal, "pidfd_send_signal", create=True) as send:
            self.assertFalse(SOAK.signal_owned_identity(identity, SOAK.signal.SIGTERM))
        send.assert_not_called()
        close_fd.assert_called_once_with(91)

        with patch.object(SOAK.os, "pidfd_open", create=True, new=None), \
                patch.object(SOAK.signal, "pidfd_send_signal", create=True) as send:
            self.assertFalse(SOAK.signal_owned_identity(identity, SOAK.signal.SIGTERM))
        send.assert_not_called()


if __name__ == "__main__":
    unittest.main()
