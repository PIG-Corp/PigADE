"""VM-free validation tests for the synthetic and corpus reboot plans."""
from __future__ import annotations

import hashlib
import importlib.util
import ast
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
import uuid
from unittest import mock


SCRIPT = Path(__file__).with_name('vm-reboot.py')
SPEC = importlib.util.spec_from_file_location('vm_reboot', SCRIPT)
assert SPEC and SPEC.loader
vm = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(vm)


class CorpusFixture:
    def __init__(self, root: Path):
        self.run = root / 'long-history-run-test'
        self.run.mkdir(mode=0o700)
        self.corpus = self.run / 'corpus'
        self.corpus.mkdir(mode=0o700)
        self.reference_path = root / 'recover-result.json'
        self.binary = root / 'pigade-session-host'
        self.binary.write_bytes(b'test frozen host binary')
        self.binary.chmod(0o700)
        self.profile = vm.current_core_profile()
        self.rows = []
        reference_rows = {}
        output_bytes = 1024**2
        resize_events = 100
        for index in range(4):
            session = str(uuid.uuid4())
            generation = str(uuid.uuid4())
            journal = f'{generation}.journal'
            timeline = f'{generation}.timeline.jsonl'
            history = f'{generation}.history'
            (self.corpus / journal).write_bytes(f'journal-{index}'.encode())
            (self.corpus / timeline).write_text('timeline\n')
            (self.corpus / f'{generation}.00000000000000000042.checkpoint').write_bytes(b'checkpoint')
            history_path = self.corpus / history
            history_path.mkdir()
            (history_path / 'rows.index').write_bytes(b'index')
            (history_path / '00000000000000000000.archive').write_bytes(b'archive')
            self.rows.append({
                'session_id':session, 'generation':generation, 'journal':journal,
                'logical_timeline':timeline, 'requested_output_bytes':output_bytes,
                'resize_events':resize_events, 'cold_history_records':7,
                'final_sequence':42, 'journal_bytes':len(f'journal-{index}'),
                'verification':{key:True for key in vm.CORPUS_REQUIRED_PROOFS},
            })
            reference_rows[generation] = {
                'session':session, 'sequence':42,
                'frame_sha256':hashlib.sha256(f'frame-{index}'.encode()).hexdigest(),
                'history':{'records':7, 'bytes':11}, 'journal_bytes_read':1,
                'canonical_tail':{
                    'snapshot_records':7, 'recovery_prefix_records':6,
                    'profile':self.profile,
                    'records':[{'ordinal':6, 'source_event_sequence':42,
                                'record_bytes':len(f'tail-{index}'),
                                'bytes_hex':f'tail-{index}'.encode().hex()}],
                },
            }
        self.manifest = {
            'schema':1, 'kind':'production-model-journal-cold-history-corpus',
            'core_profile':self.profile, 'sessions':4,
            'requested_output_bytes_per_session':output_bytes,
            'resize_events_per_session':resize_events,
            'logical_hours_metadata':24.0, 'verification_requested':True,
            'results':self.rows,
        }
        (self.corpus / 'manifest.json').write_text(json.dumps(self.manifest))
        (self.run / 'runner.json').write_text(json.dumps({
            'status':'completed', 'exit_code':0, 'verification_requested':True,
        }))
        files = vm.corpus_whitelist(self.corpus, self.manifest)
        inventory = vm.inventory_paths(self.corpus, files)
        source_inventory = {name:value for name,value in inventory.items() if name != 'manifest.json'}
        reference = {
            'status':'completed', 'source_inventory_unchanged':True,
            'corpus':str(self.corpus.resolve()), 'binary_sha256':vm.sha256(self.binary),
            'core_profile':self.profile, 'source_inventory':source_inventory,
            'samples':[{'generations':reference_rows}],
        }
        self.reference_path.write_text(json.dumps(reference))

    def validate(self, *, allow_small=True):
        return vm.validate_corpus(self.corpus, self.reference_path, self.binary, allow_small)


