"""Reject stale renderer artifacts before opening a compositor or app."""
import hashlib
import importlib.util
import json
from pathlib import Path
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


if __name__ == '__main__':
    unittest.main()
