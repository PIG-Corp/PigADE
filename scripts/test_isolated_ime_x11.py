"""Tests for the opt-in-only, fixed-sequence private X11/XIM harness."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
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

    def test_xim_selection_name_matches_pinned_upstream_registration_form(self):
        # xcb-imdkit constructs its registered atom as "@server=<serverName>".
        # The harness only accepts the serverName chosen by XMODIFIERS=@im=fcitx.
        self.assertIn("XMODIFIERS': '@im=fcitx'", SCRIPT.read_text())
        self.assertIn('"@server=fcitx"', Path(__file__).with_name('x11-ime-input.c').read_text())


if __name__ == '__main__':
    unittest.main()
