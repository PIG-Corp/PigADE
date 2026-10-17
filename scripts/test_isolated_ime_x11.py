"""Tests for the opt-in-only, fixed-sequence private X11/XIM harness."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import pty
import socket
from pathlib import Path
import subprocess
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock


SCRIPT = Path(__file__).with_name('isolated-ime-x11.py')
SPEC = importlib.util.spec_from_file_location('isolated_ime_x11_tests', SCRIPT)
assert SPEC is not None and SPEC.loader is not None
IME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IME)


class IsolatedImePlanTests(unittest.TestCase):
    def test_default_mode_is_plan_only_and_never_spawns(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')) as popen, \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')) as run, \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main([]), 0)
        value = json.loads(output.getvalue())
        self.assertEqual(value['status'], 'plan-only-no-process-no-write')
        self.assertEqual(value['run_requires_explicit_flag'], '--run')
        self.assertFalse(value['input']['arbitrary_text_argument'])
        popen.assert_not_called(); run.assert_not_called()

    def test_publication_trace_adds_only_the_fixed_app_diagnostic_flag(self):
        self.assertEqual(IME.app_command(), ['/pigade/app', '/artifacts/state/host.sock'])
        self.assertEqual(IME.app_command(True), ['/pigade/app', '/artifacts/state/host.sock',
                                                 '--diagnostic-publication-trace'])
        self.assertEqual(IME.app_command('arbitrary'), IME.app_command())

    def test_real_run_requires_a_new_external_output_directory(self):
        result = subprocess.run([os.environ.get('PYTHON', '/usr/bin/python3'), str(SCRIPT), '--run'],
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, timeout=3, check=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn('requires a new private --output path', result.stderr)

    def test_output_guard_rejects_repository_and_existing_or_symlink_paths(self):
        with self.assertRaisesRegex(RuntimeError, 'outside the repository'):
            IME.private_path(IME.ROOT / 'tmp')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            existing = root / 'existing'; existing.mkdir()
            with self.assertRaisesRegex(RuntimeError, 'already exists'):
                IME.private_path(existing, must_not_exist=True)
            target = root / 'target'; target.mkdir()
            link = root / 'link'; link.symlink_to(target, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, 'symlink'):
                IME.private_path(link / 'new')

    def test_private_config_maps_current_sdk_extraction_not_fixed_old_hash(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            sdk = base / 'sdk'; extraction = sdk / 'root/x11-ime-new-package-set'; extraction.mkdir(parents=True)
            platform = base / 'platform'; (platform / 'root').mkdir(parents=True)
            app, host, helper = (base / name for name in ('app', 'host', 'helper'))
            app.write_bytes(b'app'); host.write_bytes(b'host'); helper.write_bytes(b'helper')
            args = SimpleNamespace(app=app, host=host, x11_sdk=sdk, platform_sdk=platform)
            config = IME.inside_config(args, base, helper,
                                       {'extraction_root': str(extraction), 'manifest_sha256': 'x'},
                                       {'manifest_sha256': 'y'})
            self.assertEqual(config['x11_root'], '/pigade/x11-sdk')
            self.assertEqual(config['x11_extraction_root'], str(extraction))
            self.assertEqual(config['platform_root'], str(platform / 'root'))

    def test_bwrap_builder_clears_host_display_and_mounts_only_extracted_payloads(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            sdk = base / 'sdk'; extracted = sdk / 'root/x11-ime-set'; extracted.mkdir(parents=True)
            platform = base / 'platform'; (platform / 'root').mkdir(parents=True)
            output = base / 'output'; output.mkdir()
            app, host, helper = (base / name for name in ('app', 'host', 'helper'))
            app.write_bytes(b'app'); host.write_bytes(b'host'); helper.write_bytes(b'helper')
            config_path = base / 'config.json'
            config_path.write_text(json.dumps({
                'x11_root': '/pigade/x11-sdk', 'x11_extraction_root': str(extracted),
                'platform_root': str(platform / 'root'),
                'private_icd': '/pigade/platform-sdk/lavapipe-icd.json',
                'private_icd_host': str(output / 'lavapipe-icd.json'),
            }))
            args = SimpleNamespace(x11_sdk=sdk, platform_sdk=platform, output=output,
                                   app=app, host=host)
            argv = IME.bwrap_command(args, config_path, helper)
            self.assertIn('--unshare-all', argv); self.assertIn('--clearenv', argv)
            self.assertIn(str(extracted), argv)
            self.assertNotIn(str(sdk / 'downloads'), argv)
            self.assertNotIn(str(platform / 'downloads'), argv)
            self.assertNotIn('/run/user', argv)
            self.assertNotIn('/dev/input', argv)
            self.assertNotIn('/dev/dri', argv)
            environment = {argv[index + 1] for index, value in enumerate(argv[:-1])
                           if value == '--setenv'}
            self.assertTrue({'HOME', 'XDG_CONFIG_HOME', 'XDG_RUNTIME_DIR',
                             'LD_LIBRARY_PATH', 'LIBIME_MODEL_DIRS'} <= environment)
            self.assertFalse({'DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS',
                              'XAUTHORITY', 'HYPRLAND_INSTANCE_SIGNATURE'} & environment)

    def test_outer_wrapper_retains_bounded_stderr_on_startup_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            exit_status = IME.run_bwrap(['/usr/bin/python3', '-I', '-c',
                                        'import os;os.write(2,b"x"*100000);raise SystemExit(3)'], output)
            self.assertEqual(exit_status, 3)
            retained = (output / 'bwrap.stderr.log').read_bytes()
            self.assertEqual(len(retained), IME.LOG_LIMIT)
            self.assertEqual(retained, b'x' * IME.LOG_LIMIT)

    def test_process_identity_reads_real_proc_start_time(self):
        boot, start = IME.process_identity(os.getpid())
        self.assertEqual(boot, Path('/proc/sys/kernel/random/boot_id').read_text().strip())
        self.assertGreater(start, 0)

    def test_window_probe_requires_positive_verified_result(self):
        self.assertTrue(IME.window_verified({'window_verified': True}))
        self.assertFalse(IME.window_verified({'window_verified': False}))
        self.assertFalse(IME.window_verified({}))

    def test_private_identity_files_include_actual_host_uid_and_gid(self):
        passwd, group = IME.private_identity_files(1234, 2345)
        self.assertIn('pigade:x:1234:2345:', passwd)
        self.assertIn('pigade:x:2345:', group)

    def test_pinyin_selection_is_private_and_confirmed_without_key_events(self):
        config = {'display': ':3', 'x11_root': '/pigade/x11-sdk'}
        with mock.patch.object(IME.subprocess, 'run', side_effect=[
                SimpleNamespace(returncode=0, stdout='', stderr=''),
                SimpleNamespace(returncode=0, stdout='', stderr=''),
                SimpleNamespace(returncode=0, stdout='pinyin\n', stderr=''),
        ]) as run:
            self.assertEqual(IME.select_private_pinyin(
                config, Path('/home/pigade'), Path('/run/pigade-test'),
                Path('/run/pigade-test/Xauthority')), 'pinyin')
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ['/pigade/x11-sdk/usr/bin/fcitx5-remote', '-s', 'pinyin'],
            ['/pigade/x11-sdk/usr/bin/fcitx5-remote', '-o'],
            ['/pigade/x11-sdk/usr/bin/fcitx5-remote', '-n'],
        ])
        for call in run.call_args_list:
            env = call.kwargs['env']
            self.assertEqual(env['DISPLAY'], ':3')
            self.assertEqual(env['XAUTHORITY'], '/run/pigade-test/Xauthority')
            self.assertEqual(env['DBUS_SESSION_BUS_ADDRESS'], 'unix:path=/run/pigade-test/bus')

    def test_keyboard_mode_is_fixed_plan_only_and_loads_only_private_map(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'keyboard']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'keyboard')
        self.assertFalse(plan['input']['arbitrary_text_argument'])
        self.assertEqual(IME.KEYBOARD_EXPECTED,
                         b'ab1Z\x03\x04\x1a\x12\x0c\x01\x1bx\x1b[D\x1b[H\r' + 'привет'.encode())
        env = {'DISPLAY': ':3', 'XAUTHORITY': '/run/pigade-test/Xauthority'}
        with mock.patch.object(IME.subprocess, 'run', return_value=SimpleNamespace(
                returncode=0, stdout='', stderr='')) as run:
            IME.prepare_private_keyboard({'x11_root': '/pigade/x11-sdk', 'display': ':3'}, env)
        self.assertEqual(run.call_args.args[0], ['/pigade/x11-sdk/usr/bin/setxkbmap',
                         '-display', ':3', '-layout', 'us,ru', '-option', ''])
        self.assertEqual(run.call_args.kwargs['env'], env)

    def run_raw_fixture(self, expected, delivered):
        with tempfile.TemporaryDirectory() as temporary:
            path = str(Path(temporary) / 'control.sock')
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            master, slave = pty.openpty()
            process = None
            control = None
            try:
                listener.bind(path); listener.listen(1); listener.settimeout(3)
                process = subprocess.Popen(['/usr/bin/python3', '-I', '-c', IME.FIXTURE,
                                            path, expected.hex()], stdin=slave, stdout=slave,
                                           stderr=subprocess.DEVNULL, close_fds=True)
                os.close(slave); slave = -1
                control, _ = listener.accept()
                self.assertEqual(IME.json_line(control, timeout=2), 'READY')
                control.sendall(b'CHECK\n')
                self.assertEqual(IME.json_line(control, timeout=2), 'NO_PRECOMMIT_INPUT')
                os.write(master, delivered)
                control.sendall(b'COLLECT\n')
                line = IME.json_line(control, timeout=5)
                self.assertTrue(line.startswith('RESULT:'))
                result = json.loads(line[len('RESULT:'):])
                self.assertEqual(process.wait(timeout=3), 0)
                return result
            finally:
                if control is not None:
                    control.close()
                listener.close()
                os.close(master)
                if slave >= 0:
                    os.close(slave)
                if process is not None and process.poll() is None:
                    # Still-owned direct child; no PID discovery or desktop access.
                    process.kill(); process.wait(timeout=3)

    def test_raw_pty_oracle_checks_ime_and_keyboard_bytes_without_echo_or_duplicates(self):
        for expected in ('你好'.encode(), IME.KEYBOARD_EXPECTED):
            with self.subTest(expected_bytes=len(expected)):
                result = self.run_raw_fixture(expected, expected)
                self.assertEqual(result['received_bytes'], len(expected))
                self.assertTrue(result['exact_once'])
                self.assertEqual(result['extra_bytes'], 0)

    def test_raw_pty_oracle_refuses_duplicate_trailing_bytes(self):
        expected = '你好'.encode()
        result = self.run_raw_fixture(expected, expected + expected)
        self.assertFalse(result['exact_once'])
        self.assertEqual(result['extra_bytes'], len(expected))

    def test_latency_child_checks_ordered_input_and_rejects_trailing_duplicate(self):
        for duplicate in (False, True):
            with self.subTest(duplicate=duplicate), tempfile.TemporaryDirectory() as temporary:
                path = str(Path(temporary) / 'control.sock')
                listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                master, slave = pty.openpty()
                process = None
                control = None
                try:
                    listener.bind(path); listener.listen(1); listener.settimeout(3)
                    process = subprocess.Popen(['/usr/bin/python3', '-I', '-c', IME.LATENCY_FIXTURE,
                                                path, IME.LATENCY_KEYS.hex()], stdin=slave,
                                               stdout=slave, stderr=subprocess.DEVNULL, close_fds=True)
                    os.close(slave); slave = -1
                    control, _ = listener.accept()
                    self.assertEqual(IME.json_line(control, timeout=2), 'READY')
                    control.sendall(b'START\n')
                    os.write(master, IME.LATENCY_KEYS + (IME.LATENCY_KEYS[-1:] if duplicate else b''))
                    if duplicate:
                        with self.assertRaises(RuntimeError):
                            IME.json_line(control, timeout=3)
                        self.assertNotEqual(process.wait(timeout=3), 0)
                    else:
                        self.assertEqual(IME.json_line(control, timeout=3), 'FIFTY_EXACT_KEYS')
                        self.assertEqual(process.wait(timeout=3), 0)
                finally:
                    if control is not None:
                        control.close()
                    listener.close(); os.close(master)
                    if slave >= 0:
                        os.close(slave)
                    if process is not None and process.poll() is None:
                        process.kill(); process.wait(timeout=3)

    def test_helper_contains_only_fixed_input_stages_and_server_validation(self):
        source = Path(__file__).with_name('x11-ime-input.c').read_text()
        self.assertIn('const KeySym sequence[] = { XK_n, XK_i, XK_h, XK_a, XK_o }', source)
        self.assertIn('key(dpy, XK_space)', source)
        self.assertIn('"@server=fcitx"', source)
        self.assertIn('xcb_res_client_id_value_value_length(it.data)', source)
        self.assertIn('SO_PEERCRED', source)
        self.assertIn('XTestQueryExtension', source)
        focus_stage = source.split('else if (!strcmp(a.stage, "focus")) {', 1)[1].split('} else {', 1)[0]
        self.assertIn('verify_app_focus_target(&a, dpy, c, 1)', focus_stage)
        self.assertNotIn('key(dpy,', focus_stage)
        self.assertNotIn('--text', source)
        self.assertNotIn('XStringToKeysym(argv', source)

    def test_helper_build_and_identity_selftests_never_open_x(self):
        with tempfile.TemporaryDirectory() as temporary:
            helper = Path(temporary) / 'x11-ime-input'
            evidence = IME.compile_helper(Path(__file__).with_name('x11-ime-input.c'), helper)
            self.assertEqual(evidence['libraries'], ['X11', 'Xtst', 'xcb', 'xcb-res'])
            result = subprocess.run([str(helper), '--self-test-proc'], stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                    timeout=2, check=True, env={'PATH': '/usr/bin', 'DISPLAY': ':99999'})
            self.assertGreater(int(result.stdout.strip()), 0)
            self.assertEqual(result.stderr, '')
            length = subprocess.run([str(helper), '--self-test-xres-length'],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, text=True, timeout=2, check=True,
                                    env={'PATH': '/usr/bin', 'DISPLAY': ':99999'})
            self.assertEqual(length.stdout.strip(), 'wire_bytes=4 decoded_values=1')
            missing = subprocess.run([str(helper), '--self-test-window-missing'],
                                     stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True, timeout=2, check=False,
                                     env={'PATH': '/usr/bin', 'DISPLAY': ':99999'})
            self.assertEqual(missing.returncode, 2)
            self.assertIn('verified app window is absent', missing.stderr)
            diagnostic = subprocess.run([str(helper), '--self-test-latency-diagnostic'],
                                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, timeout=2, check=True,
                                        env={'PATH': '/usr/bin', 'DISPLAY': ':99999'})
            parsed = IME.parse_latency_helper_diagnostic(diagnostic.stdout)
            self.assertEqual(parsed['sample_count'], 7)
            self.assertEqual(parsed['failed_index'], 7)
            self.assertEqual(parsed['samples_ns'], [100, 110, 120, 130, 140, 150, 160])
            self.assertEqual(parsed['expected_rgb'], [45, 239, 27])
            self.assertEqual(parsed['observed_rgb'], [41, 239, 27])
            self.assertEqual(parsed['reason'], 'color_deadline')

    def latency_diagnostic_payload(self):
        return {
            'version': 1, 'stage': 'latency',
            'scope': 'XTEST-submit to private root pixel readback', 'pixel': [20, 120],
            'sample_count': 7, 'samples_ns': list(range(100, 107)), 'failed_index': 7,
            'expected_rgb': [45, 239, 27], 'observed_rgb': [41, 239, 27],
            'elapsed_ns': 1_000_100_000, 'reason': 'color_deadline',
        }

    def test_latency_diagnostic_rejects_oversize_malformed_and_nonfixture_values(self):
        valid = self.latency_diagnostic_payload()
        self.assertEqual(IME.parse_latency_helper_diagnostic(json.dumps(valid)),
                         {**valid, 'samples_ns': list(range(100, 107))})
        cases = []
        cases.append(b'x' * (IME.MAX_LATENCY_DIAGNOSTIC_BYTES + 1))
        cases.append(b'{not json')
        for change in (
                {'unexpected_raw_text': 'do not retain'},
                {'version': True},
                {'failed_index': 8},
                {'observed_rgb': [256, 0, 0]},
                {'reason': 'arbitrary-provider-output'},
                {'samples_ns': list(range(100, 106))},
                {'expected_rgb': [46, 239, 27]}):
            altered = dict(valid); altered.update(change)
            cases.append(json.dumps(altered))
        for candidate in cases:
            with self.subTest(candidate_type=type(candidate).__name__):
                with self.assertRaises((ValueError, TypeError, json.JSONDecodeError)):
                    IME.parse_latency_helper_diagnostic(candidate)

    def test_latency_failure_helper_preserves_only_validated_fixed_diagnostic(self):
        with mock.patch.dict(IME.os.environ, {'LD_LIBRARY_PATH': ''}), \
                mock.patch.object(IME.subprocess, 'run', return_value=SimpleNamespace(
                returncode=2, stdout=json.dumps(self.latency_diagnostic_payload()),
                stderr='arbitrary helper stderr must not be retained')):
            with self.assertRaisesRegex(RuntimeError, 'no key retry') as caught:
                IME.helper_call({'display': ':3', 'boot_id': 'boot', 'xvfb_pid': 1,
                                 'xvfb_start': 2, 'xvfb_socket_inode': 3,
                                 'fcitx_pid': 4, 'fcitx_start': 5,
                                 'app_pid': 6, 'app_start': 7}, 'latency')
        self.assertEqual(caught.exception.latency_diagnostic_status, 'validated')
        self.assertEqual(caught.exception.latency_diagnostic['failed_index'], 7)
        self.assertNotIn('arbitrary helper stderr', str(caught.exception))

    def test_latency_failure_diagnostic_collection_cannot_mask_original_exception(self):
        original = RuntimeError('fixed latency color deadline; no key retry')
        with mock.patch.object(IME, 'collect_latency_failure_diagnostics',
                               side_effect=OSError('private diagnostic failure')):
            snapshot = IME.capture_latency_failure_diagnostics({}, {}, Path('/nonexistent'), original)
        self.assertEqual(snapshot, {'status': 'unavailable', 'error_type': 'OSError'})
        self.assertEqual(str(original), 'fixed latency color deadline; no key retry')

    def test_latency_host_snapshot_redacts_content_and_bounds_palette_and_cells(self):
        sentinel = 'PRIVATE_SENTINEL_MUST_NOT_ESCAPE'
        cells = [{'text': chr(97 + index % 26), 'bg': 'Spec(Rgb { r: 45, g: 239, b: 27 })',
                  'fg': sentinel, 'hyperlink': 1} for index in range(8)]
        cells[7]['text'] = sentinel
        reply = {'views': [{
            'session': 'fixture-session', 'generation': 'private-generation', 'sequence': 10,
            'state': 'running', 'pid': 999, 'storage_error': None,
            'frame': {'geometry': {'rows': 2, 'cols': 4}, 'cells': cells,
                      'cursor': [1, 2], 'modes': 7, 'history_lines': 0,
                      'sync_pending_bytes': 0, 'title': sentinel, 'colors': sentinel,
                      'palette_overrides': [[7, [45, 239, 27]]]},
            'actor_diagnostic': {'version': 1, 'status': 'available',
                                 'stage': 'model_apply', 'candidate_sequence': 11,
                                 'entered_monotonic_ns': 123,
                                 'reader': {'read_calls': 3, 'bytes_read': 8,
                                            'last_read_bytes': 4, 'enqueued_calls': 2,
                                            'bytes_enqueued': 8,
                                            'last_read_monotonic_ns': 100,
                                            'last_enqueued_monotonic_ns': 101}},
        }]}
        summary = IME.sanitize_latency_host_snapshot(reply, 'fixture-session', [45, 239, 27])
        encoded = json.dumps(summary)
        self.assertNotIn(sentinel, encoded)
        self.assertNotIn('private-generation', encoded)
        self.assertNotIn('fixture-session', encoded)
        self.assertEqual(summary['sequence'], 10)
        self.assertEqual(summary['cell_samples'][7]['text_class'], 'redacted')
        self.assertTrue(summary['cell_samples'][7]['matches_failed_expected_rgb'])
        self.assertEqual(summary['palette_overrides'], [[7, [45, 239, 27]]])
        self.assertEqual(summary['actor_diagnostic']['stage'], 'model_apply')
        reply['views'][0]['frame']['cells'] = cells * 9000
        with self.assertRaisesRegex(ValueError, 'bounds'):
            IME.sanitize_latency_host_snapshot(reply, 'fixture-session', [45, 239, 27])
        reply['views'][0]['frame']['cells'] = cells
        reply['views'][0]['frame']['palette_overrides'] = [[999, [0, 0, 0]]]
        with self.assertRaisesRegex(ValueError, 'palette'):
            IME.sanitize_latency_host_snapshot(reply, 'fixture-session', [45, 239, 27])

    def test_xim_selection_name_matches_pinned_upstream_registration_form(self):
        # xcb-imdkit constructs its registered atom as "@server=<serverName>".
        # The harness only accepts the serverName chosen by XMODIFIERS=@im=fcitx.
        self.assertIn("XMODIFIERS': '@im=fcitx'", SCRIPT.read_text())
        self.assertIn('"@server=fcitx"', Path(__file__).with_name('x11-ime-input.c').read_text())


if __name__ == '__main__':
    unittest.main()
