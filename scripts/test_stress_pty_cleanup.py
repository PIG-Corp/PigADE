"""Fail-closed ownership and evidence-retention tests for stress-pty cleanup."""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import signal
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name('stress-pty.py')
SPEC = importlib.util.spec_from_file_location('stress_pty_cleanup_tests', SCRIPT)
assert SPEC is not None and SPEC.loader is not None
STRESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STRESS)


class StressPtyCleanupTests(unittest.TestCase):
    def test_owned_signal_uses_pidfd_after_captured_identity_revalidation(self):
        expected = ('boot', 1234, 5678)
        with mock.patch.object(STRESS.os, 'pidfd_open', return_value=91, create=True) as opened, \
                mock.patch.object(STRESS.signal, 'pidfd_send_signal', create=True) as sent, \
                mock.patch.object(STRESS, 'identity', return_value=expected) as identity, \
                mock.patch.object(STRESS.os, 'close') as close, \
                mock.patch.object(STRESS.os, 'killpg', side_effect=AssertionError('no process group')):
            self.assertTrue(STRESS.signal_owned_identity(expected, signal.SIGTERM))
        opened.assert_called_once_with(1234, 0)
        identity.assert_called_once_with(1234)
        sent.assert_called_once_with(91, signal.SIGTERM)
        close.assert_called_once_with(91)

    def test_stale_identity_never_signals_pidfd_target_or_falls_back_to_pid(self):
        expected = ('boot', 1234, 5678)
        replacement = ('boot', 1234, 9999)
        with mock.patch.object(STRESS.os, 'pidfd_open', return_value=91, create=True), \
                mock.patch.object(STRESS.signal, 'pidfd_send_signal', create=True) as sent, \
                mock.patch.object(STRESS, 'identity', return_value=replacement), \
                mock.patch.object(STRESS.os, 'close'), \
                mock.patch.object(STRESS.os, 'kill', side_effect=AssertionError('raw pid signal')), \
                mock.patch.object(STRESS.os, 'killpg', side_effect=AssertionError('process-group signal')):
            self.assertFalse(STRESS.signal_owned_identity(expected, signal.SIGKILL))
        sent.assert_not_called()

    def test_missing_pidfd_support_fails_closed_without_pid_signal(self):
        expected = ('boot', 1234, 5678)
        with mock.patch.object(STRESS.os, 'pidfd_open', new=None, create=True), \
                mock.patch.object(STRESS.signal, 'pidfd_send_signal', create=True) as sent, \
                mock.patch.object(STRESS.os, 'kill', side_effect=AssertionError('raw pid signal')):
            self.assertFalse(STRESS.signal_owned_identity(expected, signal.SIGKILL))
        sent.assert_not_called()

    def test_cleanup_records_pid_replacement_without_signaling_replacement(self):
        expected = ('boot', 1234, 5678)
        replacement = ('boot', 1234, 9999)
        with mock.patch.object(STRESS, 'identity_if_present', return_value=replacement), \
                mock.patch.object(STRESS, 'signal_owned_identity') as signal_owned:
            failures = STRESS.cleanup_owned_processes(None, None, [expected])
        self.assertEqual(failures, ['producer_identity_changed_not_signaled'])
        signal_owned.assert_not_called()

    def test_unknown_proc_state_is_not_treated_as_confirmed_exit(self):
        expected = ('boot', 1234, 5678)
        for failure in (PermissionError('proc state unreadable'), ValueError('malformed stat'),
                        IndexError('truncated stat')):
            with self.subTest(failure=type(failure).__name__), \
                    mock.patch.object(STRESS, 'identity_if_present', return_value=expected), \
                    mock.patch.object(STRESS, 'process_state', side_effect=failure):
                self.assertFalse(STRESS.wait_owned_exit(expected, 0.01))

    def test_identity_read_failure_is_reported_and_retains_run_state(self):
        expected = ('boot', 1234, 5678)
        with tempfile.TemporaryDirectory() as parent:
            run_directory = STRESS.RetainableTemporaryDirectory(prefix='stress-', dir=parent)
            with run_directory as temporary:
                marker = Path(temporary) / 'journal.fixture'
                marker.write_bytes(b'partial recovery evidence')
                with mock.patch.object(STRESS, 'identity_if_present',
                                       side_effect=PermissionError('proc unreadable')):
                    failures = STRESS.cleanup_owned_processes(None, None, [expected])
                self.assertEqual(failures, ['producer_identity_unavailable'])
                run_directory.retain = bool(failures)
            self.assertTrue(marker.exists())
            self.assertEqual(marker.read_bytes(), b'partial recovery evidence')

    def test_cleanup_failure_retains_owned_run_evidence(self):
        expected = ('boot', 1234, 5678)
        with tempfile.TemporaryDirectory() as parent:
            run_directory = STRESS.RetainableTemporaryDirectory(prefix='stress-', dir=parent)
            with run_directory as temporary:
                state = Path(temporary)
                marker = state / 'journal.fixture'
                marker.write_bytes(b'synthetic journal evidence')
                with mock.patch.object(STRESS, 'identity_if_present', return_value=expected), \
                        mock.patch.object(STRESS, 'signal_owned_identity', return_value=False):
                    failures = STRESS.cleanup_owned_processes(None, None, [expected])
                self.assertEqual(failures, ['producer_pidfd_sigkill_failed'])
                run_directory.retain = bool(failures)
            self.assertTrue(marker.exists())
            self.assertEqual(marker.read_bytes(), b'synthetic journal evidence')

    def test_success_cleanup_deletes_only_its_fresh_temporary_directory(self):
        with tempfile.TemporaryDirectory() as parent:
            run_directory = STRESS.RetainableTemporaryDirectory(prefix='stress-', dir=parent)
            with run_directory as temporary:
                state = Path(temporary)
                marker = state / 'owned'
                marker.write_text('owned')
            self.assertFalse(state.exists())


if __name__ == '__main__':
    unittest.main()
