#!/usr/bin/env python3
"""Headless contract tests for the private Wayland IME harness (no compositor)."""
from __future__ import annotations

import importlib.util
import json
import errno
import hashlib
import os
from pathlib import Path
import pty
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest


SCRIPT = Path(__file__).with_name('isolated-ime-wayland.py')
SPEC = importlib.util.spec_from_file_location('isolated_ime_wayland', SCRIPT)
assert SPEC is not None and SPEC.loader is not None
harness = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(harness)


class IsolatedImeWaylandTests(unittest.TestCase):
    def test_default_plan_is_nonmutating_and_marks_fixed_input(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'never-created'
            args = harness.parse_args(['--output', str(output)])
            plan = harness.plan(args)
            self.assertEqual(plan['status'], 'plan-only-no-process-no-write')
            self.assertFalse(output.exists())
            self.assertEqual(plan['fixed_input'], ['n', 'i', 'h', 'a', 'o', 'Space'])
            self.assertFalse(plan['arbitrary_text_input_option'])
            self.assertIn('zwp_virtual_keyboard_manager_v1', plan['protocol_globals'])

    def test_default_cli_invocation_only_prints_plan(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'must-not-exist'
            result = subprocess.run([sys.executable, str(SCRIPT), '--output', str(output)],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, timeout=4, check=False)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertIn(b'plan-only-no-process-no-write', result.stdout)
            self.assertFalse(output.exists())

    def test_persistent_virtual_keyboard_helper_compiles_and_selftests_without_display(self):
        with tempfile.TemporaryDirectory() as temporary:
            helper = Path(temporary) / 'ime-keyboard-helper'
            info = harness.build_virtual_keyboard_helper(Path(temporary))
            self.assertEqual(info['path'], str(helper))
            self.assertEqual(info['protocol_xml_sha256'], harness.INPUT_PROTOCOL_SHA256)
            self.assertTrue(info['ldd_libraries'])
            self.assertGreater(info['ldd_bytes'], 0)
            self.assertTrue(all(Path(row['path']).is_file() and
                                row['path'].startswith('/usr/') and row['sha256']
                                for row in info['ldd_libraries']))
            self.assertIn('signed X11/Fcitx SDK', info['xkb_config_source'])
            result = subprocess.run([str(helper), '--selftest'], stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    timeout=3, check=False)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(result.stdout,
                             b'fixed-sequence-selftest: monotonic deadline; COMPOSE once, then COMMIT once; QUIT anytime\n')
            self.assertFalse(list(Path(temporary).glob('*.sock')))

    def test_xkb_root_requires_exact_verified_signed_payload_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            inventory = {}
            for relative in harness.XKB_REQUIRED_PAYLOADS:
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('fixed-signed-xkb-fixture')
                inventory[relative] = {'type': 'file', 'sha256': harness.sha256(path)}
            self.assertEqual(harness.verified_xkb_config_root(root, inventory),
                             '/pigade/fcitx-core/usr/share/xkeyboard-config-2')
            inventory[harness.XKB_REQUIRED_PAYLOADS[0]]['sha256'] = '0' * 64
            with self.assertRaisesRegex(RuntimeError, 'signed XKB keymap payload'):
                harness.verified_xkb_config_root(root, inventory)

    def test_virtual_keyboard_protocol_is_pinned_and_embeds_its_license(self):
        xml = harness.INPUT_PROTOCOL_XML.read_text()
        self.assertEqual(harness.sha256(harness.INPUT_PROTOCOL_XML), harness.INPUT_PROTOCOL_SHA256)
        self.assertEqual(harness.sha256(harness.WTYPE_LICENSE), harness.WTYPE_LICENSE_SHA256)
        self.assertIn('virtual_keyboard_unstable_v1', xml)
        self.assertIn('Permission is hereby granted', xml)
        self.assertIn('zwp_virtual_keyboard_manager_v1', xml)
        helper = harness.INPUT_HELPER_SOURCE.read_text()
        for command in ('COMPOSE', 'COMMIT', 'QUIT'):
            self.assertIn(f'"{command}"', helper)
        self.assertNotIn('getenv("DISPLAY")', helper)

    def test_sway_window_match_requires_one_exact_visible_pid(self):
        root = {'nodes': [{'id': 1, 'type': 'con', 'pid': 123, 'visible': True,
                           'nodes': [{'id': 2, 'type': 'con', 'pid': 456, 'visible': True}]}]}
        self.assertEqual(harness.app_window(root, 123)['id'], 1)
        with self.assertRaisesRegex(RuntimeError, 'found 0'):
            harness.app_window(root, 789)
        root['floating_nodes'] = [{'id': 3, 'type': 'floating_con', 'pid': 123, 'visible': True}]
        with self.assertRaisesRegex(RuntimeError, 'found 2'):
            harness.app_window(root, 123)

    def test_socket_peercred_and_process_identity_are_both_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'private.sock'
            code = ('import socket,sys,time; s=socket.socket(socket.AF_UNIX); '
                    's.bind(sys.argv[1]); s.listen(1); c,_=s.accept(); c.close(); s.close()')
            process = subprocess.Popen([sys.executable, '-c', code, str(path)],
                                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL, close_fds=True)
            try:
                deadline = time.monotonic() + 3
                while not path.exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                self.assertTrue(path.is_socket())
                boot, start = harness.process_identity(process.pid)
                identity = {'socket_inode': path.lstat().st_ino, 'boot_id': boot,
                            'start_ticks': start}
                peer = harness.verify_private_peer(path, identity, process)
                self.assertEqual(peer['peer_pid'], process.pid)
                identity['socket_inode'] += 1
                with self.assertRaisesRegex(RuntimeError, 'inode changed'):
                    harness.verify_private_peer(path, identity, process)
            finally:
                if process.poll() is None:
                    process.terminate()
                process.wait(timeout=2)

    def test_sway_ipc_framing_reads_exact_bounded_reply(self):
        server, client = socket.socketpair()
        def respond():
            request_header = server.recv(14)
            self.assertEqual(request_header[:6], b'i3-ipc')
            size, kind = struct.unpack('<II', request_header[6:14])
            self.assertEqual((size, kind), (0, 4))
            payload = json.dumps({'nodes': []}).encode()
            server.sendall(b'i3-ipc' + struct.pack('<II', len(payload), kind) + payload)
            server.close()
        thread = threading.Thread(target=respond)
        thread.start()
        try:
            self.assertEqual(harness.sway_ipc(client, 4), {'nodes': []})
        finally:
            client.close(); thread.join(timeout=2)

    def test_run_bounded_drains_final_chunks_after_fast_child_exit(self):
        child = ('import os; os.write(1,b"one"); os.write(2,b"two"); '
                 'os.write(1,b"three"); os._exit(0)')
        status, stdout, stderr = harness.run_bounded([sys.executable, '-c', child],
                                                    {'PATH': '/usr/bin'}, timeout=3, cap=100)
        self.assertEqual(status, 0)
        self.assertEqual(stdout, b'onethree')
        self.assertEqual(stderr, b'two')

    def test_png_header_dimensions_are_checked_without_image_decoder(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'shot.png'
            header = (b'\x89PNG\r\n\x1a\n' + struct.pack('>I', 13) + b'IHDR' +
                      struct.pack('>II', 1280, 800) + b'\x08\x02\x00\x00\x00' + b'0000')
            path.write_bytes(header)
            self.assertEqual(harness.png_dimensions(path), (1280, 800))
            path.write_bytes(b'not a png' + bytes(30))
            with self.assertRaisesRegex(RuntimeError, 'not a PNG'):
                harness.png_dimensions(path)

    def test_raw_pty_fixture_checks_noearlybytes_exact_commit_and_child_ack(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'fixture.sock'
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(str(path)); listener.listen(1); listener.settimeout(3)
            master, slave = pty.openpty()
            child = subprocess.Popen([sys.executable, '-c', harness.FIXTURE, str(path),
                                      harness.EXPECTED_TEXT.hex()],
                                     stdin=slave, stdout=slave, stderr=slave,
                                     close_fds=True)
            os.close(slave)
            control, _ = listener.accept(); listener.close(); control.settimeout(2)
            try:
                self.assertEqual(harness.read_line(control), 'READY')
                control.sendall(b'CHECK\n')
                self.assertEqual(harness.read_line(control), 'NO_PRECOMMIT_INPUT')
                os.write(master, harness.EXPECTED_TEXT)
                control.sendall(b'COLLECT\n')
                line = harness.read_line(control, timeout=3)
                self.assertTrue(line.startswith('RESULT:'))
                result = json.loads(line[len('RESULT:'):])
                self.assertEqual(result['received_bytes'], 6)
                self.assertTrue(result['exact_once'])
                self.assertEqual(result['extra_bytes'], 0)
                deadline = time.monotonic() + 2
                terminal = bytearray()
                while time.monotonic() < deadline and harness.ACK.encode() not in terminal:
                    ready, _, _ = select_select([master], .05)
                    if ready:
                        try:
                            terminal.extend(os.read(master, 4096))
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            break
                self.assertIn(harness.ACK.encode(), terminal)
                self.assertEqual(child.wait(timeout=2), 0)
            finally:
                control.close(); os.close(master)
                if child.poll() is None:
                    child.terminate(); child.wait(timeout=2)

    def test_bwrap_plan_has_private_device_tree_and_no_host_endpoint_binds(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            config = base / 'config.json'
            config.write_text(json.dumps({'ld_library_path': '/pigade/test/usr/lib'}))
            args = type('Args', (), {'output': base, 'app': base / 'app', 'host': base / 'host'})()
            command = harness.bwrap_command(config, args, base, base, base, base / 'icd', 91)
            self.assertIn('--unshare-all', command)
            self.assertIn('--clearenv', command)
            self.assertIn('--dev', command)
            self.assertNotIn('/run/user', command)
            self.assertNotIn('/dev/input', command)
            self.assertNotIn('/dev/dri', command)
            self.assertNotIn('/run/dbus', command)
            self.assertIn('--info-fd', command)
            self.assertEqual(command[command.index('--info-fd') + 1], '91')
            self.assertEqual(command[-1], '--inside')

    def test_bwrap_binds_only_the_prebuilt_input_helper_readonly(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            config = base / 'config.json'
            config.write_text(json.dumps({'ld_library_path': '/pigade/test/usr/lib'}))
            helper = base / 'ime-keyboard-helper'
            helper.write_bytes(b'headless-test-binary')
            args = type('Args', (), {'output': base, 'app': base / 'app', 'host': base / 'host'})()
            command = harness.bwrap_command(config, args, base, base, base, base / 'icd', 91,
                                            helper)
            index = command.index(str(helper))
            self.assertEqual(command[index - 1], '--ro-bind')
            self.assertEqual(command[index + 1], '/pigade/ime-keyboard')

    def test_direct_inside_requires_parent_gate_before_creating_runtime_or_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = subprocess.run([sys.executable, str(SCRIPT), '--inside'],
                                    cwd=temporary, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    timeout=4, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'missing parent namespace attestation pipe', result.stderr)
            self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_namespace_fd_checks_reject_same_namespace_closed_and_wrong_handles(self):
        handles = []
        try:
            outer = {}
            for name in harness.NAMESPACES:
                fd = os.open(f'/proc/self/ns/{name}', os.O_RDONLY | os.O_CLOEXEC)
                handles.append(fd)
                outer[name] = harness.namespace_identity_from_fd(fd, name)
            with self.assertRaisesRegex(RuntimeError, 'distinct mnt namespace'):
                harness.verify_child_namespace_handles(os.getpid(),
                                                       harness.process_identity(os.getpid()), outer)
            wrong = os.open('/dev/null', os.O_RDONLY | os.O_CLOEXEC)
            try:
                with self.assertRaisesRegex(RuntimeError, 'not an nsfs file'):
                    harness.namespace_identity_from_fd(wrong, 'mnt')
            finally:
                os.close(wrong)
            closed = os.open('/proc/self/ns/mnt', os.O_RDONLY | os.O_CLOEXEC)
            os.close(closed)
            with self.assertRaises(OSError):
                harness.namespace_identity_from_fd(closed, 'mnt')
        finally:
            for fd in handles:
                os.close(fd)

    def test_namespace_gate_binds_nonce_and_parent_kernel_attestation(self):
        with tempfile.TemporaryDirectory() as temporary:
            config_path = Path(temporary) / 'config.json'
            nonce = 'a' * 64
            attestation = {'schema': 1, 'outer': {'mnt': 'mnt:[10]'},
                           'child_pid': 22, 'child_identity': {'pid': 22},
                           'child_namespaces': {'mnt': {'identity': 'mnt:[20]'}}}
            digest = harness.namespace_attestation_digest(attestation)
            config_path.write_text(json.dumps({
                'namespace_gate_sha256': hashlib.sha256(nonce.encode()).hexdigest(),
                'namespace_attestation': attestation,
            }))
            read_fd, write_fd = os.pipe()
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            attestation_path = Path(temporary) / 'attest.sock'
            server.bind(str(attestation_path)); server.listen(1)
            received = []
            def accept_hello():
                connection, _ = server.accept()
                with connection:
                    received.append(connection.recv(len(harness.NAMESPACE_HELLO)))
            thread = threading.Thread(target=accept_hello)
            thread.start()
            saved_stdin = harness.sys.stdin
            try:
                os.write(write_fd, f'{nonce}:{digest}\n'.encode())
                os.close(write_fd); write_fd = -1
                harness.sys.stdin = os.fdopen(read_fd, 'rb', closefd=False)
                checked = harness.await_namespace_gate(config_path, timeout=0.1,
                                                       attestation_path=attestation_path)
                self.assertEqual(checked['namespace_attestation'], attestation)
            finally:
                harness.sys.stdin = saved_stdin
                os.close(read_fd)
                if write_fd >= 0:
                    os.close(write_fd)
                thread.join(timeout=2); server.close()
            self.assertEqual(received, [harness.NAMESPACE_HELLO])

    def test_namespace_gate_partial_token_with_open_writer_is_deadline_bounded(self):
        read_fd, write_fd = os.pipe()
        with tempfile.TemporaryDirectory() as temporary:
            attestation_path = Path(temporary) / 'attest.sock'
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            server.bind(str(attestation_path)); server.listen(1)
            received = []
            def accept_hello():
                connection, _ = server.accept()
                with connection:
                    received.append(connection.recv(len(harness.NAMESPACE_HELLO)))
            thread = threading.Thread(target=accept_hello)
            thread.start()
            saved_stdin = harness.sys.stdin
            try:
                os.write(write_fd, b'partial')
                harness.sys.stdin = os.fdopen(read_fd, 'rb', closefd=False)
                started = time.monotonic()
                with self.assertRaisesRegex(RuntimeError, 'read deadline'):
                    harness.await_namespace_gate(timeout=0.05, attestation_path=attestation_path)
                self.assertLess(time.monotonic() - started, 0.5)
            finally:
                harness.sys.stdin = saved_stdin
                os.close(read_fd); os.close(write_fd)
                thread.join(timeout=2); server.close()
            self.assertEqual(received, [harness.NAMESPACE_HELLO])

    def test_bwrap_info_allows_user_setup_namespace_but_checks_other_kernel_ids(self):
        child = {name: {'inode': index + 100} for index, name in enumerate(harness.NAMESPACES)}
        info = {'child-pid': 42, 'mnt-namespace': 100, 'pid-namespace': 101,
                'user-namespace': 999, 'ipc-namespace': 103, 'net-namespace': 104}
        evidence = harness.verify_bwrap_namespace_info(info, child)
        self.assertFalse(evidence['bwrap_info_matches_kernel']['user'])
        self.assertEqual(evidence['kernel_child_namespace_ids']['user'], 102)
        del info['user-namespace']
        omitted = harness.verify_bwrap_namespace_info(info, child)
        self.assertEqual(omitted['user_namespace_info_status'], 'omitted')
        self.assertIsNone(omitted['bwrap_info_namespace_ids']['user'])
        info['user-namespace'] = 0
        zero = harness.verify_bwrap_namespace_info(info, child)
        self.assertEqual(zero['user_namespace_info_status'], 'omitted-zero')
        self.assertEqual(zero['bwrap_info_namespace_ids']['user'], 0)
        info['user-namespace'] = 999
        info['ipc-namespace'] += 1
        with self.assertRaisesRegex(RuntimeError, 'ipc namespace differs'):
            harness.verify_bwrap_namespace_info(info, child)

    def test_close_owned_fd_does_not_close_a_reused_descriptor(self):
        read_fd, write_fd = os.pipe()
        reused = read_fd
        read_fd = harness.close_owned_fd(read_fd)
        self.assertEqual(read_fd, -1)
        replacement = os.open('/dev/null', os.O_RDONLY | os.O_CLOEXEC)
        try:
            self.assertEqual(replacement, reused)
            read_fd = harness.close_owned_fd(read_fd)
            self.assertEqual(read_fd, -1)
            self.assertTrue(os.fstat(replacement))
        finally:
            os.close(replacement); os.close(write_fd)

    def test_failed_run_report_transitions_from_running_to_failed_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'run.json'
            diagnostic = {'raw': '{"child-pid":42}', 'child': {'mnt': {'inode': 123}}}
            path.write_text(json.dumps({'schema': 1, 'status': 'running', 'stage': 'launch',
                                        'namespace_diagnostic': diagnostic}))
            harness.mark_failed_run(path, 'namespace-attestation', RuntimeError('proof mismatch'))
            report = json.loads(path.read_text())
            self.assertEqual(report['status'], 'failed-closed')
            self.assertEqual(report['failure_stage'], 'namespace-attestation')
            self.assertIn('proof mismatch', report['failure_reason'])
            self.assertEqual(report['namespace_diagnostic'], diagnostic)


def select_select(readers, timeout):
    import select
    return select.select(readers, [], [], timeout)


if __name__ == '__main__':
    unittest.main()
