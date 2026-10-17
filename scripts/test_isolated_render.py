"""Reject stale renderer artifacts before opening a compositor or app."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
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

    def test_selection_fixture_is_explicit_and_has_no_tty_input(self):
        plain = RENDER.app_command(False)
        self.assertEqual(RENDER.app_command(False, True), [*plain, '--diagnostic-selection'])
        self.assertEqual(RENDER.app_command(True, True), [*plain, '--diagnostic-preedit', '--diagnostic-selection'])

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


class PaletteFixtureTests(unittest.TestCase):
    def test_palette_fixture_command_sets_resets_then_sets_final_values(self):
        command = RENDER.palette_fixture_command()
        steps = [
            '4;1;rgb:01/02/03', '10;rgb:01/02/03', '11;rgb:01/02/03',
            '104;1', '110', '111',
            '4;1;rgb:12/34/56', '10;rgb:ab/cd/ef', '11;rgb:09/18/27',
        ]
        offsets = [command.index(step) for step in steps]
        self.assertEqual(offsets, sorted(offsets))
        self.assertEqual(RENDER.PALETTE_FIXTURE_EXPECTED, [
            [1, [0x12, 0x34, 0x56]],
            [256, [0xAB, 0xCD, 0xEF]],
            [257, [0x09, 0x18, 0x27]],
        ])
        result = subprocess.run(
            ['/bin/bash', '--noprofile', '--norc', '-c', command + "printf 'DONE\\n'"],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=False, timeout=2, check=True)
        self.assertTrue(result.stdout.startswith(b'\x1b]4;1;rgb:01/02/03\x07'))
        self.assertTrue(result.stdout.endswith(b'DONE\n'))

    def test_fixture_is_opt_in_and_targets_named_and_indexed_samples(self):
        plain = RENDER.shell_fixture(0)
        palette = RENDER.shell_fixture(0, palette_fixture=True)
        self.assertNotIn(']4;1;', plain)
        self.assertNotIn('NAMED-INDEX-1', plain)
        self.assertLess(palette.index('4;1;rgb:01/02/03'), palette.index(']2;Bash 1'))
        self.assertIn(r'\033[31mNAMED-INDEX-1\033[0m', palette)
        self.assertIn(r'\033[38;5;1mINDEXED-1\033[0m', palette)
        self.assertTrue(palette.endswith(RENDER.shell_read_loop(False)))
        self.assertEqual(RENDER.app_command(False), ['/pigade/app', '/artifacts/state/host.sock'])

    def test_cli_flag_is_explicit_and_opt_in(self):
        help_text = subprocess.run(
            [sys.executable, str(Path(__file__).with_name('isolated-render.py')), '--help'],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, timeout=2, check=True).stdout
        self.assertIn('--palette-fixture', help_text)


class CursorFixtureTests(unittest.TestCase):
    def test_cursor_commands_emit_exact_shape_and_visibility_sequences(self):
        expected = [b'\x1b[2 q', b'\x1b[4 q', b'\x1b[6 q', b'\x1b[?25l']
        for index, sequence in enumerate(expected):
            command = RENDER.cursor_fixture_command(index)
            result = subprocess.run(
                ['/bin/bash', '--noprofile', '--norc', '-c', command],
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=False, timeout=2, check=True)
            self.assertEqual(result.stdout, sequence)
        for invalid in (-1, 4):
            with self.assertRaisesRegex(RuntimeError, 'between 0 and 3'):
                RENDER.cursor_fixture_command(invalid)

    def test_fixture_is_opt_in_and_starts_after_palette_setup_before_text(self):
        plain = RENDER.shell_fixture(0)
        fixture = RENDER.shell_fixture(2, palette_fixture=True, cursor_fixture=True)
        self.assertNotIn(r'\033[6 q', plain)
        self.assertLess(fixture.index('4;1;rgb:01/02/03'), fixture.index(r'\033[6 q'))
        self.assertLess(fixture.index(r'\033[6 q'), fixture.index(']2;Bash 3'))
        self.assertIn(r'\033[?25l', RENDER.shell_fixture(3, cursor_fixture=True))
        self.assertEqual(RENDER.CURSOR_FIXTURE_EXPECTED, [
            {'shape': 'block', 'blinking': False},
            {'shape': 'underline', 'blinking': False},
            {'shape': 'beam', 'blinking': False},
            {'shape': 'hidden', 'blinking': False},
        ])
        self.assertEqual(RENDER.CURSOR_FIXTURE_SESSIONS,
                         sorted(RENDER.CURSOR_FIXTURE_SESSIONS))
        self.assertEqual(len(set(RENDER.CURSOR_FIXTURE_SESSIONS)), 4)

    def test_app_selection_and_reduced_motion_arguments_are_opt_in(self):
        plain = RENDER.app_command(False)
        self.assertEqual(plain, ['/pigade/app', '/artifacts/state/host.sock'])
        self.assertEqual(RENDER.app_command(False, selected_pane=2),
                         [*plain, '--diagnostic-selected=2'])
        self.assertEqual(RENDER.app_command(False, reduced_motion=True),
                         [*plain, '--reduced-motion'])
        self.assertEqual(RENDER.app_command(True, True, 3, True), [
            *plain, '--diagnostic-preedit', '--diagnostic-selection',
            '--diagnostic-selected=3', '--reduced-motion'])

    def test_cursor_cli_options_are_bounded_and_recorded_as_opt_in(self):
        help_text = subprocess.run(
            [sys.executable, str(Path(__file__).with_name('isolated-render.py')), '--help'],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, timeout=2, check=True).stdout
        self.assertIn('--cursor-fixture', help_text)


class DecorationFixtureTests(unittest.TestCase):
    @staticmethod
    def view_fixture():
        def cells(text, flags=None):
            flags = flags or {}
            return [{'text': char, 'flags': flags.get(index, 0), 'underline': None}
                    for index, char in enumerate(text)]

        rows = []
        styles = cells('S D C P A T', {0: 1 << 3, 2: 1 << 11, 4: 1 << 12,
                                       6: 1 << 13, 8: 1 << 14, 10: 1 << 9})
        rows.append(styles)
        color = cells('U', {0: 1 << 3})
        color[0]['underline'] = 'Spec(Rgb { r: 17, g: 238, b: 119 })'
        rows.append(color)
        wide = cells('漢B', {0: (1 << 5) | (1 << 3)})
        wide.insert(1, {'text': '', 'flags': 1 << 6, 'underline': None})
        rows.append(wide)
        spaces = cells('L   R', {1: 1 << 3, 2: 1 << 3, 3: 1 << 3})
        rows.append(spaces)
        hidden = cells('VMASKEDW', {i: 1 << 8 for i in range(1, 7)})
        rows.append(hidden)
        for row in rows:
            row.extend({'text': ' ', 'flags': 0, 'underline': None}
                       for _ in range(80 - len(row)))
        return {'frame': {'geometry': {'cols': 80, 'rows': len(rows)},
                          'cells': [cell for row in rows for cell in row]}}

    def test_fixture_is_opt_in_small_and_emits_each_style_from_the_shell(self):
        plain = RENDER.shell_fixture(0)
        fixture = RENDER.shell_fixture(0, decoration_fixture=True)
        self.assertNotIn(r'\033[4:2m', plain)
        self.assertIn(r'\033[4:2m', fixture)
        self.assertIn(r'\033[4:3m', fixture)
        self.assertIn(r'\033[4:4m', fixture)
        self.assertIn(r'\033[4:5m', fixture)
        self.assertIn(r'\033[4;58:2::17:238:119mU', fixture)
        self.assertIn(r'\033[8mMASKED\033[28m', fixture)
        self.assertNotIn('PigADE · Bash', fixture)
        self.assertTrue(fixture.endswith(RENDER.shell_read_loop(False)))
        self.assertEqual(fixture.count(r'\r\n'), 5)
        emitted = subprocess.run(
            ['/bin/bash', '--noprofile', '--norc', '-c',
             RENDER.decoration_fixture_command() + "printf 'DONE\\n'"],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=False, timeout=2, check=True).stdout
        for sequence in (b'\x1b[4mS', b'\x1b[4:2mD', b'\x1b[4:3mC',
                         b'\x1b[4:4mP', b'\x1b[4:5mA', b'\x1b[9mT',
                         b'\x1b[4;58:2::17:238:119mU', b'\x1b[8mMASKED'):
            self.assertIn(sequence, emitted)
        self.assertTrue(emitted.endswith(b'DONE\n'))

    def test_typed_host_decoration_metadata_validator(self):
        self.assertTrue(RENDER.decoration_fixture_valid(self.view_fixture()))
        malformed = self.view_fixture()
        malformed['frame']['cells'][0]['flags'] = 0
        self.assertFalse(RENDER.decoration_fixture_valid(malformed))

    def test_cli_fixture_flag_is_opt_in(self):
        help_text = subprocess.run(
            [sys.executable, str(Path(__file__).with_name('isolated-render.py')), '--help'],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, timeout=2, check=True).stdout
        self.assertIn('--decoration-fixture', help_text)
        self.assertIn('--selected-pane', help_text)
        self.assertIn('--reduced-motion', help_text)
        rejected = subprocess.run(
            [sys.executable, str(Path(__file__).with_name('isolated-render.py')),
             '--selected-pane', '4', '--output', '/tmp/not-created'],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, timeout=2, check=False)
        self.assertEqual(rejected.returncode, 2)

if __name__ == '__main__':
    unittest.main()
