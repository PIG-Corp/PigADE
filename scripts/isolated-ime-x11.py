#!/usr/bin/env python3
"""Plan or run one strictly isolated PigADE X11/XIM Pinyin acceptance probe.

Default behavior is plan-only. `--run` uses a fresh bwrap PID/user/network
namespace and private Xvfb/Fcitx/home/DBus/runtime; it never connects to a host
display or accepts arbitrary text for synthetic input.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import resource
import select
import shutil
import signal
import socket
import sqlite3
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_APP = ROOT / 'crates/app/target/debug/pigade-app'
DEFAULT_HOST = ROOT / 'target/release/pigade-session-host'
X11_SDK = Path('/tmp/pigade-x11-ime-sdk-20261017')
PLATFORM_SDK = Path('/tmp/pigade-platform-sdk')
FONT_MANIFEST = ROOT / 'crates/app/assets/fonts/derived-terminal-v1/derived-font-manifest.json'
SCREEN = (1280, 800, 24)
NAMESPACES = ('mnt', 'pid', 'user', 'ipc', 'net')
MAX_IPC = 8 * 1024 * 1024
MAX_CAPTURE_BYTES = 1280 * 800 * 3 + 128
MAX_LATENCY_DIAGNOSTIC_BYTES = 4096
FIXTURE_TOKEN = '__PIGADE_IME_CHILD_ACK_20261017__'
KEYBOARD_EXPECTED = b'ab1Z\x03\x04\x1a\x12\x0c\x01\x1bx\x1b[D\x1b[H\r' + 'привет'.encode('utf-8')
KEYBOARD_PASS = 'pass-private-x11-en-ru-cli-keyboard-fixture'
IME_PASS = 'pass-private-x11-xim-pinyin-commit-fixture'
LATENCY_PASS = 'pass-private-x11-fixed-input-cell-latency-fixture'
CONTEXT_PASS = 'pass-private-x11-workspace-context-editor-fixture'
MEMORY_PASS = 'pass-private-x11-startup-memory-review-fixture'
GENERATIONS_PASS = 'pass-private-x11-generation-history-selection-fixture'
RESUME_PASS = 'pass-private-x11-codex-resume-review-fixture'
LATENCY_KEYS = bytes(97 + index % 26 for index in range(50))
HOST_STATES = {'running', 'exited', 'starting', 'degraded', 'recovering'}
ACTOR_STAGES = {'waiting', 'pty_resize', 'journal_append', 'history_begin', 'model_apply',
                'history_commit', 'checkpoint', 'frame_build', 'publish', 'query_responses',
                'exited', 'degraded'}
LATENCY_FAILURE_REASONS = {'color_deadline', 'keycode_unavailable',
                           'key_submission_failed', 'pixel_readback_failed'}
FIXTURE = r'''import json, os, select, socket, sys, termios, time, tty
control_path = sys.argv[1]
expected = bytes.fromhex(sys.argv[2])
assert 0 < len(expected) <= 128
fd = sys.stdin.fileno()
original = termios.tcgetattr(fd)
control = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
control.connect(control_path)
try:
    tty.setraw(fd, termios.TCSANOW)
    os.write(1, b"PIGADE-IME-READY\r\n")
    control.sendall(b"READY\n")
    while True:
        command = control.recv(64)
        if not command:
            raise SystemExit(31)
        if command.startswith(b"CHECK\n"):
            ready, _, _ = select.select([fd], [], [], 0.20)
            if ready:
                data = os.read(fd, 64)
                control.sendall(b"PRECOMMIT_INPUT:" + data.hex().encode() + b"\n")
                raise SystemExit(32)
            control.sendall(b"NO_PRECOMMIT_INPUT\n")
        elif command.startswith(b"COLLECT\n"):
            payload = bytearray()
            deadline = time.monotonic() + 3.0
            while len(payload) < len(expected) and time.monotonic() < deadline:
                ready, _, _ = select.select([fd], [], [], max(0, deadline-time.monotonic()))
                if ready:
                    payload.extend(os.read(fd, len(expected)-len(payload)))
            extra_ready, _, _ = select.select([fd], [], [], 0.35)
            extra = os.read(fd, 64) if extra_ready else b""
            exact = bytes(payload) == expected and not extra
            control.sendall(("RESULT:" + json.dumps({
                "received_bytes": len(payload),
                "synthetic_fixture_payload_hex": payload.hex(),
                "synthetic_fixture_extra_hex": extra.hex(),
                "payload_sha256": __import__("hashlib").sha256(payload).hexdigest(),
                "exact_utf8": bytes(payload) == expected,
                "extra_bytes": len(extra),
                "exact_once": exact,
            }, sort_keys=True) + "\n").encode())
            if exact:
                os.write(1, ("\r\n" + "__PIGADE_IME_CHILD_ACK_20261017__" + "\r\n").encode())
            else:
                os.write(1, b"\r\n__PIGADE_IME_CHILD_REJECT__\r\n")
            break
        else:
            raise SystemExit(33)
finally:
    termios.tcsetattr(fd, termios.TCSANOW, original)
    control.close()
'''


LATENCY_FIXTURE = r'''import os, select, socket, sys, termios, tty
fd = sys.stdin.fileno()
original = termios.tcgetattr(fd)
control = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
control.connect(sys.argv[1])
assert sys.argv[2] == bytes(97 + index % 26 for index in range(50)).hex()
def emit(payload):
    remaining = memoryview(payload)
    while remaining:
        written = os.write(1, remaining)
        remaining = remaining[written:]
try:
    tty.setraw(fd, termios.TCSANOW)
    emit(b"PIGADE-IME-READY\r\n")
    control.sendall(b"READY\n")
    assert control.recv(64) == b"START\n"
    for index in range(50):
        assert os.read(fd, 1) == bytes([97 + index % 26])
        emit(("\x1b[48;2;%d;239;27m\x1b[2J\x1b[H" % (17 + 4 * index)).encode())
    assert not select.select([fd], [], [], 0.35)[0], "extra fixture input"
    control.sendall(b"FIFTY_EXACT_KEYS\n")
    emit(b"__PIGADE_IME_CHILD_ACK_20261017__\r\n")
finally:
    termios.tcsetattr(fd, termios.TCSANOW, original)
    control.close()
'''


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def load_script(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    require(spec is not None and spec.loader is not None, f'cannot load {path.name}')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def safe_file(path: Path, label: str) -> Path:
    resolved = path.resolve(strict=True)
    require(resolved.is_file() and not path.is_symlink(), f'{label} must be a regular non-symlink file')
    require(resolved.stat().st_uid == os.getuid(), f'{label} is not user-owned')
    return resolved


def validate_sdk(sdk_dir: Path, manifest_path: Path, preparer_path: Path) -> dict:
    manifest = json.loads(safe_file(manifest_path, 'X11 SDK manifest').read_text())
    preparer = load_script(preparer_path, 'x11_ime_sdk_preparer_for_verification')
    require(manifest.get('status') == 'ready-for-isolated-review', 'X11 SDK status is not ready')
    require(manifest.get('preparer_sha256') == sha256(preparer_path), 'X11 SDK preparer source changed')
    root = Path(manifest.get('extraction_root', '')).resolve(strict=True)
    require(root.is_relative_to(sdk_dir.resolve(strict=True)), 'X11 extracted SDK escapes its root')
    expected = {row['name']: row for row in preparer.PACKAGES}
    packages = manifest.get('packages')
    require(isinstance(packages, list) and len(packages) == len(expected), 'X11 SDK package set differs')
    require({row.get('name') for row in packages} == set(expected), 'X11 SDK package names differ')
    downloads = sdk_dir / 'downloads'
    signatures = sdk_dir / 'signatures'
    if not signatures.exists():
        signatures = downloads
    verified = []
    for row in packages:
        pin = expected[row['name']]
        require(row.get('archive_sha256') == pin['sha256'], f"SDK archive pin differs: {pin['name']}")
        archive = downloads / preparer.package_filename(pin)
        signature = signatures / (archive.name + '.sig')
        safe_file(archive, 'X11 package archive')
        safe_file(signature, 'X11 package signature')
        require(sha256(archive) == pin['sha256'], f"SDK archive hash mismatch: {pin['name']}")
        require(sha256(signature) == row.get('signature_sha256'), f"SDK signature hash mismatch: {pin['name']}")
        checked = preparer.verify_signature(signature, archive, sdk_dir / 'keyring/archlinux.gpg',
                                            sdk_dir / 'keyring/gnupg', pin['signer'])
        require(checked['primary_fingerprint'] == row.get('verified_signer', {}).get('primary_fingerprint'),
                f"SDK signer mismatch: {pin['name']}")
        verified.append({'package': pin['name'], 'sha256': pin['sha256'],
                         'signer': pin['signer'], 'signature_verified': True})
    for relative in preparer.REQUIRED_PAYLOADS:
        payload = root / relative
        require(payload.exists() and payload.resolve(strict=True).is_relative_to(root),
                f'missing or escaping X11 payload: {relative}')
    require(not manifest.get('unresolved_runtime_dependencies'), 'X11 SDK has unresolved runtime dependencies')
    return {'manifest_sha256': sha256(manifest_path), 'extraction_root': str(root),
            'package_set_id': root.name.removeprefix('x11-ime-'), 'packages': verified,
            'runtime_checks': len(manifest.get('runtime_ldd_checks', [])),
            'limits': manifest.get('limits', [])}


def validate_platform_sdk(sdk_dir: Path) -> dict:
    manifest_path = safe_file(sdk_dir / 'manifest.json', 'platform SDK manifest')
    manifest = json.loads(manifest_path.read_text())
    preparer_path = ROOT / 'scripts/prepare-platform-sdk.py'
    preparer = load_script(preparer_path, 'platform_sdk_preparer_for_verification')
    require(manifest.get('status') == 'prepared-not-executed', 'software-renderer SDK is not prepared')
    require(manifest.get('trusted_keyring_sha256') == sha256(Path('/usr/share/pacman/keyrings/archlinux.gpg')),
            'software-renderer trusted keyring differs from current validated keyring')
    require(len(manifest.get('packages', [])) == len(preparer.PACKAGES),
            'software-renderer package set differs from its preparer')
    keyring = safe_file(sdk_dir / 'keyring/archlinux.gpg', 'software-renderer keyring')
    for row, pin in zip(manifest['packages'], preparer.PACKAGES, strict=True):
        archive = sdk_dir / 'downloads' / Path(row['url']).name
        signature = archive.with_name(archive.name + '.sig')
        safe_file(archive, 'software-renderer package archive')
        safe_file(signature, 'software-renderer package signature')
        require(row['sha256'] == pin[1] and sha256(archive) == pin[1],
                'software-renderer package hash differs from pin')
        require(sha256(signature) == row['signature_sha256'],
                'software-renderer package signature hash differs')
        verification = subprocess.run(
            ['gpgv', '--homedir', str(sdk_dir / 'keyring'), '--keyring', str(keyring),
             '--status-fd', '1', str(signature), str(archive)],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, timeout=15, check=False)
        require(verification.returncode == 0 and
                f'[GNUPG:] VALIDSIG {preparer.SIGNER} ' in verification.stdout + verification.stderr,
                'software-renderer package signature/signer verification failed')
    icd = safe_file(Path(manifest['lavapipe_icd']), 'Lavapipe ICD')
    require(icd.is_relative_to(sdk_dir.resolve()), 'Lavapipe ICD escapes SDK')
    return {'manifest_sha256': sha256(manifest_path), 'lavapipe_icd_sha256': sha256(icd),
            'package_hashes_checked': len(manifest.get('packages', [])),
            'package_signatures_verified': len(manifest.get('packages', [])),
            'status': manifest['status']}


def process_identity(pid: int) -> tuple[str, int]:
    boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    raw = Path(f'/proc/{pid}/stat').read_text()
    rest = raw[raw.rfind(')') + 2:].split()
    require(len(rest) >= 20, 'short /proc process stat')
    return boot, int(rest[19])


def private_path(path: Path, *, must_not_exist: bool = False) -> Path:
    absolute = Path(os.path.abspath(path))
    require(absolute != ROOT and not absolute.is_relative_to(ROOT), 'run output must be outside the repository')
    for parent in (absolute, *absolute.parents):
        require(not parent.is_symlink(), f'run path contains symlink component: {parent}')
    if must_not_exist:
        require(not absolute.exists(), f'run output already exists: {absolute}')
    return absolute


def private_identity_files(uid: int, gid: int) -> tuple[str, str]:
    passwd = ('root:x:0:0:root:/home/pigade:/bin/sh\n'
              f'pigade:x:{uid}:{gid}:Isolated test:/home/pigade:/usr/bin/bash\n')
    group = f'root:x:0:\npigade:x:{gid}:\n'
    return passwd, group


def compile_helper(source: Path, output: Path) -> dict:
    safe_file(source, 'X11 helper source')
    require(not output.exists() and not output.is_symlink(), 'helper output must be new')
    flags = subprocess.run(['pkg-config', '--cflags', '--libs', 'x11', 'xtst', 'xcb', 'xcb-res'],
                           stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, text=True, timeout=5, check=True).stdout.split()
    subprocess.run(['cc', '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror', str(source),
                    '-o', str(output), *flags], stdin=subprocess.DEVNULL,
                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20, check=True)
    output.chmod(0o700)
    return {'path': str(output), 'sha256': sha256(output), 'source_sha256': sha256(source),
            'libraries': ['X11', 'Xtst', 'xcb', 'xcb-res']}


def app_command(publication_trace=False) -> list[str]:
    command = ['/pigade/app', '/artifacts/state/host.sock']
    if publication_trace is True:
        command.append('--diagnostic-publication-trace')
    return command


def window_verified(result: dict) -> bool:
    return result.get('window_verified') is True


def select_private_pinyin(config: dict, home: Path, runtime: Path, auth: Path) -> str:
    env = {
        'PATH': '/usr/bin', 'HOME': str(home),
        'XDG_CONFIG_HOME': str(home / '.config'),
        'XDG_CACHE_HOME': str(home / '.cache'),
        'XDG_DATA_HOME': str(home / '.local/share'),
        'XDG_STATE_HOME': str(home / '.local/state'),
        'XDG_RUNTIME_DIR': str(runtime), 'TMPDIR': '/tmp',
        'DISPLAY': config['display'], 'XAUTHORITY': str(auth),
        'XMODIFIERS': '@im=fcitx', 'LC_ALL': 'C.utf8', 'LC_CTYPE': 'C.utf8',
        'DBUS_SESSION_BUS_ADDRESS': 'unix:path=' + str(runtime / 'bus'),
        'LD_LIBRARY_PATH': os.environ.get('LD_LIBRARY_PATH', ''),
    }
    remote = config['x11_root'] + '/usr/bin/fcitx5-remote'
    for arguments in (['-s', 'pinyin'], ['-o']):
        result = subprocess.run([remote, *arguments], stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, timeout=3, check=False, env=env)
        require(result.returncode == 0 and len(result.stdout) <= 1024 and
                len(result.stderr) <= 1024,
                'private Fcitx could not select/activate Pinyin')
    result = subprocess.run([remote, '-n'], stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, timeout=3, check=False, env=env)
    require(result.returncode == 0 and result.stdout.strip() == 'pinyin' and
            len(result.stderr) <= 1024,
            'private Fcitx did not confirm Pinyin: rc=' + str(result.returncode) +
            ' current=' + repr(result.stdout[:128]) + ' stderr=' + repr(result.stderr[:128]))
    return result.stdout.strip()


def prepare_private_keyboard(config: dict, env: dict) -> None:
    # Only called after inside_run proves namespace, server and XIM ownership.
    result = subprocess.run([config['x11_root'] + '/usr/bin/setxkbmap',
                             '-display', config['display'], '-layout', 'us,ru', '-option', ''],
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True, timeout=5, check=False, env=env)
    require(result.returncode == 0 and len(result.stdout) <= 4096 and len(result.stderr) <= 4096,
            'private US/RU keyboard map load failed: ' + result.stderr[-240:])


def deactivate_private_fcitx(config, env):
    remote = config['x11_root'] + '/usr/bin/fcitx5-remote'
    result = subprocess.run([remote, '-c'], stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, timeout=3, check=False, env=env)
    require(result.returncode == 0 and len(result.stdout) <= 1024 and len(result.stderr) <= 1024,
            'private Fcitx keyboard deactivation failed')
    result = subprocess.run([remote], stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, timeout=3, check=False, env=env)
    require(result.returncode == 0 and result.stdout.strip() == '1',
            'private Fcitx did not confirm inactive state')


def fixture_status(name):
    return {'keyboard': KEYBOARD_PASS, 'pinyin': IME_PASS, 'latency': LATENCY_PASS,
            'context': CONTEXT_PASS, 'memory': MEMORY_PASS,
            'generations': GENERATIONS_PASS, 'resume': RESUME_PASS}[name]


def seed_context_catalog(session, memory_fixture=False):
    catalog = host_request({'command': 'catalog'})['catalog']
    project, workspace, tab = (str(uuid.uuid4()) for _ in range(3))
    selection = dict(catalog['selection'], project_id=project, workspace_id=workspace, tab_id=tab)
    catalog = host_request({'command': 'catalog_apply', 'batch': {
        'projects': [{'expected_revision': None, 'value': {
            'id': project, 'canonical_path': '/artifacts', 'git_common_dir': '/artifacts',
            'name': 'Context fixture', 'roots': [], 'revision': 0}}],
        'workspaces': [{'expected_revision': None, 'value': {
            'id': workspace, 'project_id': project, 'task': 'Context editor fixture',
            'cwd': '/artifacts', 'mode': 'shared_checkout', 'status': 'ready',
            'error': None, 'context_revision': None, 'revision': 0}}],
        'tabs': [{'expected_revision': None, 'value': {
            'id': tab, 'workspace_id': workspace, 'name': 'Development',
            'layout_json': 'null', 'active_pane': None, 'revision': 0}}],
        'selection': {'expected_revision': selection['revision'], 'value': selection},
        'layout': None}})['catalog']
    host_request({'command': 'layout_edit', 'expected_revision': catalog['layout']['revision'],
                  'action': {'action': 'apply', 'command': {'Insert': {
                      'tab': tab, 'pane': session, 'target': None, 'side': 'Right',
                      'split': str(uuid.uuid4())}}}})
    if memory_fixture:
        memory = '# Факты\n' + ('x' * (40 * 1024)) + '\n## Решения\nSELECTED_MEMORY λ決定\n## Что осталось\nLATER\n'
        host_request({'command': 'save_context', 'expected_revision': 0, 'document': {
            'scope_id': workspace, 'revision': 0, 'instructions': '', 'role': '',
            'memory': memory, 'content_hash': '', 'saved_at_unix_ms': None, 'author': ''}})
    return {'project': project, 'workspace': workspace, 'tab': tab}


def prepare_resume_fixture(report):
    fixture = report['fixture']
    raw_session, raw_generation = fixture['session'], fixture['generation']
    require(host_request({'command': 'stop', 'session': raw_session,
                         'generation': raw_generation}).get('error') is None,
            'could not stop the owned bootstrap PTY')
    wait_until(lambda: (model if (model := fixture_session_models(raw_session)).get(
        raw_generation, {}).get('state') == 'exited' else None), time.monotonic() + 8)
    workspace, tab = fixture['workspace'], fixture['tab']
    context = 'FIXTURE_FROZEN_RESUME_CONTEXT'
    host_request({'command': 'save_context', 'expected_revision': 0, 'document': {
        'scope_id': workspace, 'revision': 0, 'instructions': context, 'role': '',
        'memory': '', 'content_hash': '', 'saved_at_unix_ms': None, 'author': ''}})
    preview = host_request({'command': 'prepare_launch', 'request': {
        'tab': tab, 'provider': 'codex', 'side': 'Right', 'target': raw_session,
        'role_override': '', 'agent_instructions': '', 'include_memory': False,
        'memory_selection': None, 'deliver_context': True}})['launch_preview']
    require(preview.get('provider') == 'codex' and preview.get('can_launch'),
            'private Codex initial preview is unavailable')
    host_request({'command': 'start_prepared', 'plan_id': preview['plan_id']})
    session, generation = preview['session'], preview['generation']
    def codex_exited():
        views = host_request({'command': 'list'}).get('views', [])
        return views if any(v.get('session') == session and v.get('generation') == generation
                            and v.get('state') == 'exited' for v in views) else None
    wait_until(codex_exited, time.monotonic() + 8)
    conversation_id = '01234567-89ab-cdef-0123-456789abcdef'
    host_request({'command': 'bind_conversation', 'session': session,
                  'generation': generation, 'conversation_id': conversation_id,
                  'expected_revision': 0})
    fixture.update({'codex_session': session, 'codex_generation': generation,
                    'conversation_id': conversation_id, 'frozen_context': context})


def prepared_launches_read_only():
    connection = sqlite3.connect('file:/artifacts/state/metadata.sqlite?mode=ro', uri=True, timeout=2)
    try:
        rows = connection.execute('SELECT id,payload,phase FROM prepared_launches').fetchall()
        require(len(rows) <= 64, 'fixture prepared launch count exceeds diagnostic bound')
        return {row[0]: {'payload': json.loads(row[1]), 'phase': row[2]} for row in rows}
    finally:
        connection.close()


def fixture_generations():
    reply = host_request({'command': 'poll', 'known': []})
    views = reply.get('views', [])
    require(isinstance(views, list) and len(views) <= 32, 'fixture generation snapshot exceeds bounds')
    return sorted((view['session'], view['generation']) for view in views)


def fixture_session_models(session):
    reply = host_request({'command': 'list'})
    views = reply.get('views', [])
    require(isinstance(views, list) and len(views) <= 32,
            'fixture generation snapshot exceeds bounds')
    matching = [view for view in views if view.get('session') == session]
    require(len(matching) <= 2, 'fixture session has an unexpected generation count')
    return {view['generation']: view for view in matching}


def fixture_generation_list(session):
    result = host_request({'command': 'generations', 'session': session, 'before': None})
    generations = result.get('generations')
    require(isinstance(generations, dict) and generations.get('session') == session,
            'host omitted fixture generation metadata')
    entries = generations.get('entries')
    require(isinstance(entries, list) and len(entries) <= 32,
            'fixture generation metadata exceeds bounds')
    return generations


def fixture_panel_pin(session):
    catalog = host_request({'command': 'catalog'}).get('catalog')
    require(isinstance(catalog, dict), 'host omitted fixture catalog')
    selection = catalog.get('selection', {})
    window = selection.get('window', {})
    pins = window.get('panel_generations', {})
    require(isinstance(pins, dict), 'fixture panel generation pins have invalid shape')
    return pins.get(session)


def fixture_journal_hashes(generations):
    result = {}
    state = Path('/artifacts/state')
    for generation in generations:
        path = state / (generation + '.journal')
        require(path.is_file() and not path.is_symlink(),
                'fixture generation journal is unavailable')
        require(path.stat().st_size <= 64 * 1024 * 1024,
                'fixture journal exceeds the private diagnostic bound')
        result[generation] = sha256(path)
    return result


def run_generations_fixture(config, report, env, save):
    report['stage'] = 'generation-history-selection'
    save()
    deactivate_private_fcitx(config, env)
    session = report['fixture']['session']
    first_generation = report['fixture']['generation']

    initial_models = fixture_session_models(session)
    require(set(initial_models) == {first_generation},
            'generation fixture did not start with its owned initial model')
    initial = initial_models[first_generation]
    require('PIGADE-IME-READY' in view_text(initial),
            'original owned fixture frame lost its ready marker')
    first_frame = initial.get('frame')
    stop = host_request({'command': 'stop', 'session': session,
                         'generation': first_generation})
    require(stop.get('error') is None, 'could not stop original private fixture generation')
    exited = wait_until(lambda: (models if (models := fixture_session_models(session)).get(
        first_generation, {}).get('state') == 'exited' else None), time.monotonic() + 8)
    require(exited[first_generation].get('frame') == first_frame,
            'stopping the original fixture mutated its retained frame')

    second_generation = '00000000-0000-4000-8000-000000000001'
    launch = host_request({'command': 'launch', 'spec': {
        'session': session, 'generation': second_generation, 'operation': str(uuid.uuid4()),
        'executable': '/usr/bin/python3',
        'args': ['-c', "import os; os.write(1, b'NEW_GENERATION_CELLS\\r\\n')"],
        'cwd': '/artifacts', 'geometry': {'rows': 24, 'cols': 80}}})
    require(launch.get('error') is None, 'second private fixture generation launch failed')
    wait_until(lambda: (models if (models := fixture_session_models(session)).get(
        second_generation, {}).get('state') == 'exited' and 'NEW_GENERATION_CELLS' in
        view_text(models[second_generation]) else None), time.monotonic() + 8)

    generation_list = fixture_generation_list(session)
    require(generation_list.get('head') == second_generation and
            len(generation_list['entries']) == 2 and
            generation_list['entries'][0].get('generation') == second_generation and
            generation_list['entries'][1].get('generation') == first_generation and
            generation_list['entries'][0].get('ordinal') == 2 and
            generation_list['entries'][1].get('ordinal') == 1,
            'durable generation head/order differs from launch chronology')
    require(int(first_generation.replace('-', ''), 16) >
            int(second_generation.replace('-', ''), 16),
            'fixture generation UUIDs do not oppose chronological order')

    expected_generations = {first_generation, second_generation}
    expected_models = fixture_session_models(session)
    require(set(expected_models) == expected_generations,
            'fixture created or lost a generation before UI selection')
    journal_hashes = fixture_journal_hashes(expected_generations)

    helper_call(config, 'generations-open')
    time.sleep(0.25)
    helper_call(config, 'capture', '/artifacts/generations-picker.ppm')
    require(0 < Path('/artifacts/generations-picker.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'generation picker capture exceeds bound')

    helper_call(config, 'generations-select-old')
    wait_until(lambda: fixture_panel_pin(session) == first_generation,
               time.monotonic() + 8)
    time.sleep(0.25)
    helper_call(config, 'capture', '/artifacts/generation-pinned-old.ppm')
    require(0 < Path('/artifacts/generation-pinned-old.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'pinned generation capture exceeds bound')
    pinned_list = fixture_generation_list(session)
    require(pinned_list.get('head') == second_generation and
            pinned_list.get('pinned') == first_generation and
            pinned_list['entries'][0].get('generation') == second_generation and
            pinned_list['entries'][0].get('ordinal') == 2 and
            pinned_list['entries'][1].get('generation') == first_generation and
            pinned_list['entries'][1].get('ordinal') == 1,
            'pin selection changed the durable head or failed to persist the old pin')
    require(fixture_session_models(session) == expected_models and
            fixture_journal_hashes(expected_generations) == journal_hashes,
            'historical selection mutated a journal or generation model/frame')

    helper_call(config, 'generations-open')
    time.sleep(0.25)
    helper_call(config, 'generations-follow')
    wait_until(lambda: fixture_panel_pin(session) is None, time.monotonic() + 8)
    time.sleep(0.25)
    helper_call(config, 'capture', '/artifacts/generation-follow-head.ppm')
    require(0 < Path('/artifacts/generation-follow-head.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'follow-head generation capture exceeds bound')
    followed_list = fixture_generation_list(session)
    require(followed_list.get('head') == second_generation and followed_list.get('pinned') is None and
            followed_list['entries'][0].get('generation') == second_generation and
            followed_list['entries'][0].get('ordinal') == 2 and
            followed_list['entries'][1].get('generation') == first_generation and
            followed_list['entries'][1].get('ordinal') == 1,
            'following current generation changed head or retained the old pin')
    require(fixture_session_models(session) == expected_models and
            fixture_journal_hashes(expected_generations) == journal_hashes,
            'follow-head selection mutated a journal or generation model/frame')
    require(fixture_generations() == sorted((session, generation) for generation in expected_generations),
            'generation selection spawned an extra PTY or changed the fixture generation set')
    require(window_verified(helper_call(config, 'probe-window')),
            'PigADE window disappeared during generation history fixture')
    report['generationHistory'] = {
        'head': second_generation, 'pinned_then_followed': first_generation,
        'ordinals': [2, 1], 'journals_unchanged': True,
        'models_and_cells_unchanged': True, 'extra_generations': False,
        'captures': ['generations-picker.ppm', 'generation-pinned-old.ppm',
                     'generation-follow-head.ppm'],
        'scope': 'isolated Xvfb and private host; two exited fixture-owned PTYs only',
    }
    report.update({'status': GENERATIONS_PASS, 'stage': 'complete'})
    save()


def run_memory_fixture(config, report, env, save):
    report['stage'] = 'startup-memory-review'
    save()
    deactivate_private_fcitx(config, env)
    baseline_generations = fixture_generations()
    baseline_plans = prepared_launches_read_only()
    helper_call(config, 'memory-open')
    initial_plans = wait_until(
        lambda: (plans if len(plans := prepared_launches_read_only()) > len(baseline_plans) else None),
        time.monotonic() + 8)
    initial_ids = set(initial_plans) - set(baseline_plans)
    require(len(initial_ids) == 1, 'opening memory review did not create exactly one preview plan')
    initial_plan = initial_plans[next(iter(initial_ids))]['payload']['preview']
    require(initial_plan.get('provider') == 'shell',
            'memory review fixture selected a provider other than the local shell adapter')
    require(initial_plan.get('memory', {}).get('selected') == [],
            'initial memory preview unexpectedly includes sections')
    require(not any(block.get('source') == 'workspace_memory'
                    for block in initial_plan['context']['blocks']),
            'initial memory review unexpectedly includes a memory block')
    time.sleep(0.25)
    helper_call(config, 'capture', '/artifacts/memory-review-initial.ppm')
    require(0 < Path('/artifacts/memory-review-initial.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'initial memory review capture exceeds bound')

    helper_call(config, 'memory-select')
    def selected_preview():
        plans = prepared_launches_read_only()
        new_ids = set(plans) - set(baseline_plans)
        candidates = [(plan_id, plans[plan_id]) for plan_id in new_ids
                      if plans[plan_id]['payload'].get('preview', {}).get('memory', {}).get('selected') == [1]]
        return candidates[0] if len(candidates) == 1 else None
    selected_plan_id, selected_plan = wait_until(selected_preview, time.monotonic() + 8)
    require(selected_plan['phase'] == 'prepared', 'selected review plan was already attempted')
    preview = selected_plan['payload']['preview']
    require(preview['include_memory'] is True, 'selected review did not include memory')
    memory_review = preview['memory']
    require(memory_review['revision'] > 0 and memory_review['selected_bytes'] == len('## Решения\nSELECTED_MEMORY λ決定\n'.encode()),
            'selected section metadata differs from the exact UTF-8 source slice')
    blocks = preview['context']['blocks']
    memory_blocks = [block for block in blocks if block.get('source') == 'workspace_memory']
    expected = '## Решения\nSELECTED_MEMORY λ決定\n'
    require(len(memory_blocks) == 1 and memory_blocks[0]['text'] == expected,
            'prepared context memory block differs from the selected source slice')
    require('x' * 128 not in preview['context']['text'] and 'LATER' not in preview['context']['text'],
            'unselected memory text leaked into prepared context')
    time.sleep(0.25)
    helper_call(config, 'capture', '/artifacts/memory-review-selected.ppm')
    require(0 < Path('/artifacts/memory-review-selected.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'selected memory review capture exceeds bound')
    require(fixture_generations() == baseline_generations,
            'memory preview created or changed a PTY generation')
    require(window_verified(helper_call(config, 'probe-window')),
            'PigADE window disappeared during memory review')
    report['memoryReview'] = {
        'plan_id': selected_plan_id, 'selected': memory_review['selected'],
        'selected_bytes': memory_review['selected_bytes'],
        'prepared_context_exact': True, 'unselected_text_absent': True,
        'pty_generations_unchanged': True, 'launch_confirmed': False,
        'captures': ['memory-review-initial.ppm', 'memory-review-selected.ppm'],
        'scope': 'isolated Xvfb and host metadata; prepared launch only',
    }
    report.update({'status': MEMORY_PASS, 'stage': 'complete'})
    save()


def run_resume_fixture(config, report, env, save):
    report['stage'] = 'codex-resume-review'
    save()
    deactivate_private_fcitx(config, env)
    session = report['fixture']['codex_session']
    generation = report['fixture']['codex_generation']
    baseline_models = fixture_session_models(session)
    require(set(baseline_models) == {generation} and
            'OLD_RESUME_FRAME' in view_text(baseline_models[generation]),
            'private Codex bootstrap generation is missing')
    old_frame = baseline_models[generation].get('frame')
    old_layout = host_request({'command': 'catalog'})['catalog']['layout']
    old_generations = fixture_generations()
    old_plans = prepared_launches_read_only()
    helper_call(config, 'resume-open')
    plans = wait_until(lambda: (value if len(value := prepared_launches_read_only()) > len(old_plans)
        and any((plan['payload'].get('preview', {}).get('resume') or {}).get('binding', {}).get(
            'conversation_id') == report['fixture']['conversation_id']
            for plan in value.values()) else None), time.monotonic() + 8)
    fresh = [(plan_id, plan) for plan_id, plan in plans.items() if plan_id not in old_plans and
             (plan['payload'].get('preview', {}).get('resume') or {}).get('binding', {}).get(
                 'conversation_id') == report['fixture']['conversation_id']]
    require(len(fresh) == 1 and fresh[0][1]['phase'] == 'prepared',
            'resume review did not create one unstarted frozen plan')
    preview = fresh[0][1]['payload']['preview']
    new_generation = preview.get('generation')
    require(preview.get('session') == session and new_generation != generation and
            preview.get('context', {}).get('text', '').find(report['fixture']['frozen_context']) >= 0,
            'resume preview differs from the bound session or frozen context')
    require(fixture_generations() == old_generations,
            'opening resume review created a terminal generation')
    time.sleep(0.2)
    helper_call(config, 'capture', '/artifacts/resume-review.ppm')
    helper_call(config, 'resume-confirm')
    def resumed():
        models = fixture_session_models(session)
        view = models.get(new_generation)
        return models if view and view.get('state') == 'exited' and \
            'NEW_RESUME_FRAME' in view_text(view) else None
    models = wait_until(resumed, time.monotonic() + 10)
    require(set(models) == {generation, new_generation} and
            models[generation].get('frame') == old_frame,
            'resume changed the old frame or created an extra generation')
    require(fixture_generations() == sorted(old_generations + [(session, new_generation)]),
            'resume changed another session or created an extra process')
    require(host_request({'command': 'catalog'})['catalog']['layout'] == old_layout,
            'resume changed layout placement or undo history')
    generation_list = fixture_generation_list(session)
    require(generation_list.get('head') == new_generation and
            {entry.get('generation') for entry in generation_list['entries']} ==
            {generation, new_generation}, 'resume did not preserve old head history')
    time.sleep(0.2)
    helper_call(config, 'capture', '/artifacts/resume-new-generation.ppm')
    report['resume'] = {'session': session, 'previous_generation': generation,
        'new_generation': new_generation, 'frozen_context_reviewed': True,
        'old_frame_unchanged': True, 'layout_undo_history_unchanged': True,
        'extra_generation': False, 'captures': ['resume-review.ppm', 'resume-new-generation.ppm'],
        'scope': 'private fake Codex and isolated host metadata'}
    report.update({'status': RESUME_PASS, 'stage': 'complete'})
    save()


def run_context_fixture(config, report, env, save):
    report['stage'] = 'workspace-context-editor'
    save()
    workspace = report['fixture']['workspace']
    get_context = lambda: host_request({'command': 'get_context', 'scope_id': workspace})['context']
    initial = get_context()
    require(initial['revision'] == 0 and initial['instructions'] == '',
            'workspace context fixture did not start empty')
    helper_call(config, 'context-open')
    time.sleep(0.5)
    active_im = select_private_pinyin(config, Path('/home/pigade'), Path('/run/pigade-test'),
                                      Path('/run/pigade-test/Xauthority'))
    helper_call(config, 'compose')
    time.sleep(0.2)
    helper_call(config, 'commit')
    time.sleep(0.2)
    deactivate_private_fcitx(config, env)
    helper_call(config, 'context-edit')
    time.sleep(0.2)
    helper_call(config, 'capture', '/artifacts/context-editor.ppm')
    require(0 < Path('/artifacts/context-editor.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'context editor capture exceeds bound')
    before_save = get_context()
    require(before_save['revision'] == 0 and before_save['instructions'] == '',
            'workspace context changed before explicit save')
    helper_call(config, 'context-save')
    expected = '你好xb'
    saved = wait_until(lambda: (document if (document := get_context())['revision'] == 1
                                and document['instructions'] == expected else None),
                       time.monotonic() + 8)
    require(saved['instructions'] == expected and saved['revision'] == 1,
            'saved workspace context differs from fixed edit sequence')
    report['contextEditor'] = {
        'workspace': workspace, 'initialRevision': initial['revision'],
        'unsavedRevisionRemainedZero': True, 'savedRevision': saved['revision'],
        'savedInstructions': expected, 'editSequence': 'Pinyin commit 你好; type ab; Left; Shift+Left; replace x; undo; redo; Ctrl+Enter',
        'privateInputMethod': active_im,
        'capture': 'context-editor.ppm', 'scope': 'private Xvfb window; isolated host metadata only',
    }
    report.update({'status': CONTEXT_PASS, 'stage': 'complete'})
    save()


def run_latency_fixture(config, report, control, app, fcitx, env, save):
    report['stage'] = 'fixed-input-cell-latency'
    save()
    deactivate_private_fcitx(config, env)
    report['fcitx']['activation_state'] = 1
    control.sendall(b'START\n')
    samples = helper_call(config, 'latency', timeout=60)
    values = samples.get('samples_ns')
    require(isinstance(values, list) and len(values) == 50 and
            all(type(value) is int and 0 < value <= 1_000_000_000 for value in values),
            'latency helper omitted bounded numeric samples')
    require(json_line(control, timeout=3) == 'FIFTY_EXACT_KEYS',
            'latency fixture did not receive exactly50 ordered fixed keys')
    ordered = sorted(values)
    report['latency'] = {**samples, 'sample_count': 50,
                         'p50_ns': ordered[24], 'p95_ns': ordered[47],
                         'p99_ns': ordered[49], 'max_ns': ordered[-1],
                         'quantile': 'nearest-rank',
                         'sample_interval_poll_sleep_ns': 1_000_000,
                         'scope': 'software Xvfb/Lavapipe observed-cell diagnostic; physical input/present gate unclaimed'}
    def ack_seen():
        views = host_request({'command': 'poll', 'known': []}).get('views', [])
        return bool(views and FIXTURE_TOKEN in view_text(views[0]))
    wait_until(ack_seen, time.monotonic() + 8)
    report['latency']['child_ack_seen'] = True
    time.sleep(0.25)
    helper_call(config, 'capture', '/artifacts/committed.ppm')
    require(app.poll() is None and fcitx.poll() is None, 'app/Fcitx exited during latency probe')
    report.update({'status': LATENCY_PASS, 'stage': 'complete'})
    save()


def run_keyboard_fixture(config, report, control, app, fcitx, env, artifacts, save):
    report['stage'] = 'native-en-ru-cli-keyboard'
    save()
    deactivate_private_fcitx(config, env)
    report['fcitx']['activation_state'] = 1
    control.sendall(b'CHECK\n')
    require(json_line(control, timeout=2) == 'NO_PRECOMMIT_INPUT',
            'PTY received bytes before fixed keyboard sequence')
    helper_call(config, 'keyboard')
    control.sendall(b'COLLECT\n')
    line = json_line(control, timeout=5)
    require(line.startswith('RESULT:'), 'keyboard fixture omitted bounded result')
    result = json.loads(line[len('RESULT:'):])
    expected_hash = hashlib.sha256(KEYBOARD_EXPECTED).hexdigest()
    report['keyboard'] = {'expected_bytes': len(KEYBOARD_EXPECTED),
                           'expected_sha256': expected_hash, **result,
                           'requested_layouts': ['us', 'ru'], 'verified_group_indices': [0, 1],
                           'modifiers_clear_before_after': True,
                           'controls': ['Ctrl-C', 'Ctrl-D', 'Ctrl-Z', 'Ctrl-R', 'Ctrl-L', 'Ctrl-A',
                                        'Alt-x', 'Left', 'Home', 'Enter'],
                           'pre_input_bytes': 0}
    save()
    require(result.get('received_bytes') == len(KEYBOARD_EXPECTED) and
            result.get('payload_sha256') == expected_hash and result.get('exact_once') is True and
            result.get('extra_bytes') == 0, 'native EN/RU/CLI bytes differed from fixed expectation')
    def ack_seen():
        views = host_request({'command': 'poll', 'known': []}).get('views', [])
        return bool(views and FIXTURE_TOKEN in view_text(views[0]))
    wait_until(ack_seen, time.monotonic() + 8)
    time.sleep(0.25)
    helper_call(config, 'capture', '/artifacts/committed.ppm')
    require(0 < (artifacts / 'committed.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'keyboard capture exceeds bound')
    require(app.poll() is None and fcitx.poll() is None, 'app or Fcitx exited during keyboard fixture')
    report['keyboard']['child_ack_seen'] = True
    report.update({'status': KEYBOARD_PASS, 'stage': 'complete'})
    save()


def wait_for_private_xim(config: dict, timeout: float = 8.0) -> dict:
    deadline = time.monotonic() + timeout
    last_error = 'XIM owner has not appeared yet'
    while time.monotonic() < deadline:
        try:
            return helper_call(config, 'preflight', timeout=2)
        except RuntimeError as error:
            last_error = str(error)[:160]
            time.sleep(0.1)
    raise RuntimeError('private Fcitx XIM readiness timed out: ' + last_error)


def inside_config(args, output, helper, x11_evidence, platform_evidence):
    extracted_root = Path(x11_evidence['extraction_root'])
    return {
        'app_sha256': sha256(args.app), 'host_sha256': sha256(args.host),
        'helper_sha256': sha256(helper), 'harness_sha256': sha256(Path(__file__)),
        'x11_manifest_sha256': x11_evidence['manifest_sha256'],
        'platform_manifest_sha256': platform_evidence['manifest_sha256'],
        'x11_root': '/pigade/x11-sdk', 'x11_extraction_root': str(extracted_root),
        'platform_root': str(args.platform_sdk / 'root'),
        'private_icd': '/pigade/platform-sdk/lavapipe-icd.json',
        'private_icd_host': str(output / 'lavapipe-icd.json'),
        'publication_trace': getattr(args, 'publication_trace', False) is True,
        'app_command': app_command(getattr(args, 'publication_trace', False)),
        'host_command': ['/pigade/host', 'serve', '/artifacts/state'],
        'screen': list(SCREEN), 'namespaces': list(NAMESPACES),
        'fixture_token': FIXTURE_TOKEN, 'fixture': getattr(args, 'fixture', 'pinyin'),
    }


def bwrap_command(args, config_path, helper):
    config = json.loads(config_path.read_text())
    x11_root = config['x11_root']
    xkb_source = (Path(config['x11_extraction_root']) / 'usr/share/X11/xkb').resolve()
    xkb_target = Path('/usr/share/X11/xkb').resolve(strict=True)
    require(xkb_target.is_relative_to('/usr/share'), 'host XKB destination escapes read-only /usr/share')
    args_list = [
        'bwrap', '--unshare-all', '--new-session', '--die-with-parent', '--clearenv',
        '--ro-bind', '/usr', '/usr', '--symlink', 'usr/bin', '/bin',
        '--symlink', 'usr/lib', '/lib', '--symlink', 'usr/lib', '/lib64',
        '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/run',
        '--dir', '/home', '--dir', '/etc', '--dir', '/pigade', '--dir', '/pigade/platform-sdk',
        '--ro-bind', '/etc/fonts', '/etc/fonts',
        '--ro-bind', '/etc/ld.so.cache', '/etc/ld.so.cache',
        # Bind the resolved directory, not /usr/share/X11/xkb's symlink.
        # Xvfb's compiled absolute xkbcomp path also gets the pinned SDK binary.
        '--ro-bind', str(xkb_source), str(xkb_target),
        '--ro-bind', str(Path(config['x11_extraction_root']) / 'usr/bin/xkbcomp'),
        '/usr/bin/xkbcomp',
        '--ro-bind', str(Path(config['x11_extraction_root']) / 'usr/lib/fcitx5'),
        '/usr/lib/fcitx5',
        '--ro-bind', str(Path(config['x11_extraction_root']) / 'usr/share/fcitx5'),
        '/usr/share/fcitx5',
        '--ro-bind', config['x11_extraction_root'], '/pigade/x11-sdk',
        '--ro-bind', config['platform_root'], '/pigade/platform-sdk/root',
        '--ro-bind', config['private_icd_host'], config['private_icd'],
        '--ro-bind', str(args.output / 'passwd'), '/etc/passwd',
        '--ro-bind', str(args.output / 'group'), '/etc/group',
        '--bind', str(args.output), '/artifacts',
        '--ro-bind', str(args.app), '/pigade/app',
        '--ro-bind', str(args.host), '/pigade/host',
        '--ro-bind', str(helper), '/pigade/x11-ime-input',
        '--ro-bind', str(Path(__file__).resolve()), '/pigade/isolated-ime-x11.py',
        '--chdir', '/artifacts',
    ]
    for key, value in {
        'PATH': '/usr/bin', 'LC_ALL': 'C.utf8', 'LC_CTYPE': 'C.utf8',
        'HOME': '/home/pigade', 'XDG_CONFIG_HOME': '/home/pigade/.config',
        'XDG_CACHE_HOME': '/home/pigade/.cache', 'XDG_DATA_HOME': '/home/pigade/.local/share',
        'XDG_STATE_HOME': '/home/pigade/.local/state', 'XDG_RUNTIME_DIR': '/run/pigade-test',
        'TMPDIR': '/tmp', 'XKB_CONFIG_ROOT': '/usr/share/X11/xkb',
        'LD_LIBRARY_PATH': x11_root + '/usr/lib:/pigade/platform-sdk/root/usr/lib:/pigade/platform-sdk/root/usr/lib/weston',
        'LIBIME_MODEL_DIRS': x11_root + '/usr/lib/libime',
        'VK_DRIVER_FILES': '/pigade/platform-sdk/lavapipe-icd.json',
        'VK_ICD_FILENAMES': '/pigade/platform-sdk/lavapipe-icd.json',
        'LIBGL_ALWAYS_SOFTWARE': 'true', 'FONTCONFIG_FILE': '/etc/fonts/fonts.conf',
    }.items():
        args_list.extend(['--setenv', key, value])
    args_list.extend(['/usr/bin/python3', '/pigade/isolated-ime-x11.py', '--inside'])
    return args_list


def read_framed(stream: socket.socket) -> dict:
    def read_exact(n):
        data = bytearray()
        while len(data) < n:
            chunk = stream.recv(n - len(data))
            require(chunk, 'truncated private host IPC')
            data.extend(chunk)
        return bytes(data)
    size, = struct.unpack('<I', read_exact(4))
    require(0 < size <= MAX_IPC, 'host IPC frame exceeds bound')
    value = json.loads(read_exact(size))
    require(value.get('protocol') == 2 and value.get('error') is None,
            'private host rejected fixture request')
    return value


def host_request(command: dict, timeout=4) -> dict:
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(timeout)
        connection.connect('/artifacts/state/host.sock')
        payload = json.dumps({'protocol': 2, 'command': command}, separators=(',', ':')).encode()
        connection.sendall(struct.pack('<I', len(payload)) + payload)
        return read_framed(connection)


def wait_until(check, deadline: float, interval=0.05):
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(interval)
    raise RuntimeError('bounded isolated IME condition expired')


LOG_SUMMARIES = []
LOG_THREADS = []
LOG_LOCK = threading.Lock()
LOG_LIMIT = 64 * 1024


def _drain_log(stream, path, process_name):
    saved = 0
    digest = hashlib.sha256()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
    try:
        while chunk := stream.read(8192):
            digest.update(chunk)
            if saved < LOG_LIMIT:
                portion = chunk[:LOG_LIMIT - saved]
                os.write(fd, portion)
                saved += len(portion)
    finally:
        os.fsync(fd); os.close(fd); stream.close()
    with LOG_LOCK:
        LOG_SUMMARIES.append({'process': process_name, 'saved_bytes': saved,
                              'truncated_at_bytes': LOG_LIMIT, 'stream_sha256': digest.hexdigest()})


def start_process(argv, *, env=None, pass_fds=(), name=None, stdout=subprocess.PIPE):
    Path('/artifacts/logs').mkdir(mode=0o700, exist_ok=True)
    process_name = name or Path(argv[0]).name
    safe_name = re.sub(r'[^A-Za-z0-9_.-]+', '_', process_name)[:48]
    log_path = Path('/artifacts/logs') / f'{safe_name}-{uuid.uuid4().hex[:8]}.log'
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=stdout,
                               stderr=subprocess.STDOUT if stdout == subprocess.PIPE else subprocess.DEVNULL,
                               close_fds=True, start_new_session=True, env=env, pass_fds=pass_fds)
    if stdout == subprocess.PIPE:
        thread = threading.Thread(target=_drain_log, args=(process.stdout, log_path, safe_name),
                                  daemon=True)
        LOG_THREADS.append(thread); thread.start()
    return process


def read_display_fd(fd, timeout=8):
    ready, _, _ = select.select([fd], [], [], timeout)
    require(ready, 'Xvfb did not report a display number')
    raw = os.read(fd, 64)
    require(raw.endswith(b'\n') and re.fullmatch(rb'[0-9]+\n', raw), 'Xvfb returned invalid display number')
    return int(raw[:-1])


def json_line(control, timeout=4):
    control.settimeout(timeout)
    raw = bytearray()
    while len(raw) <= 4096:
        part = control.recv(1)
        require(part, 'fixture control socket closed early')
        if part == b'\n':
            return bytes(raw).decode('utf-8')
        raw.extend(part)
    raise RuntimeError('fixture control record exceeded bound')


def view_text(view):
    frame = view.get('frame', {})
    cells = frame.get('cells', [])
    return ''.join(cell.get('text', '') for cell in cells if isinstance(cell, dict))


def helper_call(config, stage, output=None, timeout=5):
    argv = ['/pigade/x11-ime-input', '--stage', stage,
            '--display', config['display'], '--boot-id', config['boot_id'],
            '--xvfb-pid', str(config['xvfb_pid']), '--xvfb-start', str(config['xvfb_start']),
            '--xvfb-socket-inode', str(config['xvfb_socket_inode']),
            '--fcitx-pid', str(config['fcitx_pid']), '--fcitx-start', str(config['fcitx_start'])]
    if stage != 'preflight':
        argv += ['--app-pid', str(config['app_pid']), '--app-start', str(config['app_start'])]
    if output:
        argv += ['--output', output]
    result = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True, timeout=timeout, check=False,
                            env={'PATH': '/usr/bin', 'DISPLAY': config['display'],
                                 'XAUTHORITY': '/run/pigade-test/Xauthority',
                                 'XMODIFIERS': '@im=fcitx', 'LC_ALL': 'C.utf8',
                                 'LD_LIBRARY_PATH': os.environ['LD_LIBRARY_PATH']})
    require(len(result.stdout) <= MAX_LATENCY_DIAGNOSTIC_BYTES and
            len(result.stderr) <= MAX_LATENCY_DIAGNOSTIC_BYTES, 'helper output exceeded bound')
    if result.returncode != 0:
        error = RuntimeError('private X11 helper failed its identity/protocol check')
        if stage == 'latency':
            try:
                error.latency_diagnostic = parse_latency_helper_diagnostic(result.stdout)
                error.latency_diagnostic_status = 'validated'
            except (ValueError, TypeError, json.JSONDecodeError):
                error.latency_diagnostic_status = 'invalid-or-missing'
            if getattr(error, 'latency_diagnostic_status', None) == 'validated':
                error.args = ('fixed latency ' + error.latency_diagnostic['reason'] + '; no key retry',)
        elif result.stderr:
            error.args = (error.args[0] + ': ' + result.stderr[:160],)
        raise error
    require(result.returncode == 0,
            'private X11 helper failed its identity/protocol check: ' + result.stderr.strip()[:240])
    return json.loads(result.stdout)


def _bounded_uint(value, maximum=(1 << 64) - 1):
    return type(value) is int and 0 <= value <= maximum


def _rgb_string(value):
    if not isinstance(value, str):
        return None
    match = re.fullmatch(r'Spec\(Rgb \{ r: (\d+), g: (\d+), b: (\d+) \}\)', value)
    if not match:
        return None
    channels = [int(channel) for channel in match.groups()]
    return channels if all(channel <= 255 for channel in channels) else None


def sanitize_latency_host_snapshot(reply, session, expected_rgb):
    if not isinstance(reply, dict) or not isinstance(reply.get('views'), list) or len(reply['views']) > 32:
        raise ValueError('host poll response shape invalid')
    matching = [view for view in reply['views']
                if isinstance(view, dict) and view.get('session') == session]
    result = {'view_count': len(reply['views']), 'matching_fixture_views': len(matching)}
    if len(matching) != 1:
        return result
    view = matching[0]
    frame = view.get('frame')
    if not isinstance(frame, dict):
        raise ValueError('host frame shape invalid')
    geometry = frame.get('geometry')
    cells = frame.get('cells')
    if not isinstance(geometry, dict) or not isinstance(cells, list):
        raise ValueError('host frame fields invalid')
    rows, cols = geometry.get('rows'), geometry.get('cols')
    if (type(rows) is not int or type(cols) is not int or not 0 <= rows <= 512 or
            not 0 <= cols <= 512 or rows * cols > 65536 or len(cells) > 65536):
        raise ValueError('host frame geometry exceeds diagnostic bounds')
    sequence = view.get('sequence')
    if not _bounded_uint(sequence):
        raise ValueError('host sequence invalid')
    state = view.get('state')
    if state not in HOST_STATES:
        state = 'other'
    cursor = frame.get('cursor')
    cursor_summary = None
    if (isinstance(cursor, list) and len(cursor) == 2 and type(cursor[0]) is int and
            type(cursor[1]) is int and -512 <= cursor[0] <= 512 and 0 <= cursor[1] <= 512):
        cursor_summary = cursor
    modes = frame.get('modes')
    if not _bounded_uint(modes, (1 << 32) - 1):
        modes = None
    palette = frame.get('palette_overrides')
    palette_summary = []
    if isinstance(palette, list) and len(palette) <= 269:
        for entry in palette:
            if (not isinstance(entry, list) or len(entry) != 2 or type(entry[0]) is not int or
                    not 0 <= entry[0] <= 268 or not isinstance(entry[1], list) or
                    len(entry[1]) != 3 or any(type(x) is not int or not 0 <= x <= 255 for x in entry[1])):
                raise ValueError('host palette entry invalid')
            palette_summary.append([entry[0], list(entry[1])])
    else:
        raise ValueError('host palette exceeds diagnostic bounds')
    cell_samples = []
    for index, cell in enumerate(cells[:50]):
        if not isinstance(cell, dict) or not isinstance(cell.get('text'), str):
            raise ValueError('host cell shape invalid')
        text = cell['text']
        text_bytes = len(text.encode('utf-8'))
        if text_bytes > 128:
            raise ValueError('host cell text length exceeds diagnostic bound')
        text_class = ('empty' if not text else 'fixed_latency_ascii'
                      if len(text) == 1 and text.encode('utf-8') in LATENCY_KEYS else 'redacted')
        bg = _rgb_string(cell.get('bg'))
        cell_samples.append({'index': index, 'text_class': text_class, 'text_bytes': text_bytes,
                             'bg_rgb': bg,
                             'matches_failed_expected_rgb': bg == expected_rgb})
    actor = view.get('actor_diagnostic')
    actor_summary = None
    if actor is not None:
        if not isinstance(actor, dict):
            raise ValueError('actor diagnostic shape invalid')
        stage = actor.get('stage')
        if stage is not None and stage not in ACTOR_STAGES:
            stage = 'other'
        reader = actor.get('reader', {})
        if not isinstance(reader, dict):
            raise ValueError('actor reader diagnostic shape invalid')
        numeric_fields = ('read_calls', 'bytes_read', 'last_read_bytes', 'enqueued_calls',
                          'bytes_enqueued')
        actor_summary = {'version': actor.get('version') if _bounded_uint(actor.get('version'), 255) else None,
                         'sample_available': actor.get('status') == 'available',
                         'stage': stage,
                         'candidate_sequence': actor.get('candidate_sequence')
                             if actor.get('candidate_sequence') is None or _bounded_uint(actor.get('candidate_sequence')) else None,
                         'entered_monotonic_ns': actor.get('entered_monotonic_ns')
                             if actor.get('entered_monotonic_ns') is None or _bounded_uint(actor.get('entered_monotonic_ns')) else None,
                         'reader': {key: reader.get(key) if _bounded_uint(reader.get(key)) else None
                                    for key in numeric_fields}}
    history_lines = frame.get('history_lines')
    pending = frame.get('sync_pending_bytes')
    return {**result, 'sequence': sequence, 'state': state, 'pid_present': view.get('pid') is not None,
            'storage_error_present': isinstance(view.get('storage_error'), str) and bool(view['storage_error']),
            'geometry': {'rows': rows, 'cols': cols}, 'cell_count': len(cells),
            'cursor': cursor_summary, 'modes': modes,
            'history_lines': history_lines if _bounded_uint(history_lines) else None,
            'sync_pending_bytes': pending if _bounded_uint(pending) else None,
            'palette_overrides': palette_summary, 'cell_samples': cell_samples,
            'actor_diagnostic': actor_summary}


def collect_latency_failure_diagnostics(config, report, artifacts, error):
    diagnostics = {'helper_status': getattr(error, 'latency_diagnostic_status', 'unavailable')}
    validated = getattr(error, 'latency_diagnostic', None)
    if validated is not None:
        diagnostics['helper'] = validated
        expected = validated['expected_rgb']
    else:
        expected = None
    try:
        reply = host_request({'command': 'poll', 'known': []}, timeout=4)
        diagnostics['host_poll'] = sanitize_latency_host_snapshot(
            reply, report.get('fixture', {}).get('session'), expected)
    except BaseException as snapshot_error:
        diagnostics['host_poll'] = {'status': 'unavailable',
                                    'error_type': type(snapshot_error).__name__}
    try:
        helper_call(config, 'capture', '/artifacts/latency-failure.ppm', timeout=8)
        capture = artifacts / 'latency-failure.ppm'
        info = capture.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_size <= 0 or info.st_size > MAX_CAPTURE_BYTES:
            raise ValueError('private capture type/size invalid')
        diagnostics['capture'] = {'status': 'captured', 'file': capture.name,
                                  'bytes': info.st_size, 'sha256': sha256(capture),
                                  'scope': 'private Xvfb root'}
    except BaseException as capture_error:
        diagnostics['capture'] = {'status': 'unavailable',
                                  'error_type': type(capture_error).__name__}
    return diagnostics


def capture_latency_failure_diagnostics(config, report, artifacts, error):
    """Best-effort bounded evidence collection; never changes the original failure."""
    try:
        return collect_latency_failure_diagnostics(config, report, artifacts, error)
    except BaseException as diagnostic_error:
        return {'status': 'unavailable', 'error_type': type(diagnostic_error).__name__}


def parse_latency_helper_diagnostic(payload):
    if isinstance(payload, str):
        payload = payload.encode('utf-8')
    if not isinstance(payload, bytes) or not 0 < len(payload) <= MAX_LATENCY_DIAGNOSTIC_BYTES:
        raise ValueError('latency diagnostic size/type invalid')
    value = json.loads(payload)
    expected_fields = {'version', 'stage', 'scope', 'pixel', 'sample_count', 'samples_ns',
                       'failed_index', 'expected_rgb', 'observed_rgb', 'elapsed_ns', 'reason'}
    if not isinstance(value, dict) or set(value) != expected_fields:
        raise ValueError('latency diagnostic schema mismatch')
    if (type(value['version']) is not int or value['version'] != 1 or
            value['stage'] != 'latency' or
            value['scope'] != 'XTEST-submit to private root pixel readback' or
            value['pixel'] != [20, 120] or
            not isinstance(value['pixel'], list) or
            any(type(item) is not int for item in value['pixel'])):
        raise ValueError('latency diagnostic identity mismatch')
    failed = value['failed_index']
    samples = value['samples_ns']
    if (type(failed) is not int or not 0 <= failed < 50 or
            type(value['sample_count']) is not int):
        raise ValueError('latency diagnostic index invalid')
    if (not isinstance(samples, list) or len(samples) != failed or value['sample_count'] != failed or
            any(type(sample) is not int or not 0 < sample <= 1_000_000_000 for sample in samples)):
        raise ValueError('latency diagnostic samples invalid')
    def rgb(candidate):
        return (isinstance(candidate, list) and len(candidate) == 3 and
                all(type(channel) is int and 0 <= channel <= 255 for channel in candidate))
    expected = [17 + 4 * failed, 239, 27]
    if value['expected_rgb'] != expected or not rgb(value['observed_rgb']):
        raise ValueError('latency diagnostic color invalid')
    elapsed = value['elapsed_ns']
    if type(elapsed) is not int or not 0 < elapsed <= 60_000_000_000:
        raise ValueError('latency diagnostic duration invalid')
    reason = value['reason']
    if reason not in LATENCY_FAILURE_REASONS or (reason == 'color_deadline' and elapsed <= 1_000_000_000):
        raise ValueError('latency diagnostic reason invalid')
    return {'version': 1, 'stage': 'latency', 'scope': value['scope'], 'pixel': [20, 120],
            'sample_count': failed, 'samples_ns': list(samples), 'failed_index': failed,
            'expected_rgb': expected, 'observed_rgb': list(value['observed_rgb']),
            'elapsed_ns': elapsed, 'reason': reason}


def atomic_json(path: Path, value: dict):
    temporary = path.with_suffix(path.suffix + '.tmp')
    data = (json.dumps(value, sort_keys=True, indent=2) + '\n').encode()
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(data); output.flush(); os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()


def run_bwrap(command: list[str], output: Path) -> int:
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               stderr=subprocess.PIPE, close_fds=True,
                               start_new_session=True,
                               env={'PATH': '/usr/bin', 'HOME': str(output), 'LC_ALL': 'C.utf8'})
    drain = threading.Thread(target=_drain_log,
                             args=(process.stderr, output / 'bwrap.stderr.log', 'bwrap'),
                             daemon=True)
    drain.start()
    try:
        return process.wait(timeout=100)
    except subprocess.TimeoutExpired:
        process.kill()  # Exact unreaped direct child; bwrap --die-with-parent tears down its namespace.
        process.wait(timeout=5)
        raise
    except BaseException:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)
        raise
    finally:
        drain.join(timeout=2)


def inside_run() -> int:
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    artifacts = Path('/artifacts')
    report = {'schema': 1, 'status': 'running', 'stage': 'namespace-preflight'}
    children: list[subprocess.Popen] = []
    output = artifacts / 'run.json'
    def save(): atomic_json(output, report)
    try:
        config = json.loads((artifacts / 'config.json').read_text())
        runtime = Path('/run/pigade-test'); runtime.mkdir(mode=0o700)
        home = Path('/home/pigade'); home.mkdir(mode=0o700)
        for relative in ('.config/fcitx5', '.cache', '.local/share', '.local/state'):
            (home / relative).mkdir(mode=0o700, parents=True, exist_ok=True)
        namespaces = {name: os.readlink('/proc/self/ns/' + name) for name in NAMESPACES}
        require(all(namespaces[name] != config['outer_namespaces'][name] for name in NAMESPACES),
                'required namespace boundary missing')
        for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'XAUTHORITY',
                     'DBUS_SESSION_BUS_ADDRESS', 'HYPRLAND_INSTANCE_SIGNATURE', 'SWAYSOCK',
                     'MIR_SOCKET', 'XDG_ACTIVATION_TOKEN', 'SSH_AUTH_SOCK'):
            require(name not in os.environ, f'host desktop/session variable inherited: {name}')
        for path in ('/run/user', '/tmp/.X11-unix', '/dev/input', '/dev/uinput', '/dev/dri',
                     '/dev/nvidia0', '/dev/nvidiactl'):
            require(not Path(path).exists(), f'host runtime/device path exposed: {path}')
        route_rows = Path('/proc/net/route').read_text().splitlines()[1:]
        ipv6_route_rows = Path('/proc/net/ipv6_route').read_text().splitlines()
        require(all(row.split()[0] == 'lo' for row in route_rows), 'network namespace has a non-loopback IPv4 route')
        require(all(row.split()[-1] == 'lo' for row in ipv6_route_rows), 'network namespace has a non-loopback IPv6 route')
        netdev_names = {line.split(':', 1)[0].strip() for line in Path('/proc/net/dev').read_text().splitlines()[2:]}
        require(netdev_names <= {'lo'}, 'network namespace exposes a non-loopback interface')
        require(stat.S_IMODE(runtime.stat().st_mode) == 0o700 and runtime.stat().st_uid == os.getuid(),
                'private runtime permissions/ownership are wrong')
        report['preflight'] = {'namespaces': namespaces, 'uid': os.getuid(),
                               'host_display_bus_devices_absent': True,
                               'network_interfaces': sorted(netdev_names),
                               'non_loopback_routes': 0}
        report['stage'] = 'xvfb-start'
        save()
        private_x_socket_dir = Path('/tmp/.X11-unix')
        private_x_socket_dir.mkdir(mode=0o1777)
        private_x_socket_dir.chmod(0o1777)
        auth = runtime / 'Xauthority'
        auth.write_bytes(b''); auth.chmod(0o600)
        cookie = os.urandom(16).hex()
        authority_commands = runtime / 'xauth-commands'
        authority_commands.write_text(''.join(
            f'add :{number} MIT-MAGIC-COOKIE-1 {cookie}\n' for number in range(64)))
        authority_commands.chmod(0o600)
        auth_result = subprocess.run(
            [config['x11_root'] + '/usr/bin/xauth', '-f', str(auth), 'source', str(authority_commands)],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            timeout=4, check=False,
            env={'PATH': '/usr/bin', 'HOME': str(home), 'XAUTHORITY': str(auth)})
        require(auth_result.returncode == 0, 'private Xauthority cookie setup failed')
        authority_commands.unlink()
        pipe_r, pipe_w = os.pipe(); os.set_inheritable(pipe_w, True)
        xvfb_path = config['x11_root'] + '/usr/bin/Xvfb'
        xvfb = start_process([xvfb_path, '-displayfd', str(pipe_w), '-screen', '0',
                              f'{SCREEN[0]}x{SCREEN[1]}x{SCREEN[2]}', '-nolisten', 'tcp',
                              '-extension', 'GLX',
                              '-auth', str(auth)], env={k: v for k, v in os.environ.items()
                                                       if k in ('PATH', 'HOME', 'XDG_CONFIG_HOME',
                                                                'XDG_CACHE_HOME', 'XDG_DATA_HOME',
                                                                'XDG_STATE_HOME', 'XDG_RUNTIME_DIR',
                                                                'TMPDIR', 'LC_ALL', 'LD_LIBRARY_PATH',
                                                                'XKB_CONFIG_ROOT')}, pass_fds=(pipe_w,))
        children.append(xvfb); os.close(pipe_w)
        display_number = read_display_fd(pipe_r); os.close(pipe_r)
        require(display_number < 64, 'Xvfb selected a display outside the preauthorized private range')
        display = f':{display_number}'
        socket_path = Path(f'/tmp/.X11-unix/X{display_number}')
        wait_until(lambda: socket_path.is_socket() and xvfb.poll() is None, time.monotonic() + 8)
        socket_stat = socket_path.lstat()
        boot_id, xvfb_start = process_identity(xvfb.pid)
        config.update({'display': display, 'boot_id': boot_id, 'xvfb_pid': xvfb.pid,
                       'xvfb_start': xvfb_start, 'xvfb_socket_inode': socket_stat.st_ino})
        report['xvfb'] = {'pid': xvfb.pid, 'start_time_ticks': xvfb_start,
                          'display': display, 'socket_inode': socket_stat.st_ino,
                          'transport': 'private local Unix socket', 'tcp': False}
        setxkb = subprocess.run([config['x11_root'] + '/usr/bin/setxkbmap', '-display', display,
                                 '-layout', 'us'], stdin=subprocess.DEVNULL,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                timeout=5, check=False,
                                env={'PATH': config['x11_root'] + '/usr/bin:/usr/bin',
                                     'HOME': str(home), 'DISPLAY': display,
                                     'XAUTHORITY': str(auth), 'XKB_CONFIG_ROOT': '/usr/share/X11/xkb',
                                     'LD_LIBRARY_PATH': os.environ['LD_LIBRARY_PATH']})
        require(setxkb.returncode == 0, 'private X keyboard layout setup failed')
        report['stage'] = 'private-dbus-fcitx'
        save()
        bus_path = runtime / 'bus'
        address = 'unix:path=' + str(bus_path)
        bus_config = runtime / 'dbus.conf'
        bus_config.write_text('<busconfig><type>session</type><listen>' + address +
                              '</listen><auth>EXTERNAL</auth><policy context="default">'
                              '<allow own="*"/><allow send_destination="*"/>'
                              '<allow receive_sender="*"/></policy></busconfig>\n')
        bus_config.chmod(0o600)
        # No service directories/includes: prevent unrelated host-installed
        # portal/desktop services from being autoactivated inside the namespace.
        dbus = start_process(['/usr/bin/dbus-daemon', '--nofork', '--config-file=' + str(bus_config)],
                             name='dbus-daemon',
                             env={'PATH': '/usr/bin', 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime),
                                  'TMPDIR': '/tmp', 'LC_ALL': 'C.utf8'})
        children.append(dbus)
        wait_until(lambda: bus_path.is_socket() and dbus.poll() is None, time.monotonic() + 5)
        profile = home / '.config/fcitx5/profile'
        profile.write_text('''[Groups/0]\nName=Default\nDefault Layout=us\nDefaultIM=pinyin\n\n[Groups/0/Items/0]\nName=keyboard-us\nLayout=us\n\n[Groups/0/Items/1]\nName=pinyin\n\n[GroupOrder]\n0=Default\n''')
        profile.chmod(0o600)
        plugin_dir = Path(config['x11_root']) / 'usr/lib/fcitx5'
        model_dir = Path(config['x11_root']) / 'usr/lib/libime'
        require((model_dir / 'zh_CN.lm').is_file(), 'pinned libime model is missing')
        require((plugin_dir / 'libpinyin.so').is_file(), 'pinned Pinyin plugin missing')
        fcitx_env = {'PATH': '/usr/bin', 'HOME': str(home), 'XDG_CONFIG_HOME': str(home / '.config'),
                     'XDG_CACHE_HOME': str(home / '.cache'), 'XDG_DATA_HOME': str(home / '.local/share'),
                     'XDG_STATE_HOME': str(home / '.local/state'), 'XDG_RUNTIME_DIR': str(runtime),
                     'TMPDIR': '/tmp', 'DISPLAY': display, 'XAUTHORITY': str(auth),
                     'XMODIFIERS': '@im=fcitx', 'LC_ALL': 'C.utf8', 'LC_CTYPE': 'C.utf8',
                     'DBUS_SESSION_BUS_ADDRESS': address,
                     'LD_LIBRARY_PATH': os.environ['LD_LIBRARY_PATH'],
                     'XDG_DATA_DIRS': str(Path(config['x11_root']) / 'usr/share') + ':/usr/share',
                     'LIBIME_MODEL_DIRS': str(model_dir),
                     'FCITX_ADDON_DIRS': str(plugin_dir), 'FCITX_DATA_DIRS': str(Path(config['x11_root']) / 'usr/share/fcitx5')}
        fcitx = start_process([config['x11_root'] + '/usr/bin/fcitx5', '--replace'], env=fcitx_env)
        children.append(fcitx)
        boot, fcitx_start = process_identity(fcitx.pid)
        require(boot == boot_id, 'Fcitx process boot identity differs')
        config.update({'fcitx_pid': fcitx.pid, 'fcitx_start': fcitx_start})
        wait_for_private_xim(config)
        report['fcitx'] = {'pid': fcitx.pid, 'start_time_ticks': fcitx_start,
                           'private_profile': True, 'xim_owner_verified_by': 'XRes local client PID',
                           'xim_selection': '@server=fcitx', 'input_method': 'pinyin'}
        report['stage'] = 'host-fixture'
        save()
        control_path = runtime / 'ime-fixture.sock'
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(control_path)); os.chmod(control_path, 0o600); listener.listen(1); listener.settimeout(8)
        host_path = '/usr/bin'
        if config.get('fixture') == 'resume':
            private_bin = Path('/artifacts/bin'); private_bin.mkdir(mode=0o700)
            fake_codex = private_bin / 'codex'
            fake_codex.write_text("#!/usr/bin/python3\nimport sys\n"
                "if sys.argv[1:] == ['--version']:\n print('codex-cli 0.162.1'); raise SystemExit(0)\n"
                "print('NEW_RESUME_FRAME' if sys.argv[1:2] == ['resume'] else 'OLD_RESUME_FRAME')\n")
            fake_codex.chmod(0o700)
            host_path = '/artifacts/bin:/usr/bin'
        host = start_process(['/pigade/host', 'serve', '/artifacts/state'], env={
            'PATH': host_path, 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime),
            'TMPDIR': '/tmp', 'LC_ALL': 'C.utf8'})
        children.append(host)
        wait_until(lambda: Path('/artifacts/state/host.sock').is_socket() or host.poll() is not None,
                   time.monotonic() + 8)
        require(host.poll() is None, 'private host exited before fixture setup')
        session = str(uuid.uuid4())
        generation = ('00000000-0000-4000-8000-000000000002'
                      if config.get('fixture') == 'generations' else str(uuid.uuid4()))
        operation = str(uuid.uuid4())
        spec = {'session': session, 'generation': generation, 'operation': operation,
                'executable': '/usr/bin/python3', 'args': ['-c', LATENCY_FIXTURE if config.get('fixture') == 'latency' else FIXTURE, str(control_path),
                    (LATENCY_KEYS if config.get('fixture') == 'latency' else
                     KEYBOARD_EXPECTED if config.get('fixture') == 'keyboard' else '你好'.encode()).hex()],
                'cwd': '/artifacts', 'geometry': {'rows': 24, 'cols': 80}}
        launch = host_request({'command': 'launch', 'spec': spec})
        require(launch.get('error') is None, 'private fixture launch failed')
        control, _ = listener.accept(); control.settimeout(8); listener.close()
        require(json_line(control) == 'READY', 'fixture did not report readiness')
        def fixture_visible():
            views = host_request({'command': 'poll', 'known': []}).get('views', [])
            return bool(views and 'PIGADE-IME-READY' in view_text(views[0]))
        wait_until(fixture_visible, time.monotonic() + 8)
        report['fixture'] = {'session': session, 'generation': generation, 'ready': True,
                             'tty': 'raw/no-echo', 'ack_marker': FIXTURE_TOKEN}
        if config.get('fixture') in ('context', 'memory', 'generations', 'resume'):
            report['fixture'].update(seed_context_catalog(session, memory_fixture=config.get('fixture') == 'memory'))
        if config.get('fixture') == 'resume':
            prepare_resume_fixture(report)
        report['stage'] = 'pigade-app'
        save()
        app_env = dict(fcitx_env)
        app_env['LP_NUM_THREADS'] = '2'
        app_env['LIBGL_ALWAYS_SOFTWARE'] = 'true'
        app_env['VK_DRIVER_FILES'] = '/pigade/platform-sdk/lavapipe-icd.json'
        app_env['VK_ICD_FILENAMES'] = '/pigade/platform-sdk/lavapipe-icd.json'
        if config.get('fixture') in ('keyboard', 'latency'):
            prepare_private_keyboard(config, app_env)
        app = start_process(app_command(config.get('publication_trace') is True), env=app_env)
        children.append(app)
        boot, app_start = process_identity(app.pid)
        require(boot == boot_id, 'app process boot identity differs')
        config.update({'app_pid': app.pid, 'app_start': app_start})
        wait_until(lambda: window_verified(helper_call(config, 'probe-window', timeout=3)),
                   time.monotonic() + 25)
        helper_call(config, 'focus')
        # First X11/Vulkan initialization may continue after MapNotify. This
        # fixed fixture settle is not a startup-latency measurement.
        time.sleep(5)
        report['app'] = {'pid': app.pid, 'start_time_ticks': app_start,
                         'window_verified_by': ['_NET_WM_PID', 'XRes local client PID'],
                         'command': config['app_command']}
        helper_call(config, 'capture', '/artifacts/baseline.ppm')
        report['app']['baseline_capture'] = 'baseline.ppm'
        report['app']['initial_settle_seconds'] = 5
        save()
        if config.get('fixture') == 'latency':
            run_latency_fixture(config, report, control, app, fcitx, app_env, save)
            return 0
        if config.get('fixture') == 'keyboard':
            run_keyboard_fixture(config, report, control, app, fcitx, app_env, artifacts, save)
            return 0
        if config.get('fixture') == 'context':
            run_context_fixture(config, report, app_env, save)
            return 0
        if config.get('fixture') == 'memory':
            run_memory_fixture(config, report, app_env, save)
            return 0
        if config.get('fixture') == 'generations':
            run_generations_fixture(config, report, app_env, save)
            return 0
        if config.get('fixture') == 'resume':
            run_resume_fixture(config, report, app_env, save)
            return 0
        active_im = select_private_pinyin(config, home, runtime, auth)
        time.sleep(0.25)
        report['fcitx']['active_input_method'] = active_im
        baseline_reply = host_request({'command': 'poll', 'known': []})
        baseline = baseline_reply['views'][0]
        helper_call(config, 'compose')
        time.sleep(0.25)
        control.sendall(b'CHECK\n')
        precommit = json_line(control, timeout=2)
        require(precommit == 'NO_PRECOMMIT_INPUT', 'PTY received bytes before XIM commit')
        precommit_reply = host_request({'command': 'poll', 'known': []})
        after_preedit = precommit_reply['views'][0]
        require(view_text(after_preedit).replace('PIGADE-IME-READY', '') ==
                view_text(baseline).replace('PIGADE-IME-READY', ''),
                'preedit unexpectedly changed host terminal output')
        time.sleep(0.20)
        helper_call(config, 'capture', '/artifacts/preedit.ppm')
        size = (artifacts / 'preedit.ppm').stat().st_size
        require(0 < size <= MAX_CAPTURE_BYTES, 'preedit screenshot exceeds capture bound')
        report['preedit'] = {'capture': 'preedit.ppm', 'capture_bytes': size,
                             'pty_bytes_before_commit': 0, 'host_output_unchanged': True,
                             'capture_scope': 'private Xvfb root window',
                             'visibility': 'captured before commit; requires screenshot review'}
        report['stage'] = 'xim-commit'
        save()
        helper_call(config, 'commit')
        control.sendall(b'COLLECT\n')
        result_line = json_line(control, timeout=5)
        require(result_line.startswith('RESULT:'), 'fixture omitted bounded commit result')
        result = json.loads(result_line[len('RESULT:'):])
        require(result.get('received_bytes') == 6 and result.get('exact_utf8') is True and
                result.get('extra_bytes') == 0 and result.get('exact_once') is True,
                'PTY did not receive exactly one UTF-8 你好 commit')
        def child_ack_visible():
            views = host_request({'command': 'poll', 'known': []}).get('views', [])
            return bool(views and FIXTURE_TOKEN in view_text(views[0]))
        wait_until(child_ack_visible, time.monotonic() + 8)
        time.sleep(0.25)
        helper_call(config, 'capture', '/artifacts/committed.ppm')
        for path in (artifacts / 'preedit.ppm', artifacts / 'committed.ppm'):
            require(path.stat().st_size <= MAX_CAPTURE_BYTES, 'X11 screenshot exceeds byte cap')
        require(app.poll() is None, 'PigADE exited during IME fixture')
        require(fcitx.poll() is None, 'Fcitx exited during IME fixture')
        report['commit'] = {'received_bytes': 6, 'payload_sha256': result['payload_sha256'],
                            'expected_fixture_hash': hashlib.sha256('你好'.encode()).hexdigest(),
                            'exact_once': True, 'extra_bytes': 0, 'child_ack_seen': True}
        report['commit']['capture_settle_seconds'] = 0.25
        report['status'] = IME_PASS
        report['stage'] = 'complete'
        save()
        return 0
    except Exception as error:
        if (report.get('stage') == 'fixed-input-cell-latency' and
                isinstance(locals().get('config'), dict)):
            report['latency_failure_diagnostics'] = capture_latency_failure_diagnostics(
                config, report, artifacts, error)
        report.update({'status': 'failed-closed', 'failure_type': type(error).__name__,
                       'failure_stage': report.get('stage'), 'failure_reason': str(error)[:320]})
        save()
        return 1
    finally:
        for process in reversed(children):
            if process.poll() is None:
                process.terminate()
                try: process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait(timeout=2)
        for thread in LOG_THREADS:
            thread.join(timeout=2)
        report['process_logs'] = sorted(LOG_SUMMARIES, key=lambda row: (row['process'], row['stream_sha256']))
        save()


def outer_run(args) -> dict:
    app = safe_file(args.app, 'PigADE application binary')
    host = safe_file(args.host, 'PigADE session-host binary')
    font_result = load_script(ROOT / 'scripts/isolated-render.py', 'isolated_render_for_font_verification')
    fonts = font_result.verify_embedded_fonts(app, FONT_MANIFEST)
    x11 = validate_sdk(args.x11_sdk, args.x11_sdk / 'evidence/manifest.json',
                       ROOT / 'scripts/prepare-x11-ime-sdk.py')
    platform = validate_platform_sdk(args.platform_sdk)
    helper_output = args.output / 'tools/x11-ime-input'
    helper_output.parent.mkdir(mode=0o700, parents=True)
    helper_evidence = compile_helper(Path(__file__).with_name('x11-ime-input.c'), helper_output)
    args.app, args.host = app, host
    config = inside_config(args, args.output, helper_output, x11, platform)
    private_icd = json.loads((args.platform_sdk / 'lavapipe-icd.json').read_text())
    original_icd_path = private_icd['ICD']['library_path']
    private_icd['ICD']['library_path'] = '/pigade/platform-sdk/root/usr/lib/libvulkan_lvp.so'
    atomic_json(args.output / 'lavapipe-icd.json', private_icd)
    passwd, group = private_identity_files(os.getuid(), os.getgid())
    (args.output / 'passwd').write_text(passwd)
    (args.output / 'group').write_text(group)
    (args.output / 'passwd').chmod(0o600); (args.output / 'group').chmod(0o600)
    config['outer_namespaces'] = {name: os.readlink('/proc/self/ns/' + name) for name in NAMESPACES}
    config['app_path_sha256'] = sha256(app); config['host_path_sha256'] = sha256(host)
    config['icd_source_sha256'] = sha256(args.platform_sdk / 'lavapipe-icd.json')
    config['icd_source_library_path'] = original_icd_path
    config['icd_private_sha256'] = sha256(args.output / 'lavapipe-icd.json')
    config_file = args.output / 'config.json'
    atomic_json(config_file, config)
    command = bwrap_command(args, config_file, helper_output)
    atomic_json(args.output / 'run.json', {
        'schema': 1, 'status': 'running', 'stage': 'bwrap-launch',
        'app_sha256': sha256(app), 'host_sha256': sha256(host),
        'helper_sha256': sha256(helper_output), 'harness_sha256': sha256(Path(__file__)),
        'x11_manifest_sha256': x11['manifest_sha256'],
        'platform_manifest_sha256': platform['manifest_sha256'],
    })
    try:
        exit_status = run_bwrap(command, args.output)
    except subprocess.TimeoutExpired:
        run_result = json.loads((args.output / 'run.json').read_text())
        run_result.update({'status': 'unknown-private-run-timeout',
                           'failure_stage': run_result.get('stage', 'bwrap-launch'),
                           'failure_reason': 'bounded outer timeout; no automatic retry'})
        atomic_json(args.output / 'run.json', run_result)
        raise RuntimeError('isolated run timed out; retained private state and will not retry')
    run_result = json.loads((args.output / 'run.json').read_text()) if (args.output / 'run.json').exists() else {}
    run_result['bwrap_exit_status'] = exit_status
    if run_result.get('status') == 'running':
        run_result.update({'status': 'failed-closed', 'failure_stage': 'bwrap-exit',
                           'failure_reason': 'private run exited without a terminal report'})
    atomic_json(args.output / 'run.json', run_result)
    if exit_status != 0 or run_result.get('status') != fixture_status(args.fixture):
        raise RuntimeError('isolated fixture failed closed; private run.json retains stage metadata')
    return {'status': run_result['status'], 'run': str(args.output / 'run.json'),
            'app_sha256': sha256(app), 'host_sha256': sha256(host),
            'helper': helper_evidence, 'fonts': fonts, 'x11_sdk': x11,
            'platform_sdk': platform, 'captures': [str(args.output / name) for name in
                (['context-editor.ppm'] if args.fixture == 'context' else
                 ['memory-review-initial.ppm', 'memory-review-selected.ppm'] if args.fixture == 'memory' else
                 ['generations-picker.ppm', 'generation-pinned-old.ppm',
                  'generation-follow-head.ppm'] if args.fixture == 'generations' else
                 ['resume-review.ppm', 'resume-new-generation.ppm'] if args.fixture == 'resume' else
                 ['committed.ppm'] if args.fixture in ('keyboard', 'latency') else
                 ['preedit.ppm', 'committed.ppm'])],
            'limits': ['one fixed local X11 keyboard/IME/context/memory-review/generation-history/resume fixture only',
                       'does not certify Wayland, other IMEs/locales, reset barriers, or universal CJK']}


def plan(args) -> dict:
    return {
        'status': 'plan-only-no-process-no-write',
        'run_requires_explicit_flag': '--run',
        'actual_app_only': True, 'fixture': args.fixture,
        'publication_trace': getattr(args, 'publication_trace', False) is True,
        'app': str(args.app), 'host': str(args.host), 'output': str(args.output) if args.output else None,
        'private_sdk': str(args.x11_sdk / 'evidence/manifest.json'),
        'software_renderer_sdk': str(args.platform_sdk / 'manifest.json'),
        'input': {'compose': 'fixed XTEST n,i,h,a,o sequence', 'commit': 'fixed XTEST Space',
                  'keyboard': 'fixed US/RU groups, Ctrl-C/D/Z/R/L/A, Alt-x, Left/Home/Enter',
                  'context': 'fixed private shortcut, Pinyin commit, selection/replace/undo/redo and Ctrl+Enter',
                  'memory': 'fixed Ctrl+Shift+Space, V, Tab, Tab, 2, Ctrl+R; no confirm/launch',
                  'generations': 'fixed Ctrl+Shift+Space, O, 2, Enter; reopen O and type fixed current, Enter',
                  'resume': 'fixed Ctrl+Shift+Space, P, type resume, Enter to review; Return confirms the frozen plan',
                  'latency': 'fixed50 ordered alphabet keys with unique causal flat-cell colors and private pixel readback',
                  'arbitrary_text_argument': False},
        'preflight': ['fresh bwrap user/pid/mount/ipc/network namespaces',
                      'private Xvfb -displayfd and MIT-MAGIC-COOKIE-1',
                      'Unix-socket inode plus SO_PEERCRED exact Xvfb PID',
                      'XTEST extension and XRes local PID ownership for XIM and app window',
                      ('workspace context editing/saving occurs only in the private app and host'
                       if args.fixture == 'context' else
                       'memory review prepares shell-only previews, verifies selected slices, and never confirms launch'
                       if args.fixture == 'memory' else
                       'generation history changes only a per-panel pin; private fixture journals and models are checked unchanged'
                       if args.fixture == 'generations' else
                       'private fake Codex only; resume review is captured before explicit confirmation'
                       if args.fixture == 'resume' else
                       'raw/no-echo PTY receives exactly UTF-8 你好 once after Space'),
                      'host display/session/device paths and inherited desktop variables absent'],
        'limits': ['plan mode does not verify readiness by launching software',
                   'actual X11/Fcitx/app/input remain unrun until separately reviewed --run',
                   ('context editor capture is retained for human review'
                    if args.fixture == 'context' else
                    'initial and selected memory-review captures are retained for human review'
                    if args.fixture == 'memory' else
                    'picker, pinned-old, and followed-head generation captures are retained for human review'
                    if args.fixture == 'generations' else
                    'resume review and new-generation captures are retained for human review'
                    if args.fixture == 'resume' else
                    'preedit capture is retained for human review and is not machine-interpreted')]}


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--run', action='store_true', help='run the private Xvfb/Fcitx/app fixture')
    modes.add_argument('--compile-helper', metavar='PATH', type=Path,
                       help='compile the fixed-sequence XTEST helper only; does not start a display')
    parser.add_argument('--fixture', choices=['pinyin', 'keyboard', 'latency', 'context', 'memory', 'generations', 'resume'], default='pinyin',
                        help='fixed Pinyin, EN/RU, latency, context editor, startup-memory review, generation history, or Codex resume fixture')
    parser.add_argument('--publication-trace', action='store_true',
                        help='enable finite numeric-only app publication tracing')
    parser.add_argument('--app', type=Path, default=DEFAULT_APP)
    parser.add_argument('--host', type=Path, default=DEFAULT_HOST)
    parser.add_argument('--x11-sdk', type=Path, default=X11_SDK)
    parser.add_argument('--platform-sdk', type=Path, default=PLATFORM_SDK)
    parser.add_argument('--output', type=Path)
    return parser.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    try:
        if args.compile_helper:
            print(json.dumps(compile_helper(Path(__file__).with_name('x11-ime-input.c'),
                                            args.compile_helper), indent=2))
            return 0
        if not args.run:
            print(json.dumps(plan(args), indent=2))
            return 0
        require(args.output is not None, '--run requires a new private --output path')
        args.output = private_path(args.output, must_not_exist=True)
        args.output.mkdir(mode=0o700, parents=True)
        require(stat.S_IMODE(args.output.stat().st_mode) == 0o700 and
                args.output.stat().st_uid == os.getuid(), 'private output must be user-owned mode 0700')
        result = outer_run(args)
        atomic_json(args.output / 'result.json', result)
        print(json.dumps(result, indent=2))
        return 0
    except (RuntimeError, OSError, subprocess.SubprocessError, json.JSONDecodeError,
            KeyError, ValueError) as error:
        print(f'Isolated X11 IME harness failed closed: {str(error)[:400]}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    if '--inside' in sys.argv[1:]:
        try:
            sys.exit(inside_run())
        except Exception as error:
            print(f'isolated IME private stage failed closed: {type(error).__name__}: {str(error)[:320]}', file=sys.stderr)
            sys.exit(1)
    raise SystemExit(main())