class CorpusPreflightTests(unittest.TestCase):
    def test_completed_small_fixture_requires_explicit_override(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            with self.assertRaisesRegex(RuntimeError, 'exactly 1 GiB'):
                fixture.validate(allow_small=False)
            result = fixture.validate(allow_small=True)
            self.assertEqual(result['manifest']['sessions'], 4)
            self.assertEqual(len(result['files']), 4 * 5 + 1)

    def test_running_runner_is_rejected_before_fixture_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            (fixture.run / 'runner.json').write_text(json.dumps({
                'status':'running', 'verification_requested':True,
            }))
            with self.assertRaisesRegex(RuntimeError, 'not completed successfully'):
                fixture.validate()


    def test_partial_manifest_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            (fixture.corpus / 'manifest.json').unlink()
            with self.assertRaisesRegex(RuntimeError, 'manifest.json is missing'):
                fixture.validate()

    def test_missing_live_oracle_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            fixture.rows[0]['verification'].pop('live_cold_history_equal')
            (fixture.corpus / 'manifest.json').write_text(json.dumps(fixture.manifest))
            with self.assertRaisesRegex(RuntimeError, 'lacks full/fast/live equality'):
                fixture.validate()

    def test_wrong_profile_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            fixture.manifest['core_profile'] = 'different-terminal-profile'
            (fixture.corpus / 'manifest.json').write_text(json.dumps(fixture.manifest))
            with self.assertRaisesRegex(RuntimeError, 'core profile does not match'):
                fixture.validate()

    def test_unlisted_artifact_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            (fixture.corpus / 'unexpected.bin').write_bytes(b'no')
            with self.assertRaisesRegex(RuntimeError, 'unexpected file'):
                fixture.validate()

    def test_completed_recovery_runtime_artifacts_are_ignored_and_never_staged(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            for name in ('host.lock', 'metadata.sqlite', 'metadata.sqlite-wal',
                         'metadata.sqlite-shm'):
                (fixture.corpus / name).write_bytes(b'runtime')
            cache = fixture.corpus / '.history-replay-abcdef'
            cache.mkdir()
            (cache / '.pigade-replay-owner').write_bytes(b'lease')
            (cache / 'rows.index').write_bytes(b'cache index')
            (cache / '00000000000000000000.archive').write_bytes(b'cache archive')
            endpoint = __import__('socket').socket(__import__('socket').AF_UNIX)
            endpoint.bind(str(fixture.corpus / 'host.sock'))
            endpoint.close()

            paths = vm.corpus_whitelist(fixture.corpus, fixture.manifest)
            self.assertNotIn('host.lock', paths)
            self.assertNotIn('metadata.sqlite', paths)
            self.assertNotIn('.history-replay-abcdef/rows.index', paths)
            result = fixture.validate()

            destination = Path(temporary) / 'staged-corpus'
            copied = []

            def fake_copy(command, **kwargs):
                self.assertIn('--reflink=always', command)
                self.assertNotEqual(command[-2], str(fixture.corpus / '.'))
                source, target = Path(command[-2]), Path(command[-1])
                self.assertIn(source.relative_to(fixture.corpus).as_posix(), result['files'])
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)
                copied.append(target.relative_to(destination).as_posix())
                return ''

            with mock.patch.object(vm, 'run', side_effect=fake_copy):
                vm.copy_reflink_fixture(fixture.corpus, destination, result)
            self.assertEqual(set(copied), set(result['files']))
            self.assertFalse((destination / 'host.lock').exists())
            self.assertFalse((destination / 'metadata.sqlite-wal').exists())
            self.assertFalse((destination / cache.name).exists())

    def test_unknown_replay_cache_child_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            cache = fixture.corpus / '.history-replay-abcdef'
            cache.mkdir()
            (cache / 'unknown.data').write_bytes(b'unknown')
            with self.assertRaisesRegex(RuntimeError, 'unknown file'):
                vm.corpus_whitelist(fixture.corpus, fixture.manifest)

    def test_reference_must_match_frozen_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            reference = json.loads(fixture.reference_path.read_text())
            reference['binary_sha256'] = '0' * 64
            fixture.reference_path.write_text(json.dumps(reference))
            with self.assertRaisesRegex(RuntimeError, 'different session-host binary'):
                fixture.validate()

    def test_reference_requires_exactly_bounded_canonical_tail(self):
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CorpusFixture(Path(temporary))
            reference = json.loads(fixture.reference_path.read_text())
            generation = fixture.rows[0]['generation']
            del reference['samples'][0]['generations'][generation]['canonical_tail']
            fixture.reference_path.write_text(json.dumps(reference))
            with self.assertRaisesRegex(RuntimeError, 'lacks the canonical checkpoint-tail oracle'):
                fixture.validate()


class SerialResultTests(unittest.TestCase):
    def test_partial_guest_result_line_waits_for_lf_before_json_decode(self):
        marker = 'PIGADE_VM_RESULT:nonce:'
        partial = 'PIGADE_BOOT:one\n' + marker + '{"status":"recovery_verified","second_boot_recovery":{"records'
        self.assertIsNone(vm.completed_serial_payload(partial, marker))

        completed = partial + '":4}}\n'
        self.assertEqual(vm.completed_serial_payload(completed, marker),
                         '{"status":"recovery_verified","second_boot_recovery":{"records":4}}')

    def test_result_with_unterminated_json_line_is_not_accepted(self):
        marker = 'PIGADE_VM_RESULT:nonce:'
        serial = 'banner\r\n' + marker + '{"status":"recovery_verified"}'
        self.assertIsNone(vm.completed_serial_payload(serial, marker))

    def test_complete_result_line_supports_crlf_serial_termination(self):
        marker = 'PIGADE_VM_RESULT:nonce:'
        serial = 'banner\r\n' + marker + '{"status":"ok"}\r\n'
        self.assertEqual(vm.completed_serial_payload(serial, marker), '{"status":"ok"}')


class QemuPlanTests(unittest.TestCase):
    @unittest.skipUnless(hasattr(os, 'pidfd_open') and hasattr(signal, 'pidfd_send_signal'),
                         'Linux pidfd support required')
    def test_identity_failure_after_spawn_reaps_only_owned_child(self):
        with tempfile.TemporaryDirectory() as temporary:
            run_dir = Path(temporary)
            sdk = {'qemu_system_x86_64': Path(sys.executable),
                   'qemu_data_dir': run_dir, 'seabios_bios_256k': run_dir}
            args = __import__('argparse').Namespace(sdk_manifest=run_dir / 'sdk.json', timeout=60)
            child = None
            spawn = subprocess.Popen

            def owned_spawn(*args, **kwargs):
                nonlocal child
                child = spawn(*args, **kwargs)
                return child

            with mock.patch.object(vm, 'sdk_paths', return_value=sdk), \
                    mock.patch.object(vm, 'build_qemu_command', return_value=
                                      [sys.executable, '-c', 'import time; time.sleep(60)']), \
                    mock.patch.object(vm, 'process_identity', side_effect=RuntimeError('identity fault')), \
                    mock.patch.object(vm.subprocess, 'Popen', side_effect=owned_spawn):
                with self.assertRaisesRegex(RuntimeError, 'identity fault'):
                    vm.run_vm_with_qmp(run_dir, {}, args, run_dir / 'qmp')
            self.assertIsNotNone(child)
            self.assertIn(child.poll(), (-signal.SIGTERM, -signal.SIGKILL))

    def test_process_identity_reads_exact_current_pid(self):
        boot, pid, started = vm.process_identity(os.getpid())
        self.assertEqual(pid, os.getpid())
        self.assertEqual(boot, Path('/proc/sys/kernel/random/boot_id').read_text().strip())
        self.assertGreater(started, 0)

    def test_offline_image_tool_uses_private_sdk_library_path(self):
        environment = vm.sdk_tool_environment({
            'qemu_system_x86_64': Path('/private/sdk/usr/bin/qemu-system-x86_64')})
        self.assertEqual(environment['LD_LIBRARY_PATH'],
                         '/private/sdk/usr/lib:/private/sdk/usr/lib/qemu')
        self.assertNotIn('LD_PRELOAD', environment)

    def guest_function(self, name):
        rendered = vm.user_data('0' * 32, corpus_mode=True,
                                data_serial='pgd-data-01234567')
        config = json.loads(rendered.split('\n', 1)[1])
        scenario = next(item['content'] for item in config['write_files']
                        if item['path'].endswith('pigade-vm-test.py'))
        parsed = ast.parse(scenario)
        function = next(item for item in parsed.body
                        if isinstance(item, ast.FunctionDef) and item.name == name)
        namespace = {'pathlib':__import__('pathlib'), 'os':os, 'json':json,
                     'hashlib':hashlib, 'time':__import__('time'),
                     'subprocess':__import__('subprocess'), 'SOCKET':Path('/state/host.sock'),
                     'STATE':Path('/state'), 'ROOT':Path('/tmp')}
        return scenario, compile(ast.Module(body=[function], type_ignores=[]),
                                 '<guest-function>', 'exec'), namespace

    def test_guest_corpus_copy_body_runs_and_preserves_inventory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source'
            source.mkdir()
            (source / 'nested').mkdir()
            (source / 'nested' / 'corpus.bin').write_bytes(b'fixture bytes')
            inventory = {'nested/corpus.bin': {
                'bytes':len(b'fixture bytes'),
                'sha256':hashlib.sha256(b'fixture bytes').hexdigest(),
            }}
            data = root / 'data'
            data.mkdir()
            scenario, code, namespace = self.guest_function('copy_corpus_to_data_disk')

            def durable_json(path, value):
                path.write_text(json.dumps(value))

            namespace.update({
                'CORPUS_DATA':data, 'CORPUS_SOURCE':source,
                'canonical_inventory_hash':lambda value: hashlib.sha256(
                    json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest(),
                'verify_inventory':lambda root, value: vm.inventory_paths(root, list(value)),
                'durable_json':durable_json,
                'ensure':lambda condition, message: self.assertTrue(condition, message),
            })
            exec(code, namespace)
            with mock.patch('os.path.ismount', return_value=True):
                target, digest = namespace['copy_corpus_to_data_disk']({'source_inventory':inventory})
            self.assertEqual(digest, hashlib.sha256(
                json.dumps(inventory, sort_keys=True, separators=(',', ':')).encode()).hexdigest())
            self.assertEqual((target / 'nested' / 'corpus.bin').read_bytes(), b'fixture bytes')
            self.assertTrue((data / 'corpus-copy.json').is_file())
            compile(scenario, '<guest-scenario>', 'exec')

    def test_guest_startup_readiness_probes_selected_runtime_endpoint(self):
        _scenario, code, namespace = self.guest_function('start_host')
        state = Path('/private/state')
        runtime = Path('/private/runtime')
        namespace['ROOT'] = Path(tempfile.gettempdir())
        seen = []
        namespace['ipc'] = lambda command, endpoint=None: seen.append((command, endpoint)) or {}
        exec(code, namespace)
        with tempfile.TemporaryDirectory() as temporary:
            namespace['ROOT'] = Path(temporary)
            with mock.patch.object(namespace['subprocess'], 'Popen', return_value=object()):
                namespace['start_host'](Path(temporary) / 'state', Path(temporary) / 'runtime')
            self.assertEqual(seen[-1][1], Path(temporary) / 'runtime' / 'host.sock')

    def test_corpus_guest_uses_private_runtime_socket(self):
        rendered = vm.user_data('0' * 32, corpus_mode=True,
                                data_serial='pgd-data-01234567')
        cloud_config = json.loads(rendered.split('\n', 1)[1])
        scenario = next(item['content'] for item in cloud_config['write_files']
                        if item['path'].endswith('pigade-vm-test.py'))
        self.assertIn("runtime = pathlib.Path('/run/pigade-vm-runtime')", scenario)
        self.assertIn("endpoint = runtime / 'host.sock'", scenario)
        self.assertIn("endpoint=endpoint", scenario)

    def test_data_disk_serial_is_unique_per_run_nonce(self):
        first = vm.corpus_disk_serial('0123456789abcdef0123456789abcdef')
        second = vm.corpus_disk_serial('1123456789abcdef0123456789abcdef')
        self.assertEqual(first, 'pgd-data-01234567')
        self.assertNotEqual(first, second)

    def test_corpus_command_has_unique_serial_and_private_readonly_fixture(self):
        qemu = Path('/opt/qemu/bin/qemu-system-x86_64')
        sdk = {'qemu_data_dir':Path('/opt/qemu/share'), 'seabios_bios_256k':Path('/opt/qemu/bios')}
        manifest = {'artifacts':{
            'serial_log':'/private/run/serial.log', 'disk':'/private/run/root.qcow2',
            'seed':'/private/run/seed.img', 'stage':'/private/run/stage',
            'corpus_data_disk':'/private/run/corpus-data.qcow2',
            'corpus_fixture':'/private/run/corpus-source',
        }, 'data_disk':{'device_serial':'pgd-data-01234567'}}
        command = vm.build_qemu_command(qemu, sdk, Path('/private/run'), manifest, Path('/tmp/q'))
        device = command[command.index('-device') + 1]
        self.assertEqual(device, 'virtio-blk-pci,drive=pigade-corpus-data,serial=pgd-data-01234567')
        self.assertIn('-nic', command)
        self.assertEqual(command[command.index('-nic') + 1], 'none')
        self.assertIn('-nodefaults', command)
        self.assertEqual(command[command.index('-display') + 1], 'none')
        virtfs = [command[index + 1] for index, value in enumerate(command) if value == '-virtfs']
        self.assertEqual(len(virtfs), 2)
        self.assertTrue(all('readonly=on' in item for item in virtfs))
        self.assertTrue(any('mount_tag=pigade_corpus' in item and '/private/run/corpus-source' in item
                            for item in virtfs))

    def test_synthetic_command_keeps_existing_isolation_shape(self):
        qemu = Path('/opt/qemu/bin/qemu-system-x86_64')
        sdk = {'qemu_data_dir':Path('/opt/qemu/share'), 'seabios_bios_256k':Path('/opt/qemu/bios')}
        manifest = {'artifacts':{
            'serial_log':'/private/run/serial.log', 'disk':'/private/run/root.qcow2',
            'seed':'/private/run/seed.img', 'stage':'/private/run/stage',
        }}
        command = vm.build_qemu_command(qemu, sdk, Path('/private/run'), manifest, Path('/tmp/q'))
        self.assertNotIn('-device', command)
        self.assertEqual(command.count('-virtfs'), 1)
        self.assertEqual(command[command.index('-nic') + 1], 'none')
        self.assertIn('-nodefaults', command)


if __name__ == '__main__':
    unittest.main()
