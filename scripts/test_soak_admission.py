"""Retry safety at the boundary between rejection and an unknown outcome."""
import importlib.util
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
