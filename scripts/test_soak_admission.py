"""Retry safety at the boundary between rejection and an unknown outcome."""
import importlib.util
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
        with patch.object(SOAK.STRESS, "request", side_effect=[rejection, []]) as request, \
                patch.object(SOAK.time, "monotonic", clock.monotonic), \
                patch.object(SOAK.time, "sleep", clock.sleep):
            SOAK.resize_with_admission_retry(Path("/private/host.sock"),
                                             {"command": "resize"}, stats)
        self.assertEqual(request.call_count, 2)
        self.assertEqual(stats["attempts"], 2)
        self.assertEqual(stats["retries"], 1)
        self.assertEqual(stats["total_wait_seconds"], .025)

    def test_unknown_or_transport_outcome_is_never_retried(self):
        for error in (TimeoutError("may have been applied"),
                      ConnectionResetError("reply lost"),
                      RuntimeError("unmarked host error"),
                      SOAK.STRESS.HostRejection("future_code", "unknown proof")):
            with self.subTest(error=type(error).__name__):
                with patch.object(SOAK.STRESS, "request", side_effect=error) as request, \
                        patch.object(SOAK.time, "sleep") as sleep:
                    with self.assertRaises(type(error)):
                        SOAK.resize_with_admission_retry(Path("/private/host.sock"),
                                                         {"command": "resize"}, self.stats())
                self.assertEqual(request.call_count, 1)
                sleep.assert_not_called()

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


if __name__ == "__main__":
    unittest.main()
