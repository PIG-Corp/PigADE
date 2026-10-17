"""Reject stale renderer artifacts before opening a compositor or app."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location(
    'isolated_render_tests', Path(__file__).with_name('isolated-render.py'))
RENDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RENDER)


class EmbeddedFontTests(unittest.TestCase):
    def fixture(self, directory):
        font = b'owned synthetic font payload'
        (directory / 'face.ttf').write_bytes(font)
        manifest = directory / 'manifest.json'
        manifest.write_text(json.dumps({'profile_id': 'test-v1', 'files': [{
            'path': 'face.ttf', 'bytes': len(font),
            'sha256': hashlib.sha256(font).hexdigest()}]}))
        return font, manifest

    def test_stale_binary_cannot_satisfy_current_font_profile(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            font, manifest = self.fixture(directory)
            binary = directory / 'app'
            binary.write_bytes(b'old renderer artifact')
            with self.assertRaisesRegex(RuntimeError, 'does not embed'):
                RENDER.verify_embedded_fonts(binary, manifest)
            binary.write_bytes(b'header' + font + b'trailer')
            checked = RENDER.verify_embedded_fonts(binary, manifest)
            self.assertEqual(checked['profile_id'], 'test-v1')
            self.assertEqual(len(checked['embedded_faces']), 1)

    def test_asset_change_is_rejected_even_if_binary_contains_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            _, manifest = self.fixture(directory)
            changed = b'modified without regenerating manifest'
            (directory / 'face.ttf').write_bytes(changed)
            binary = directory / 'app'
            binary.write_bytes(changed)
            with self.assertRaisesRegex(RuntimeError, 'source differs'):
                RENDER.verify_embedded_fonts(binary, manifest)


class RecoveryWarningTests(unittest.TestCase):
    def test_ime_overlay_is_explicit_and_does_not_change_the_host_command(self):
        plain = RENDER.app_command(False)
        diagnostic = RENDER.app_command(True)
        self.assertEqual(diagnostic[:-1], plain)
        self.assertEqual(diagnostic[-1], '--diagnostic-preedit')
        self.assertNotIn(RENDER.IME_PREEDIT, ' '.join(diagnostic))

    def test_recovery_reason_is_required_for_the_named_journal(self):
        reply = {'recovery': {'issues': [
            {'journal': 'broken.journal', 'message': 'invalid journal header'}], 'omitted': 0}}
        self.assertEqual(RENDER.recovery_issue(reply, 'broken.journal', 'list'),
                         'invalid journal header')
        with self.assertRaisesRegex(RuntimeError, 'did not name'):
            RENDER.recovery_issue(reply, 'other.journal', 'poll')

    def test_recovery_child_ack_is_distinct_from_tty_echo_probe(self):
        probe = '__PIGADE_RECOVERY_PROBE_1__\n'
        child_ack = '__PIGADE_RECOVERY_CHILD_ACK_1__'
        acknowledged = subprocess.run(
            ['/bin/bash', '--noprofile', '--norc', '-c', RENDER.shell_read_loop(True)],
            input=probe, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            timeout=2, check=True)
        echoed_only = subprocess.run(
            ['/bin/bash', '--noprofile', '--norc', '-c', RENDER.shell_read_loop(False)],
            input=probe, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            timeout=2, check=True)
        self.assertIn(child_ack, acknowledged.stdout)
        self.assertNotIn(child_ack, echoed_only.stdout)
        self.assertIn(probe.rstrip(), echoed_only.stdout)

if __name__ == '__main__':
    unittest.main()
