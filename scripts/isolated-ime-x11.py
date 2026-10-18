#!/usr/bin/env python3
"""Plan or run one strictly isolated PigADE X11/XIM Pinyin acceptance probe.

Default behavior is plan-only. `--run` uses a fresh bwrap PID/user/network
namespace and private Xvfb/Fcitx/home/DBus/runtime; it never connects to a host
display or accepts arbitrary text for synthetic input.
"""
from __future__ import annotations

import argparse
import fcntl
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
import http.client

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
HISTORY_PASS = 'pass-private-x11-readonly-terminal-history-fixture'
SETUP_PASS = 'pass-private-x11-project-setup-shell-fixture'
OPENCODE_PASS = 'pass-private-x11-opencode-mouse-terminal-fixture'
CONTROLS_PASS = 'pass-private-x11-panel-controls-fixture'
POINTER_PASS = 'pass-private-x11-pointer-layout-fixture'
CATALOG_PASS = 'pass-private-x11-catalog-template-fixture'
RECIPES_PASS = 'pass-private-x11-template-recipes-fixture'
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


def prepare_private_fcitx_preedit_config(home: Path, fixture: str) -> str:
    """Select callback preedit only in the harness-owned Fcitx configuration."""
    config_path = home / '.config/fcitx5/conf/xim.conf'
    if fixture == 'keyboard':
        config_path.unlink(missing_ok=True)
        return 'server-default-external'
    config_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    config_path.write_text('UseOnTheSpot=True\n')
    config_path.chmod(0o600)
    return 'on-the-spot'


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
    expected_state = '0' if config.get('fixture') == 'keyboard' else '1'
    require(result.returncode == 0 and result.stdout.strip() == expected_state,
            'private Fcitx did not confirm expected inactive/no-context state')
    return int(expected_state)


def pane_ids(node):
    if not isinstance(node, dict):
        return []
    pane = node.get('Pane')
    if isinstance(pane, dict) and isinstance(pane.get('id'), str):
        return [pane['id']]
    split = node.get('Split')
    if isinstance(split, dict):
        return [pane_id for child in split.get('children', []) for pane_id in pane_ids(child)]
    return []


def fixture_status(name):
    return {'keyboard': KEYBOARD_PASS, 'pinyin': IME_PASS, 'latency': LATENCY_PASS,
            'context': CONTEXT_PASS, 'memory': MEMORY_PASS,
            'generations': GENERATIONS_PASS, 'resume': RESUME_PASS,
            'history': HISTORY_PASS, 'setup': SETUP_PASS,
            'opencode': OPENCODE_PASS, 'controls': CONTROLS_PASS,
            'pointer': POINTER_PASS, 'catalog': CATALOG_PASS,
            'recipes': RECIPES_PASS}[name]


def seed_context_catalog(session, memory_fixture=False, cwd='/artifacts'):
    catalog = host_request({'command': 'catalog'})['catalog']
    project, workspace, tab = (str(uuid.uuid4()) for _ in range(3))
    selection = dict(catalog['selection'], project_id=project, workspace_id=workspace, tab_id=tab)
    catalog = host_request({'command': 'catalog_apply', 'batch': {
        'projects': [{'expected_revision': None, 'value': {
            'id': project, 'canonical_path': cwd, 'git_common_dir': cwd,
            'name': 'Context fixture', 'roots': [], 'revision': 0}}],
        'workspaces': [{'expected_revision': None, 'value': {
            'id': workspace, 'project_id': project, 'task': 'Context editor fixture',
            'cwd': cwd, 'mode': 'shared_checkout', 'status': 'ready',
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
    # Poll intentionally selects at most four views; generation invariants need all.
    reply = host_request({'command': 'list'})
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
    report['fixture']['baseline_generations'] = baseline_generations
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
    time.sleep(1)
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


def run_history_fixture(config, report, control, env, save):
    report['stage'] = 'terminal-history-viewer'
    save()
    deactivate_private_fcitx(config, env)
    session, generation = report['fixture']['session'], report['fixture']['generation']
    baseline_models = fixture_session_models(session)
    baseline_generations = fixture_generations()
    baseline_layout = host_request({'command': 'catalog'})['catalog']['layout']
    source_hashes = fixture_journal_hashes([generation])
    helper_call(config, 'history-open')
    time.sleep(0.8)
    helper_call(config, 'capture', '/artifacts/history-open.ppm')
    helper_call(config, 'history-readonly')
    time.sleep(0.3)
    helper_call(config, 'capture', '/artifacts/history-readonly.ppm')
    helper_call(config, 'history-search')
    time.sleep(0.8)
    helper_call(config, 'capture', '/artifacts/history-search.ppm')
    control.sendall(b'CHECK\n')
    require(json_line(control, timeout=2) == 'NO_PRECOMMIT_INPUT',
            'history UI sent input to the CLI')
    models = fixture_session_models(session)
    require(set(models) == set(baseline_models) and
            all((models[key]['sequence'], models[key]['frame']) ==
                (baseline_models[key]['sequence'], baseline_models[key]['frame'])
                for key in baseline_models) and
            fixture_generations() == baseline_generations and
            fixture_journal_hashes([generation]) == source_hashes,
            'history viewer changed terminal model, journal or process generations')
    require(host_request({'command': 'catalog'})['catalog']['layout'] == baseline_layout,
            'history viewer changed the layout')
    report['history'] = {'session': session, 'generation': generation,
        'cli_input_absent': True, 'model_and_journal_unchanged': True,
        'new_processes': False,
        'captures': ['history-open.ppm', 'history-search.ppm', 'history-readonly.ppm'],
        'scope': 'fixed private X11 history sheet; visual search/selection need image review'}
    report.update({'status': HISTORY_PASS, 'stage': 'complete'})
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




def run_controls_fixture(config, report, control, app, env, save, tab_id, baseline_generations):
    def step(stage):
        report['stage'] = stage
        save()
        # Host metadata can precede the UI receipt/catalog application.
        # This fixed software-renderer settle is not a latency measurement.
        time.sleep(0.35)
        helper_call(config, stage)

    def catalog_layout():
        catalog = host_request({'command': 'catalog'})['catalog']
        tab = next(item for item in catalog['tabs'] if item['id'] == tab_id)
        return catalog, json.loads(tab['layout_json'])


    def same_live_generations(expected):
        current = fixture_generations()
        if current != expected or process_identities() != expected_processes:
            return False
        return all(fixture_session_models(session).get(generation, {}).get('state') == 'running'
                   for session, generation in expected)

    def process_identities():
        views = host_request({'command': 'list'}).get('views', [])
        require(isinstance(views, list) and len(views) <= 32,
                'controls process identity snapshot exceeds bounds')
        identities = [(view.get('session'), view.get('generation'), view.get('pid'))
                      for view in views]
        require(all(isinstance(session, str) and isinstance(generation, str) and
                    type(pid) is int and pid > 0 for session, generation, pid in identities),
                'controls fixture omitted a live process PID')
        return sorted(identities)

    catalog, initial = catalog_layout()
    original_root = initial.get('root')
    original_ids = set(pane_ids(original_root))
    generations = fixture_generations()
    expected_processes = process_identities()
    require(len(original_ids) == 4 and len(generations) == len(baseline_generations) + 4,
            'controls fixture did not reach exactly four real shell panes')
    require(same_live_generations(generations), 'a terminal was not running before controls tests')

    step('controls-focus-toggle')
    wait_until(lambda: (layout if (layout := catalog_layout()[1]).get('focus_mode') is True else None),
               time.monotonic() + 5)
    step('controls-focus-toggle')
    wait_until(lambda: catalog_layout()[1] == initial, time.monotonic() + 5)
    require(same_live_generations(generations), 'focus-mode toggle changed terminal identities')

    step('controls-focus-right')
    focused_layout = wait_until(
        lambda: (layout if (layout := catalog_layout()[1]).get('focused') != initial.get('focused') else None),
        time.monotonic() + 5)
    require(focused_layout.get('root') == original_root and
            set(pane_ids(focused_layout.get('root'))) == original_ids,
            'Ctrl+Right changed the tiling tree instead of focus only')

    before_move = focused_layout
    old_split_ids = split_ids(before_move.get('root'))
    step('controls-move')
    swapped = wait_until(lambda: (layout if (layout := catalog_layout()[1]).get('root') !=
                                 before_move.get('root') else None), time.monotonic() + 6)
    require(set(pane_ids(swapped.get('root'))) == original_ids and
            split_ids(swapped.get('root')) != old_split_ids and
            same_live_generations(generations),
            'Ctrl+LMB did not reinsert the pane as a Dwindle split or changed terminal identity')

    before_cancel = swapped
    step('controls-resize-cancel')
    wait_until(lambda: catalog_layout()[1] == before_cancel, time.monotonic() + 5)
    step('controls-resize-commit')
    resized = wait_until(lambda: (layout if (layout := catalog_layout()[1]).get('root') !=
                                 before_cancel.get('root') else None), time.monotonic() + 6)
    require(set(pane_ids(resized.get('root'))) == original_ids and
            same_live_generations(generations),
            'Ctrl+RMB resize changed pane/session identity or process generations')

    to_close = resized.get('focused')
    require(to_close in original_ids, 'focused pane is absent before Ctrl+Q')
    step('controls-close')
    closed = wait_until(lambda: (layout if to_close not in set(pane_ids((layout := catalog_layout()[1]).get('root')))
                                 else None), time.monotonic() + 6)
    require(set(pane_ids(closed.get('root'))) == original_ids - {to_close} and
            host_request({'command': 'catalog'})['catalog']['selection'].get('tab_id') == tab_id and
            same_live_generations(generations) and
            fixture_session_models(to_close)[next(generation for session, generation in generations
                                                 if session == to_close)].get('state') == 'running',
            'Ctrl+Q stopped a CLI or failed to remove only its pane placement')

    step('controls-open')
    time.sleep(0.3)
    helper_call(config, 'capture', '/artifacts/controls-settings.ppm')
    report['controls'] = {'initial_pane_ids': sorted(original_ids), 'generations': generations,
                          'focus_mode_restored_exact_layout': True, 'focus_right_only': True,
                          'move_reinserted_as_split': True, 'process_pids_unchanged': True,
                          'resize_escape_restored_exact_layout': True,
                          'resize_commit_preserved_ids': True, 'close_left_cli_running': True,
                          'settings_capture': 'controls-settings.ppm'}
    save()

    config_path = Path('/home/pigade/.config/pigade/controls.json')

    def saved_modifier(name):
        try:
            value = json.loads(config_path.read_text())
            return value.get('version') == 1 and value.get('modifier') == name
        except (OSError, ValueError):
            return False

    step('controls-select-alt')
    wait_until(lambda: saved_modifier('alt'), time.monotonic() + 6)
    time.sleep(0.5)
    require(stat.S_IMODE(config_path.stat().st_mode) == 0o600,
            'private controls preference does not have mode 0600')
    helper_call(config, 'setup-close')
    before_alt_focus = catalog_layout()[1]
    step('controls-alt-focus-toggle')
    wait_until(lambda: catalog_layout()[1].get('focus_mode') is not before_alt_focus.get('focus_mode'),
               time.monotonic() + 5)
    step('controls-alt-focus-toggle')
    wait_until(lambda: catalog_layout()[1] == before_alt_focus, time.monotonic() + 5)
    require(same_live_generations(generations), 'Alt+F changed terminal identities')

    step('controls-open')
    step('controls-select-control')
    wait_until(lambda: saved_modifier('control'), time.monotonic() + 6)
    time.sleep(0.5)
    helper_call(config, 'setup-close')
    step('controls-open')
    step('controls-select-disabled')
    wait_until(lambda: saved_modifier('disabled'), time.monotonic() + 6)
    time.sleep(0.5)
    helper_call(config, 'setup-close')
    disabled_layout = catalog_layout()[1]
    step('controls-close')
    time.sleep(0.3)
    require(catalog_layout()[1] == disabled_layout and same_live_generations(generations),
            'Disabled modifier still intercepted Ctrl+Q or changed live sessions')
    report['controls'].update({'modifier_alt_persisted_before_use': True,
                               'alt_focus_mode_restored_exact_layout': True,
                               'modifier_control_persisted': True,
                               'modifier_disabled_persisted': True,
                               'disabled_mode_ctrl_q_passthrough': True})
    report.update({'status': CONTROLS_PASS, 'stage': 'complete'})
    save()


def split_ids(node):
    if not isinstance(node, dict):
        return set()
    split = node.get('Split')
    if not isinstance(split, dict):
        return set()
    result = {split.get('id')}
    for child in split.get('children', []):
        result |= split_ids(child)
    return result


def direct_move_sibling_split(node, source, target):
    """Find the inserted split that directly pairs the moved pane and target."""
    if not isinstance(node, dict):
        return None
    split = node.get('Split')
    if not isinstance(split, dict):
        return None
    children = split.get('children', [])
    child_ids = [pane_ids(child) for child in children]
    if (len(child_ids) == 2 and child_ids[0] == [target] and child_ids[1] == [source]):
        return split.get('axis'), [target, source]
    for child in children:
        found = direct_move_sibling_split(child, source, target)
        if found:
            return found
    return None


def run_pointer_fixture(config, report, control, app, env, save, tab_id, baseline_generations):
    def catalog_layout():
        catalog = host_request({'command': 'catalog'})['catalog']
        tab = next(item for item in catalog['tabs'] if item['id'] == tab_id)
        return catalog, json.loads(tab['layout_json'])


    def process_snapshot():
        views = host_request({'command': 'list'}).get('views', [])
        require(isinstance(views, list) and len(views) <= 32, 'pointer process snapshot exceeds bounds')
        result = sorted((view.get('session'), view.get('generation'), view.get('pid')) for view in views)
        require(all(isinstance(session, str) and isinstance(generation, str) and
                    type(pid) is int and pid > 0 for session, generation, pid in result),
                'pointer fixture omitted a process identity')
        return result

    def live_unchanged(expected_generations, expected_processes):
        current = fixture_generations()
        if current != expected_generations or process_snapshot() != expected_processes:
            return False
        return all(fixture_session_models(session).get(generation, {}).get('state') == 'running'
                   for session, generation in expected_generations)

    def frame_geometries(panes):
        views = host_request({'command': 'list'}).get('views', [])
        by_key = {(view.get('session'), view.get('generation')): view
                  for view in views if isinstance(view, dict)}
        result = {}
        for session, generation in panes:
            view = by_key.get((session, generation))
            frame = view.get('frame', {}) if view else {}
            geometry = frame.get('geometry') if isinstance(frame, dict) else None
            require(isinstance(geometry, dict), 'pointer fixture host frame omitted geometry')
            result[session] = dict(geometry)
        return result

    def step(stage):
        report['stage'] = stage
        save()
        helper_call(config, stage)

    def held_stage(stage, marker, response, output, verify):
        argv = ['/pigade/x11-ime-input', '--stage', stage,
                '--display', config['display'], '--boot-id', config['boot_id'],
                '--xvfb-pid', str(config['xvfb_pid']), '--xvfb-start', str(config['xvfb_start']),
                '--xvfb-socket-inode', str(config['xvfb_socket_inode']),
                '--fcitx-pid', str(config['fcitx_pid']), '--fcitx-start', str(config['fcitx_start']),
                '--app-pid', str(config['app_pid']), '--app-start', str(config['app_start'])]
        if output:
            argv += ['--output', output]
        process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True, bufsize=1,
                                   env={'PATH': '/usr/bin', 'DISPLAY': config['display'],
                                        'XAUTHORITY': '/run/pigade-test/Xauthority',
                                        'XMODIFIERS': '@im=fcitx', 'LC_ALL': 'C.utf8',
                                        'LD_LIBRARY_PATH': os.environ['LD_LIBRARY_PATH']})
        try:
            ready, _, _ = select.select([process.stdout], [], [], 5)
            require(ready and process.stdout.readline().strip() == marker,
                    f'{stage} did not reach its fixed held state')
            verify()
            process.stdin.write(response + '\n')
            process.stdin.flush()
            remainder, error = process.communicate(timeout=5)
            require(process.returncode == 0 and len(remainder) <= 4096 and len(error) <= 4096,
                    f'{stage} did not release cleanly')
        finally:
            if process.poll() is None:
                try:
                    process.stdin.write(response + '\n')
                    process.stdin.flush()
                    process.communicate(timeout=3)
                except (OSError, subprocess.TimeoutExpired):
                    process.kill()
                    process.communicate(timeout=2)

    _, before = catalog_layout()
    original_root = before.get('root')
    original_ids = set(pane_ids(original_root))
    generations = fixture_generations()
    expected_processes = process_snapshot()
    panes = sorted(pair for pair in generations if pair[0] in original_ids)
    require(len(original_ids) == 4 and len(panes) == 4 and
            len(generations) == len(baseline_generations) + 4,
            'pointer fixture did not reach exactly four real shell panes')
    require(live_unchanged(generations, expected_processes),
            'a pointer fixture terminal was not live before hover tests')

    step('pointer-hover-left')
    time.sleep(0.2)
    left_layout = catalog_layout()[1]
    left_pane = left_layout.get('focused')
    require(left_pane in original_ids, 'hover left did not focus an existing pane')
    step('pointer-hover-right')
    right_layout = wait_until(lambda: (layout if (layout := catalog_layout()[1]).get('focused') != left_pane
                                       else None), time.monotonic() + 5)
    right_pane = right_layout.get('focused')
    require(right_pane in original_ids and right_pane != left_pane,
            'hover right did not follow the cursor to a different pane')
    step('pointer-keyboard-away')
    keyboard_focus = wait_until(lambda: (layout if (layout := catalog_layout()[1]).get('focused') == left_pane
                                        else None), time.monotonic() + 5)
    del keyboard_focus
    time.sleep(0.35)
    require(catalog_layout()[1].get('focused') == left_pane,
            'stationary pointer over right pane overrode keyboard focus')
    step('pointer-move-right-near')
    wait_until(lambda: (layout if (layout := catalog_layout()[1]).get('focused') == right_pane
                        else None), time.monotonic() + 5)
    step('pointer-hover-input')
    focused = wait_until(lambda: (layout if (layout := catalog_layout()[1]).get('focused') == left_pane
                                  else None), time.monotonic() + 5)
    del focused
    text_ready = wait_until(lambda: (views if (views := host_request({'command': 'list'}).get('views', [])) and
                                     any(view.get('session') == left_pane and
                                         'pigadehover' in view_text(view) for view in views) else None),
                            time.monotonic() + 6)
    require(sum('pigadehover' in view_text(view) for view in text_ready
                if view.get('session') in original_ids) == 1 and
            any(view.get('session') == left_pane and 'pigadehover' in view_text(view)
                for view in text_ready),
            'rapid hover A/B/A input did not reach only the last pane')
    require(catalog_layout()[1].get('root') == original_root and
            live_unchanged(generations, expected_processes),
            'hover focus or unsent input changed pane placement/process identity')
    helper_call(config, 'capture', '/artifacts/pointer-hover.ppm')

    _, before_drag = catalog_layout()
    geometries_before = frame_geometries(panes)
    def check_drag_preview():
        require(catalog_layout()[1] == before_drag and
                frame_geometries(panes) == geometries_before and
                live_unchanged(generations, expected_processes),
                'held preview wrote layout metadata, resized PTYs, or changed process identity')
        require(0 < Path('/artifacts/pointer-drag.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
                'held pointer capture is missing or oversized')

    step('pointer-hover-source')
    require(catalog_layout()[1].get('focused') == left_pane,
            'off-center drag start did not select the expected source pane')
    held_stage('pointer-hold-preview', 'HELD', 'CANCEL',
               '/artifacts/pointer-drag.ppm', check_drag_preview)
    wait_until(lambda: catalog_layout()[1].get('root') == before_drag.get('root') and
               frame_geometries(panes) == geometries_before, time.monotonic() + 5)
    require(live_unchanged(generations, expected_processes),
            'cancelled pointer preview changed a terminal or PTY size')
    # Host commit and UI paint are separate receipts; this is capture settling,
    # not an input-to-present latency measurement.
    time.sleep(0.35)
    helper_call(config, 'capture', '/artifacts/pointer-after-cancel.ppm')

    before_selection = catalog_layout()[1]
    def check_selection():
        require(catalog_layout()[1] == before_selection and
                live_unchanged(generations, expected_processes) and
                0 < Path('/artifacts/pointer-selection.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
                'text selection caused hover focus or changed the terminal layout')
    held_stage('pointer-selection-hold', 'SELECTING', 'RELEASE',
               '/artifacts/pointer-selection.ppm', check_selection)
    step('pointer-hover-left')
    time.sleep(0.2)
    ime_name = select_private_pinyin(config, Path(env['HOME']), Path(env['XDG_RUNTIME_DIR']),
                                     Path(env['XAUTHORITY']))
    require(ime_name == 'pinyin', 'private IME did not activate its fixed Pinyin engine')
    # Let Fcitx finish switching the active XIM context before sending the
    # fixed composition sequence; pointer movement happens only after the
    # helper has also allowed the preedit round trip to settle.
    time.sleep(0.25)
    ime_focus = catalog_layout()[1]
    report['stage'] = 'pointer-ime-input'
    save()
    helper_call(config, 'pointer-ime-input', '/artifacts/pointer-ime-preedit.ppm')
    require(0 < Path('/artifacts/pointer-ime-preedit.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'active IME preedit capture is missing or oversized')
    time.sleep(0.3)
    require(catalog_layout()[1] == ime_focus and live_unchanged(generations, expected_processes),
            'active IME preedit allowed hover to change focus or terminal identity')
    ime_views = host_request({'command': 'list'}).get('views', [])
    require(not any('nihao' in view_text(view) for view in ime_views),
            'IME preedit was committed into a terminal while hovering')
    helper_call(config, 'capture', '/artifacts/pointer-ime.ppm')
    control.sendall(b'CHECK\n')
    require(json_line(control, timeout=2) == 'NO_PRECOMMIT_INPUT',
            'IME hover fixture committed bytes into the focused terminal')
    step('pointer-ime-cancel')
    deactivate_private_fcitx(config, env)
    step('controls-open')
    dialog_focus = catalog_layout()[1]
    step('pointer-dialog-hover')
    time.sleep(0.25)
    require(catalog_layout()[1] == dialog_focus and live_unchanged(generations, expected_processes),
            'open controls dialog allowed pointer hover to move pane focus')
    helper_call(config, 'capture', '/artifacts/pointer-dialog.ppm')
    step('setup-close')
    step('pointer-hover-left')
    before_inactive = catalog_layout()[1]
    held_stage('pointer-inactive-hold', 'INACTIVE', 'RESTORE', None,
               lambda: require(catalog_layout()[1] == before_inactive and
                               live_unchanged(generations, expected_processes),
                               'inactive app followed pointer focus'))
    wait_until(lambda: catalog_layout()[1] == before_inactive, time.monotonic() + 5)
    require(live_unchanged(generations, expected_processes),
            'inactive pointer probe changed terminal identities')

    old_split_ids = split_ids(before_drag.get('root'))
    step('pointer-drop')
    dropped = wait_until(lambda: (layout if (layout := catalog_layout()[1]).get('root') !=
                                  before_drag.get('root') else None), time.monotonic() + 6)
    move_siblings = direct_move_sibling_split(dropped.get('root'), left_pane, right_pane)
    require(move_siblings == ('Horizontal', [right_pane, left_pane]) and
            set(pane_ids(dropped.get('root'))) == original_ids and
            split_ids(dropped.get('root')) != old_split_ids and
            live_unchanged(generations, expected_processes),
            'pointer drop did not insert the source to the right of the exact target or changed terminal identities')
    # Host commit and UI paint are separate receipts; this is capture settling,
    # not an input-to-present latency measurement.
    time.sleep(0.35)
    helper_call(config, 'capture', '/artifacts/pointer-dropped.ppm')
    step('pointer-undo')
    restored = wait_until(lambda: (layout if (layout := catalog_layout()[1]) == before_drag else None),
                          time.monotonic() + 8)
    del restored
    require(live_unchanged(generations, expected_processes),
            'pointer layout undo changed CLI/session process identities')
    # Host commit and UI paint are separate receipts; this is capture settling,
    # not an input-to-present latency measurement.
    time.sleep(0.35)
    helper_call(config, 'capture', '/artifacts/pointer-undo.ppm')
    report['pointer'] = {
        'four_pane_ids_preserved': sorted(original_ids),
        'process_generations_and_pids_unchanged': True,
        'hover_left_right_left_followed_without_click': True,
        'stationary_pointer_preserved_keyboard_focus_then_movement_restored_hover_focus': True,
        'unsent_input_reached_only_last_hovered_pane': True,
        'held_drag_preview_did_not_commit_or_resize_ptys': True,
        'escape_cancel_restored_exact_root': True,
        'selection_ime_dialog_and_inactive_focus_suppressed': True,
        'drop_inserted_source_to_right_of_expected_target_as_horizontal_sibling': True,
        'prefix_undo_restored_exact_pre_drag_layout': True,
        'captures': ['pointer-hover.ppm', 'pointer-drag.ppm', 'pointer-after-cancel.ppm',
                     'pointer-selection.ppm', 'pointer-ime-preedit.ppm', 'pointer-ime.ppm', 'pointer-dialog.ppm',
                     'pointer-dropped.ppm', 'pointer-undo.ppm']}
    report.update({'status': POINTER_PASS, 'stage': 'complete'})
    save()


def run_catalog_fixture(config, report, control, app, env, save, tab_id, baseline_generations):
    report['stage'] = 'saved-layout-catalog'
    save()
    deactivate_private_fcitx(config, env)

    def catalog_layout():
        catalog = host_request({'command': 'catalog'})['catalog']
        tab = next(item for item in catalog['tabs'] if item['id'] == tab_id)
        return catalog, json.loads(tab['layout_json'])

    def geometry_only(node):
        if not isinstance(node, dict):
            return None
        if isinstance(node.get('Pane'), dict):
            return 'Pane'
        split = node.get('Split')
        if isinstance(split, dict):
            return {'Split': {'axis': split['axis'], 'weights': split['weights'],
                              'children': [geometry_only(child) for child in split['children']]}}
        raise RuntimeError('catalog fixture encountered an unknown pane node')

    def process_identities():
        views = host_request({'command': 'list'}).get('views', [])
        result = sorted((view.get('session'), view.get('generation'), view.get('pid'))
                        for view in views)
        require(len(result) <= 32 and all(isinstance(session, str) and
                isinstance(generation, str) and type(pid) is int and pid > 0
                for session, generation, pid in result),
                'catalog fixture process identity snapshot is invalid')
        return result

    def pane_labels(catalog):
        labels = catalog.get('pane_labels') or []
        require(isinstance(labels, list) and len(labels) <= 32 and all(
            isinstance(label, dict) and isinstance(label.get('session'), str) and
            isinstance(label.get('name'), str) and type(label.get('revision')) is int and
            label['revision'] >= 0 for label in labels),
            'catalog pane labels are malformed or exceed the fixture bound')
        return labels

    def process_state_snapshot():
        views = host_request({'command': 'list'}).get('views', [])
        require(isinstance(views, list) and len(views) <= 32,
                'catalog stop process snapshot exceeds bounds')
        result = {}
        for view in views:
            session, generation = view.get('session'), view.get('generation')
            pid, state = view.get('pid'), view.get('state')
            require(isinstance(session, str) and isinstance(generation, str) and
                    (pid is None or type(pid) is int and pid > 0) and isinstance(state, str) and
                    (session, generation) not in result,
                    'catalog stop process snapshot has an invalid or duplicate identity')
            result[(session, generation)] = (pid, state)
        return result

    initial, original_layout = catalog_layout()
    original_panes = set(pane_ids(original_layout.get('root')))
    identities = process_identities()
    current_generations = fixture_generations()
    require(len(original_panes) == 4 and len(identities) == 5 and
            len(current_generations) == 5 and
            (report['fixture']['session'], report['fixture']['generation']) in current_generations,
            'catalog fixture did not start with four real panes and the bootstrap shell')

    catalog_key_action(config, 'open-library')
    time.sleep(0.25)
    helper_call(config, 'capture', '/artifacts/catalog-library-empty.ppm')
    catalog_key_action(config, 'open-save-dialog')
    catalog_key_action(config, 'save-template')
    saved_catalog = wait_until(
        lambda: (value if (value := host_request({'command': 'catalog'})['catalog']).get(
            'layout_templates') and any(item.get('name') == 'Catalog'
                                         for item in value['layout_templates']) else None),
        time.monotonic() + 8)
    template = next(item for item in saved_catalog['layout_templates']
                    if item['name'] == 'Catalog')
    require(template['geometry'].get('schema') == 1 and
            template['geometry'].get('root') == geometry_only(original_layout.get('root')),
            'saved catalog geometry differs from current four-pane tree')
    helper_call(config, 'capture', '/artifacts/catalog-library-saved.ppm')
    catalog_key_action(config, 'close-library')

    before_mutation, before_layout = catalog_layout()
    catalog_key_action(config, 'mutate-layout')
    changed = wait_until(lambda: (pair if (pair := catalog_layout())[1] != before_layout else None),
                         time.monotonic() + 8)
    mutated_wire = next(tab for tab in changed[0]['tabs'] if tab['id'] == tab_id)['layout_json']
    require(set(pane_ids(changed[1].get('root'))) == original_panes and
            process_identities() == identities,
            'native geometry edit changed pane membership or process identities')

    catalog_key_action(config, 'open-library')
    catalog_key_action(config, 'apply-first-template')
    applied = wait_until(lambda: (pair if (pair := catalog_layout())[1].get('root') and
                                  geometry_only(pair[1]['root']) == template['geometry'].get('root')
                                  else None), time.monotonic() + 8)
    require(set(pane_ids(applied[1].get('root'))) == original_panes and
            process_identities() == identities,
            'applying a saved geometry changed pane IDs or processes')

    catalog_key_action(config, 'undo-layout')
    undone = wait_until(lambda: (pair if next(
        tab for tab in pair[0]['tabs'] if tab['id'] == tab_id)['layout_json'] == mutated_wire
        else None) if (pair := catalog_layout()) else None, time.monotonic() + 8)
    require(set(pane_ids(undone[1].get('root'))) == original_panes and
            process_identities() == identities,
            'catalog template undo failed exact restoration or changed processes')

    catalog_key_action(config, 'rename-tab')
    renamed = wait_until(
        lambda: (catalog if next(tab for tab in catalog['tabs'] if tab['id'] == tab_id)
                 ['name'] == 'Renamed' else None)
        if (catalog := host_request({'command': 'catalog'})['catalog']) else None,
        time.monotonic() + 8)
    require(set(pane_ids(catalog_layout()[1].get('root'))) == original_panes and
            process_identities() == identities,
            'renaming the tab changed pane membership or process identities')
    catalog_key_action(config, 'open-library')
    time.sleep(0.2)
    reopened = host_request({'command': 'catalog'})['catalog']
    require(any(item.get('id') == template['id'] and item.get('name') == 'Catalog'
                for item in reopened['layout_templates']),
            'saved template did not persist after reopening its UI library')
    helper_call(config, 'capture', '/artifacts/catalog-library-reopened.ppm')
    catalog_key_action(config, 'delete-template')
    wait_until(lambda: value if not (value := host_request({'command': 'catalog'})['catalog']
                                    ).get('layout_templates') else None,
               time.monotonic() + 8)
    require(process_identities() == identities and
            set(pane_ids(catalog_layout()[1].get('root'))) == original_panes,
            'deleting a saved template changed the active layout or processes')
    catalog_key_action(config, 'open-library')
    time.sleep(0.2)
    helper_call(config, 'capture', '/artifacts/catalog-library-deleted.ppm')
    catalog_key_action(config, 'close-library')

    rename_layout = catalog_layout()[1]
    renamed_session = rename_layout.get('focused')
    require(renamed_session in original_panes,
            'catalog fixture has no stable focused pane to rename')
    helper_call(config, 'catalog-pane-rename-open')
    report['stage'] = 'catalog-pane-rename-action'
    save()
    helper_call(config, 'catalog-pane-rename-action')
    time.sleep(.25)
    helper_call(config, 'capture', '/artifacts/catalog-pane-rename-dialog.ppm')
    report['stage'] = 'catalog-pane-rename-input'
    save()
    helper_call(config, 'catalog-pane-rename-type')
    time.sleep(.25)
    helper_call(config, 'capture', '/artifacts/catalog-pane-rename-input.ppm')
    report['stage'] = 'catalog-pane-rename-save'
    save()
    helper_call(config, 'catalog-pane-rename-save')
    renamed_label_catalog = wait_until(
        lambda: (value if any(label.get('session') == renamed_session and
                              label.get('name') == 'Review' and label.get('revision', 0) > 0
                              for label in pane_labels(value)) else None)
        if (value := host_request({'command': 'catalog'})['catalog']) else None,
        time.monotonic() + 8)
    renamed_label = next(label for label in pane_labels(renamed_label_catalog)
                         if label.get('session') == renamed_session)
    require(process_identities() == identities and
            catalog_layout()[1].get('root') == rename_layout.get('root'),
            'renaming a pane changed its layout or process identities')
    time.sleep(.2)
    helper_call(config, 'capture', '/artifacts/catalog-pane-renamed.ppm')

    catalog_key_action(config, 'focus-right')
    other_focus = wait_until(
        lambda: (layout if (layout := catalog_layout()[1]).get('focused') in original_panes and
                 layout.get('focused') != renamed_session else None), time.monotonic() + 5)
    require(other_focus.get('root') == rename_layout.get('root') and
            process_identities() == identities,
            'moving pane focus changed layout or process identities before palette search')
    helper_call(config, 'catalog-pane-palette-open')
    helper_call(config, 'catalog-pane-palette-query')
    time.sleep(.4)
    helper_call(config, 'capture', '/artifacts/catalog-pane-palette.ppm')
    report['stage'] = 'catalog-pane-palette-select'
    save()
    helper_call(config, 'catalog-pane-palette-select')
    report['stage'] = 'catalog-pane-palette-verify'
    save()
    focused_label = wait_until(
        lambda: (layout if (layout := catalog_layout()[1]).get('focused') == renamed_session else None),
        time.monotonic() + 8)
    require(focused_label.get('root') == rename_layout.get('root') and
            process_identities() == identities and
            any(label.get('session') == renamed_session and label.get('name') == 'Review' and
                label.get('revision') == renamed_label['revision']
                for label in pane_labels(host_request({'command': 'catalog'})['catalog'])),
            'palette search did not focus the renamed pane without changing its label or processes')

    stop_tree = json.loads(json.dumps(focused_label.get('root')))
    stop_generations = sorted((session, generation) for session, generation, _ in identities)
    stop_session = renamed_session
    stop_generation = next(generation for session, generation, _ in identities
                           if session == stop_session)
    stop_key = (stop_session, stop_generation)
    stop_state_before = process_state_snapshot()
    require(set(stop_state_before) == set(stop_generations) and
            all(pid is not None and state == 'running' for pid, state in stop_state_before.values()) and
            stop_key in stop_state_before and
            catalog_layout()[1].get('root') == stop_tree,
            'named pane is not an exact running generation before stop review')

    report['stage'] = 'catalog-stop-review'
    save()
    helper_call(config, 'catalog-stop-open')
    helper_call(config, 'catalog-stop-action')
    time.sleep(.25)
    helper_call(config, 'capture', '/artifacts/catalog-stop-review.ppm')
    review_snapshot = process_state_snapshot()
    require(review_snapshot == stop_state_before and
            catalog_layout()[1].get('root') == stop_tree and
            set(pane_ids(catalog_layout()[1].get('root'))) == original_panes,
            'stop confirmation exited or changed a process before explicit confirmation')

    report['stage'] = 'catalog-stop-cancel'
    save()
    helper_call(config, 'catalog-stop-cancel')
    cancel_snapshot = process_state_snapshot()
    require(cancel_snapshot == stop_state_before and
            catalog_layout()[1].get('root') == stop_tree and
            process_identities() == identities,
            'cancelling stop changed the named generation, layout, or process identities')

    report['stage'] = 'catalog-stop-confirm-review'
    save()
    helper_call(config, 'catalog-stop-open')
    helper_call(config, 'catalog-stop-action')
    require(process_state_snapshot() == stop_state_before and
            catalog_layout()[1].get('root') == stop_tree,
            'reopened stop confirmation changed a process or the pane tree')
    report['stage'] = 'catalog-stop-confirm'
    save()
    helper_call(config, 'catalog-stop-confirm')
    report['stage'] = 'catalog-stop-wait-exited'
    save()
    stopped_snapshot = wait_until(
        lambda: (snapshot if (snapshot := process_state_snapshot()).get(stop_key, (None, None))[1]
                 == 'exited' else None), time.monotonic() + 10)
    remaining_keys = set(stop_generations) - {stop_key}
    require(set(stopped_snapshot) == set(stop_generations) and
            all(stopped_snapshot[key] == stop_state_before[key] for key in remaining_keys) and
            catalog_layout()[1].get('root') == stop_tree and
            set(pane_ids(catalog_layout()[1].get('root'))) == original_panes and
            fixture_generations() == stop_generations and
            any(label.get('session') == stop_session and label.get('name') == 'Review' and
                label.get('revision') == renamed_label['revision']
                for label in pane_labels(host_request({'command': 'catalog'})['catalog'])),
            'confirmed stop did not exit only the named generation or changed the saved pane tree')
    time.sleep(.35)
    helper_call(config, 'capture', '/artifacts/catalog-stop-complete.ppm')
    helper_call(config, 'catalog-stop-cancel')

    bootstrap_key = (report['fixture']['session'], report['fixture']['generation'])
    post_review_snapshot = process_state_snapshot()
    current_root = catalog_layout()[1].get('root')
    current_panes = set(pane_ids(current_root))
    process_sessions = {session for session, _generation in stop_generations}
    require(set(post_review_snapshot) == set(stop_generations) and
            post_review_snapshot.get(bootstrap_key, (None, None))[1] == 'running' and
            len(process_sessions) == 5 and current_panes == process_sessions - {bootstrap_key[0]} and
            current_root == stop_tree,
            'hidden-stop fixture did not retain exactly the unplaced live bootstrap generation')

    report['stage'] = 'catalog-hidden-stop-sessions'
    save()
    helper_call(config, 'catalog-sessions-open')
    helper_call(config, 'catalog-sessions-action')
    time.sleep(.25)
    helper_call(config, 'capture', '/artifacts/catalog-hidden-sessions.ppm')
    require(process_state_snapshot() == post_review_snapshot and
            catalog_layout()[1].get('root') == stop_tree,
            'opening Sessions changed a process or the pane tree before stop review')

    report['stage'] = 'catalog-hidden-stop-review'
    save()
    helper_call(config, 'catalog-hidden-stop-review')
    time.sleep(.25)
    helper_call(config, 'capture', '/artifacts/catalog-hidden-stop-review.ppm')
    hidden_review_snapshot = process_state_snapshot()
    require(hidden_review_snapshot == post_review_snapshot and
            catalog_layout()[1].get('root') == stop_tree and
            set(pane_ids(catalog_layout()[1].get('root'))) == current_panes,
            'hidden bootstrap stop confirmation changed a process or pane tree before confirmation')

    report['stage'] = 'catalog-hidden-stop-confirm'
    save()
    helper_call(config, 'catalog-hidden-stop-confirm')
    report['stage'] = 'catalog-hidden-stop-wait-exited'
    save()
    hidden_stopped_snapshot = wait_until(
        lambda: (snapshot if (snapshot := process_state_snapshot()).get(
            bootstrap_key, (None, None))[1] == 'exited' else None), time.monotonic() + 10)
    preserved_after_bootstrap = set(stop_generations) - {bootstrap_key}
    require(set(hidden_stopped_snapshot) == set(stop_generations) and
            all(hidden_stopped_snapshot[key] == post_review_snapshot[key]
                for key in preserved_after_bootstrap) and
            catalog_layout()[1].get('root') == stop_tree and
            set(pane_ids(catalog_layout()[1].get('root'))) == current_panes and
            fixture_generations() == stop_generations,
            'confirmed hidden stop did not exit only the bootstrap generation or changed panes')
    # Hidden generations reconcile through the first bounded readonly probe.
    # Capture settling is not an input-to-present measurement.
    time.sleep(1.25)
    helper_call(config, 'capture', '/artifacts/catalog-hidden-stop-complete.ppm')
    helper_call(config, 'catalog-stop-cancel')

    git_catalog, git_layout = catalog_layout()
    git_selection = git_catalog.get('selection')
    workspace_id = git_selection.get('workspace_id') if isinstance(git_selection, dict) else None
    git_workspace = next((item for item in git_catalog.get('workspaces', [])
                          if item.get('id') == workspace_id), None)
    git_project = next((item for item in git_catalog.get('projects', [])
                        if git_workspace and item.get('id') == git_workspace.get('project_id')), None)
    require(git_workspace is not None and git_project is not None and
            git_workspace.get('status') == 'ready' and
            git_workspace.get('mode') == 'shared_checkout' and
            git_workspace.get('cwd') == git_project.get('canonical_path') and
            git_layout.get('root') == stop_tree,
            'Git fixture has no selected ready shared-checkout workspace')
    git_repo = Path(git_workspace['cwd']).resolve(strict=True)
    private_home = Path('/home/pigade').resolve(strict=True)
    expected_git_repo = (private_home / 'demo').resolve(strict=True)
    require(mount_filesystem_type('/home') == 'tmpfs' and
            private_home == Path('/home/pigade') and git_repo == expected_git_repo and
            git_repo.parent == private_home and git_repo.is_dir(),
            'Git fixture workspace is outside the exact private HOME tmpfs project path')
    git_common_dir = Path(git_project['git_common_dir']).resolve(strict=True)
    require(git_common_dir == (git_repo / '.git').resolve(strict=True),
            'Git fixture project common directory does not match its private workspace')
    git_env = {
        'PATH': '/usr/bin:/bin', 'HOME': '/artifacts/git-fixture-home',
        'LC_ALL': 'C', 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_SYSTEM': '/dev/null',
        'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0',
        'GIT_OPTIONAL_LOCKS': '0',
    }
    Path(git_env['HOME']).mkdir(mode=0o700)

    def private_git(arguments):
        result = subprocess.run(
            ['/usr/bin/git', '-c', 'user.name=PigADE fixture',
             '-c', 'user.email=fixture@localhost', '-c', 'commit.gpgsign=false',
             '-c', 'core.hooksPath=/dev/null', '-C', str(git_repo), *arguments],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=False, timeout=5, check=False, env=git_env)
        require(len(result.stdout) <= 8192 and len(result.stderr) <= 8192,
                'private Git seed command exceeded its output bound')
        require(result.returncode == 0,
                'private Git seed command failed: ' + result.stderr[:512].decode('utf-8', 'replace'))
        return result.stdout

    top_level = private_git(['rev-parse', '--show-toplevel']).decode('utf-8').strip()
    common_dir = private_git(['rev-parse', '--git-common-dir']).decode('utf-8').strip()
    common_path = Path(common_dir)
    if not common_path.is_absolute():
        common_path = git_repo / common_path
    branch = private_git(['branch', '--show-current']).decode('utf-8').strip()
    initial_status = private_git(['status', '--porcelain=v2', '-z', '--untracked-files=all'])
    common_path = Path(common_dir)
    if not common_path.is_absolute():
        common_path = git_repo / common_path
    require(Path(top_level).resolve(strict=True) == git_repo and
            common_path.resolve(strict=True) == git_common_dir and branch == 'main' and
            not any(row and not row.startswith(b'# ') for row in initial_status.split(b'\0')) and
            sorted(item.name for item in git_repo.iterdir()) == ['.git'],
            'private workspace Git repository was not an empty initialized main-branch repo')
    (git_repo / 'tracked.txt').write_text('base revision\n', encoding='utf-8')
    private_git(['add', '--', 'tracked.txt'])
    private_git(['commit', '--quiet', '--no-gpg-sign', '-m', 'private fixture base'])
    (git_repo / 'staged-only.txt').write_text('staged fixture content\n', encoding='utf-8')
    private_git(['add', '--', 'staged-only.txt'])
    (git_repo / 'tracked.txt').write_text('unstaged fixture edit\n', encoding='utf-8')
    untracked_name = 'untracked space\nline.txt'
    untracked_file = git_repo / untracked_name
    untracked_file.write_text('untracked fixture content\n', encoding='utf-8')

    git_process_state = process_state_snapshot()
    git_generation_ids = fixture_generations()
    git_tree = json.loads(json.dumps(git_layout.get('root')))
    git_pane_labels = sorted((item['session'], item['name'], item['revision'])
                             for item in pane_labels(git_catalog))

    def require_git_fixture_invariants(message):
        current_catalog, current_layout = catalog_layout()
        current_labels = sorted((item['session'], item['name'], item['revision'])
                                for item in pane_labels(current_catalog))
        require(process_state_snapshot() == git_process_state and
                fixture_generations() == git_generation_ids and
                current_catalog.get('selection') == git_selection and
                current_layout.get('root') == git_tree and
                current_labels == git_pane_labels,
                message)

    def request_git_snapshot():
        response = host_request({'command': 'workspace_git_snapshot',
                                 'workspace': workspace_id,
                                 'expected_revision': git_workspace['revision']}, timeout=10)
        snapshot = response.get('workspace_git_snapshot')
        require(isinstance(snapshot, dict), 'host omitted the bounded workspace Git snapshot')
        entries = snapshot.get('entries')
        require(snapshot.get('workspace_id') == workspace_id and
                snapshot.get('workspace_revision') == git_workspace['revision'] and
                snapshot.get('project_id') == git_project['id'] and
                Path(snapshot.get('cwd', '')).resolve(strict=True) == git_repo and
                snapshot.get('branch') == 'main' and isinstance(snapshot.get('head'), str) and
                snapshot.get('head') and
                re.fullmatch(r'[0-9a-f]{40,64}', snapshot['head']) is not None and
                snapshot.get('detached') is False and snapshot.get('truncated') is False and
                isinstance(entries, list) and len(entries) <= 128 and all(
                    isinstance(entry, dict) and isinstance(entry.get('path'), str) and
                    len(entry['path'].encode('utf-8')) <= 4096 and
                    entry.get('index_status') in '.MADRCUT?!' and
                    entry.get('worktree_status') in '.MADRCUT?!' and
                    type(entry.get('untracked')) is bool and
                    type(entry.get('unmerged')) is bool for entry in entries),
                'workspace Git snapshot identity or branch metadata is invalid')
        return snapshot

    report['stage'] = 'catalog-git-open'
    save()
    helper_call(config, 'catalog-git-open')
    helper_call(config, 'catalog-git-query')
    time.sleep(.25)
    git_before_refresh = request_git_snapshot()
    git_entries = {entry.get('path'): entry for entry in git_before_refresh.get('entries', [])
                   if isinstance(entry, dict)}
    staged_entry = git_entries.get('staged-only.txt', {})
    modified_entry = git_entries.get('tracked.txt', {})
    untracked_entry = git_entries.get(untracked_name, {})
    require(staged_entry.get('index_status') == 'A' and
            staged_entry.get('worktree_status') == '.' and
            staged_entry.get('untracked') is False and
            modified_entry.get('index_status') == '.' and
            modified_entry.get('worktree_status') == 'M' and
            modified_entry.get('untracked') is False and
            untracked_entry.get('index_status') == '?' and
            untracked_entry.get('worktree_status') == '?' and
            untracked_entry.get('untracked') is True and
            all(entry.get('unmerged') is False for entry in git_entries.values()) and
            len(git_entries) == 3,
            'workspace Git snapshot did not distinguish staged, unstaged, and newline untracked paths')
    require_git_fixture_invariants(
        'opening the workspace Git dialog changed process states, pane aliases, selection, or layout')
    helper_call(config, 'capture', '/artifacts/catalog-git-snapshot.ppm')

    report['stage'] = 'catalog-git-refresh'
    save()
    untracked_file.unlink()
    helper_call(config, 'catalog-git-refresh')
    time.sleep(.25)
    git_after_refresh = request_git_snapshot()
    refreshed_entries = {entry.get('path'): entry for entry in git_after_refresh.get('entries', [])
                         if isinstance(entry, dict)}
    require(git_after_refresh.get('head') == git_before_refresh.get('head') and
            'staged-only.txt' in refreshed_entries and 'tracked.txt' in refreshed_entries and
            untracked_name not in refreshed_entries and len(refreshed_entries) == 2 and
            refreshed_entries['staged-only.txt'].get('index_status') == 'A' and
            refreshed_entries['tracked.txt'].get('worktree_status') == 'M' and
            all(entry.get('unmerged') is False for entry in refreshed_entries.values()),
            'workspace Git refresh did not reflect removal of the private untracked path')
    require_git_fixture_invariants(
        'refreshing workspace Git changed process states, pane aliases, selection, or layout')
    time.sleep(.3)
    helper_call(config, 'capture', '/artifacts/catalog-git-refreshed.ppm')

    report['stage'] = 'catalog-git-close'
    save()
    helper_call(config, 'catalog-git-close')
    require_git_fixture_invariants(
        'closing the workspace Git dialog changed process states, pane aliases, selection, or layout')

    report['catalog'] = {
        'saved_geometry_schema': template['geometry']['schema'],
        'saved_template_survived_ui_reopen': True,
        'persistence_scope': 'layout-library-dialog-reopen-only; app restart not covered',
        'apply_preserved_all_pane_ids': True,
        'undo_restored_exact_pre_apply_layout_wire': True,
        'tab_rename_persisted': renamed is not None,
        'pane_alias_saved_and_found_by_palette': True,
        'palette_focus_preserved_all_process_identities': True,
        'stop_confirmation_cancel_preserved_named_generation': True,
        'confirmed_stop_exited_only_named_generation': True,
        'initial_five_process_identities_unchanged_until_stop': True,
        'remaining_four_process_identities_and_states_unchanged_after_stop': True,
        'hidden_sessions_stop_confirmation_preserved_bootstrap_until_confirm': True,
        'hidden_sessions_stop_exited_only_unplaced_bootstrap': True,
        'other_four_process_states_and_pane_tree_preserved_after_hidden_stop': True,
        'workspace_git_snapshot_covers_staged_unstaged_and_untracked': True,
        'workspace_git_refresh_reflects_private_file_removal': True,
        'workspace_git_ui_preserved_all_five_generation_states_aliases_selection_and_tree': True,
        'workspace_git': {
            'workspace_revision': git_workspace['revision'],
            'branch': git_before_refresh['branch'],
            'head': git_before_refresh['head'],
            'before_refresh': [
                {'path': entry['path'], 'index_status': entry['index_status'],
                 'worktree_status': entry['worktree_status'], 'untracked': entry['untracked'],
                 'unmerged': entry['unmerged']}
                for entry in sorted(git_before_refresh['entries'], key=lambda item: item['path'])],
            'after_refresh': [
                {'path': entry['path'], 'index_status': entry['index_status'],
                 'worktree_status': entry['worktree_status'], 'untracked': entry['untracked'],
                 'unmerged': entry['unmerged']}
                for entry in sorted(git_after_refresh['entries'], key=lambda item: item['path'])],
            'refresh_invoked_by_fixed_keyboard_stage': 'catalog-git-refresh',
        },
        'delete_removed_template': True,
        'captures': ['catalog-library-empty.ppm', 'catalog-library-saved.ppm',
                     'catalog-library-reopened.ppm', 'catalog-library-deleted.ppm',
                     'catalog-pane-rename-dialog.ppm', 'catalog-pane-rename-input.ppm',
                     'catalog-pane-renamed.ppm', 'catalog-pane-palette.ppm',
                     'catalog-stop-review.ppm', 'catalog-stop-complete.ppm',
                     'catalog-sessions-query.ppm', 'catalog-hidden-sessions.ppm',
                     'catalog-hidden-stop-review.ppm', 'catalog-hidden-stop-complete.ppm',
                     'catalog-git-query.ppm', 'catalog-git-snapshot.ppm',
                     'catalog-git-refreshed.ppm'],
    }
    report.update({'status': CATALOG_PASS, 'stage': 'complete'})
    save()



def recipe_key_action(config, action):
    stages = {
        'select-first': 'recipe-select-first',
        'open-editor': 'recipe-open-editor',
        'save-editor': 'recipe-save-editor',
        'new-tab': 'recipe-new-tab',
        'name-tab': 'recipe-name-tab',
        'submit-name': 'recipe-submit-name',
        'confirm-run': 'recipe-confirm-run',
    }
    require(action in stages, 'recipe fixture requested an unknown fixed helper stage')
    helper_call(config, stages[action])


def template_runs_request(before=None):
    return host_request({'command': 'list_template_runs', 'before': before}).get('template_runs')


def template_run_request(run_id):
    result = host_request({'command': 'get_template_run', 'run': run_id}).get('template_run')
    require(isinstance(result, dict) and result.get('id') == run_id,
            'host did not return the requested durable template run')
    return result


def template_run_database_record(run_id):
    connection = sqlite3.connect('file:/artifacts/state/metadata.sqlite?mode=ro', uri=True, timeout=2)
    try:
        rows = connection.execute(
            'SELECT id,workspace_id,tab_id,template_id,name,slots,status,revision '
            'FROM template_runs WHERE id=?', (run_id,)).fetchall()
        require(len(rows) == 1, 'template run database record is missing or duplicated')
        return dict(zip(('id', 'workspace_id', 'tab_id', 'template_id', 'name', 'slots', 'status', 'revision'),
                        rows[0]))
    finally:
        connection.close()


def run_recipes_fixture(config, report, env, save, tab_id):
    report['stage'] = 'template-recipes'
    save()
    deactivate_private_fcitx(config, env)

    def current_catalog():
        return host_request({'command': 'catalog'})['catalog']

    def current_tab_layout(catalog, target_tab):
        tab = next((item for item in catalog['tabs'] if item['id'] == target_tab), None)
        require(tab is not None, 'recipe fixture tab disappeared')
        return json.loads(tab['layout_json'])

    def geometry_only(node):
        if not isinstance(node, dict):
            return None
        if isinstance(node.get('Pane'), dict):
            return 'Pane'
        split = node.get('Split')
        if isinstance(split, dict):
            return {'Split': {'axis': split['axis'], 'weights': split['weights'],
                              'children': [geometry_only(child) for child in split['children']]}}
        raise RuntimeError('recipe fixture encountered an unknown pane node')

    catalog = current_catalog()
    tab = next((item for item in catalog['tabs'] if item['id'] == tab_id), None)
    require(tab is not None and catalog['selection'].get('tab_id') == tab_id,
            'recipe fixture did not start on the four-shell tab')
    original_layout = current_tab_layout(catalog, tab_id)
    original_panes = set(pane_ids(original_layout.get('root')))
    original_generations = fixture_generations()
    original_identities = sorted((view.get('session'), view.get('generation'), view.get('pid'))
                                 for view in host_request({'command': 'list'}).get('views', []))
    require(len(original_panes) == 4 and len(original_generations) == 5 and
            len(original_identities) == 5 and
            (report['fixture']['session'], report['fixture']['generation']) in original_generations,
            'recipe fixture did not begin with four real shells and the unchanged bootstrap generation')
    active_before = template_runs_request(None)
    require(isinstance(active_before, dict) and active_before.get('entries') == [],
            'recipe fixture active-run list was not empty before setup')

    catalog_key_action(config, 'open-library')
    time.sleep(.2)
    catalog_key_action(config, 'open-save-dialog')
    catalog_key_action(config, 'save-template')
    saved_catalog = wait_until(
        lambda: (value if (value := current_catalog()).get('layout_templates') and any(
            item.get('name') == 'Catalog' for item in value['layout_templates']) else None),
        time.monotonic() + 8)
    template = next(item for item in saved_catalog['layout_templates'] if item['name'] == 'Catalog')
    require(template.get('geometry', {}).get('schema') == 1 and
            template['geometry'].get('root') is not None and
            template['geometry']['root'] == geometry_only(original_layout.get('root')),
            'saved geometry template differs from the live four-shell layout')
    time.sleep(.2)
    recipe_key_action(config, 'select-first')
    recipe_key_action(config, 'open-editor')
    time.sleep(.25)
    helper_call(config, 'capture', '/artifacts/recipe-editor.ppm')
    require(fixture_generations() == original_generations and
            sorted((view.get('session'), view.get('generation'), view.get('pid'))
                   for view in host_request({'command': 'list'}).get('views', [])) == original_identities,
            'opening recipe editor changed existing process identities')
    recipe_key_action(config, 'save-editor')
    saved = wait_until(
        lambda: (value if (value := current_catalog()).get('layout_templates') and any(
            item.get('id') == template['id'] and item.get('revision', 0) > template.get('revision', 0)
            and isinstance(item.get('launches'), list) for item in value['layout_templates']) else None),
        time.monotonic() + 8)
    template = next(item for item in saved['layout_templates'] if item['id'] == template['id'])
    require(len(template['launches']) == 4 and all(
        launch.get('provider') == 'shell' and not launch.get('role') and
        not launch.get('instructions') and launch.get('deliver_context') is False
        for launch in template['launches']),
        'recipe editor did not persist four default shell launch settings')
    require(fixture_generations() == original_generations and
            sorted((view.get('session'), view.get('generation'), view.get('pid'))
                   for view in host_request({'command': 'list'}).get('views', [])) == original_identities,
            'saving recipe settings launched a process or changed an existing identity')

    recipe_key_action(config, 'select-first')
    recipe_key_action(config, 'new-tab')
    time.sleep(.2)
    recipe_key_action(config, 'name-tab')
    recipe_key_action(config, 'submit-name')
    time.sleep(.3)
    preview = wait_until(
        lambda: (value if (value := current_catalog()).get('selection', {}).get('workspace_id') else None),
        time.monotonic() + 4)
    workspace_id = preview['selection']['workspace_id']
    # The UI owns creation and confirmation. This read proves there is still no
    # process or tab before the explicit first Enter in the run preview.
    prepared_catalog = current_catalog()
    require(fixture_generations() == original_generations and
            len(prepared_catalog['tabs']) == len(catalog['tabs']) and
            sorted((view.get('session'), view.get('generation'), view.get('pid'))
                   for view in host_request({'command': 'list'}).get('views', [])) == original_identities,
            'pre-confirm recipe UI created a tab or changed existing process identities')
    active_prepared = template_runs_request(None)
    require(isinstance(active_prepared, dict) and len(active_prepared.get('entries', [])) == 1,
            'UI preparation did not create exactly one active template run')
    run_id = active_prepared['entries'][0]['id']
    run = template_run_request(run_id)
    require(run.get('status') == 'prepared' and run.get('can_advance') is True and
            run.get('name') == 'Recipe' and run.get('workspace_id') == workspace_id and
            len(run.get('slots', [])) == 4 and all(slot.get('phase') == 'prepared'
                                                   and slot.get('placed') is False
                                                   and slot.get('preview', {}).get('provider') == 'shell'
                                                   and slot.get('preview', {}).get('can_launch') is True
                                                   for slot in run['slots']),
            'run preview differs from the four-shell saved template')
    helper_call(config, 'capture', '/artifacts/recipe-run-preview.ppm')
    plans_before = prepared_launches_read_only()
    for slot in run['slots']:
        plan_id = slot['preview']['plan_id']
        require(plan_id in plans_before and plans_before[plan_id]['phase'] == 'prepared',
                'recipe preview lacks exactly one durable prepared launch plan per slot')
    require(fixture_generations() == original_generations and
            len(current_catalog()['tabs']) == len(catalog['tabs']) and
            template_run_database_record(run_id)['status'] == 'prepared',
            'pre-confirm run preview changed processes, tabs, or durable run state')

    recipe_key_action(config, 'confirm-run')
    time.sleep(.35)
    helper_call(config, 'capture', '/artifacts/recipe-run-after-confirm.ppm')
    last_revision = run['revision']
    last_started = 0
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        # GetTemplateRun is a coherent durable snapshot. The UI may acknowledge
        # several AdvanceTemplateRun calls between polls, so this observer must
        # not compare its revision delta with independently sampled processes.
        current = template_run_request(run_id)
        revision = current.get('revision')
        require(type(revision) is int and revision >= last_revision,
                'template run revision regressed during confirmation')
        started = sum(slot.get('phase') != 'prepared' for slot in current.get('slots', []))
        require(len(current.get('slots', [])) == 4 and last_started <= started <= 4,
                'template run slot progress regressed or exceeded the saved recipe')
        last_revision = revision
        last_started = started
        if current.get('status') == 'complete':
            run = current
            break
        require(current.get('error') is None,
                'template run reported an error before all explicitly confirmed shell slots were added')
        time.sleep(.04)
    else:
        raise RuntimeError('explicit template run did not complete within the bounded fixture window')

    expected_pairs = sorted((slot['preview']['session'], slot['preview']['generation'])
                            for slot in run['slots'])
    require(len(expected_pairs) == 4 and len(set(expected_pairs)) == 4,
            'completed recipe did not create exactly four unique new session generations')
    require(all(slot.get('phase') == 'not_requested' and slot.get('placed') is True
                and slot.get('preview', {}).get('provider') == 'shell'
                for slot in run['slots']),
            'completed shell slots are not durably confirmed as placed without context delivery')
    final_catalog = current_catalog()
    new_tab = next((item for item in final_catalog['tabs'] if item['id'] == run['tab_id']), None)
    require(new_tab is not None and new_tab.get('name') == 'Recipe' and
            final_catalog['selection'].get('tab_id') == run['tab_id'] and
            final_catalog['selection'].get('workspace_id') == workspace_id,
            'completed recipe did not select the new Recipe tab in its source workspace')
    final_layout = json.loads(new_tab['layout_json'])
    require(geometry_only(final_layout.get('root')) == template['geometry']['root'] and
            set(pane_ids(final_layout.get('root'))) == {slot['preview']['session'] for slot in run['slots']},
            'new tab geometry or process identities differ from the saved template')
    final_identities = sorted((view.get('session'), view.get('generation'), view.get('pid'))
                              for view in host_request({'command': 'list'}).get('views', []))
    original_pairs = {(session, generation) for session, generation, _ in original_identities}
    new_identities = [identity for identity in final_identities
                      if identity[:2] not in original_pairs]
    require(len(final_identities) == 9 and set(original_identities).issubset(set(final_identities)) and
            {(session, generation) for session, generation, _ in final_identities} ==
            set(original_generations + expected_pairs) and
            sorted((session, generation) for session, generation, _ in new_identities) == expected_pairs and
            len(new_identities) == 4 and
            all(type(pid) is int and pid > 0 for _, _, pid in new_identities) and
            len({pid for _, _, pid in final_identities}) == 9,
            'recipe completion changed original identities or created unexpected processes')
    active_after = template_runs_request(None)
    require(isinstance(active_after, dict) and active_after.get('entries') == [],
            'completed recipe remains in the active run list')
    record = template_run_database_record(run_id)
    require(record['status'] == 'complete' and record['slots'] == 4 and record['tab_id'] == run['tab_id'],
            'completed template run database record differs from the host view')
    durable = template_run_request(run_id)
    require(durable.get('status') == 'complete' and durable.get('revision') == record['revision'],
            'host GetTemplateRun differs from the completed SQLite record')
    final_plans = prepared_launches_read_only()
    final_plan_ids = [slot['preview']['plan_id'] for slot in run['slots']]
    require(len(final_plan_ids) == 4 and len(set(final_plan_ids)) == 4 and
            all(slot['preview']['plan_id'] in final_plans and
                final_plans[slot['preview']['plan_id']]['phase'] == 'not_requested' and
                final_plans[slot['preview']['plan_id']]['payload'].get('launch', {}).get('session') ==
                slot['preview']['session'] and
                final_plans[slot['preview']['plan_id']]['payload'].get('launch', {}).get('generation') ==
                slot['preview']['generation'] for slot in run['slots']),
            'completed recipe does not retain one confirmed launch plan per new session')
    wait_until(lambda: (views if len(views := host_request({'command': 'list'}).get('views', [])) == 9 and
                        all(any(view.get('session') == slot['preview']['session'] and
                                view.get('generation') == slot['preview']['generation'] and
                                view.get('state') == 'running' for view in views)
                            for slot in run['slots']) else None), time.monotonic() + 8)
    time.sleep(.35)
    helper_call(config, 'capture', '/artifacts/recipe-final-tab.ppm')
    require(set(pane_ids(current_tab_layout(current_catalog(), tab_id).get('root'))) == original_panes and
            fixture_generations() == sorted(original_generations + expected_pairs),
            'final recipe capture occurred before layout and original sessions settled')
    report['recipes'] = {
        'saved_template_id': template['id'], 'saved_template_revision': template['revision'],
        'run_id': run_id, 'run_revision': record['revision'], 'new_tab_id': run['tab_id'],
        'new_tab_name': new_tab['name'], 'four_unique_new_generations': True,
        'original_five_process_identities_unchanged': True,
        'no_process_or_tab_before_explicit_confirm': True,
        'revision_observer_allows_skipped_acknowledgements': True,
        'saved_geometry_reused': True, 'all_phases_not_requested_and_placed': True,
        'active_run_list_empty_after_completion': True,
        'captures': ['recipe-editor.ppm', 'recipe-run-preview.ppm', 'recipe-final-tab.ppm'],
    }
    report.update({'status': RECIPES_PASS, 'stage': 'complete'})
    save()


def run_setup_fixture(config, report, control, app, env, save):
    report['stage'] = 'project-workspace-shell-setup'
    save()
    deactivate_private_fcitx(config, env)
    baseline_generations = fixture_generations()
    require((report['fixture']['session'], report['fixture']['generation']) in baseline_generations,
            'unplaced bootstrap PTY disappeared before setup')
    helper_call(config, 'capture', '/artifacts/welcome.ppm')
    helper_call(config, 'setup-compact')
    time.sleep(.6)
    helper_call(config, 'capture', '/artifacts/welcome-compact.ppm')
    helper_call(config, 'setup-wide')
    time.sleep(.6)
    helper_call(config, 'setup-open')
    time.sleep(0.2)
    helper_call(config, 'capture', '/artifacts/new-project.ppm')
    helper_call(config, 'setup-create')

    def project_ready():
        catalog = host_request({'command': 'catalog'})['catalog']
        if len(catalog['projects']) != 1 or len(catalog['workspaces']) != 1 or len(catalog['tabs']) != 1:
            return None
        project, workspace, tab = catalog['projects'][0], catalog['workspaces'][0], catalog['tabs'][0]
        selection = catalog['selection']
        expected_project_path = Path('/home/pigade/demo')
        if (project['name'] != 'demo' or Path(project['canonical_path']).name != 'demo' or
                project['canonical_path'] != str(expected_project_path) or
                project['canonical_path'] != workspace['cwd'] or
                project['git_common_dir'] != str(expected_project_path / '.git') or
                workspace['task'] != 'Основной' or workspace['mode'] != 'shared_checkout' or
                workspace['status'] != 'ready' or tab['name'] != 'Development' or
                selection.get('project_id') != project['id'] or
                selection.get('workspace_id') != workspace['id'] or
                selection.get('tab_id') != tab['id']):
            return None
        layout = json.loads(tab['layout_json'])
        if layout not in (None, {'tree': None, 'focused': None},
                          {'root': None, 'focused': None, 'focus_mode': False}):
            return None
        if not Path(project['canonical_path']).is_dir():
            return None
        return catalog

    catalog = wait_until(project_ready, time.monotonic() + 12)
    require(fixture_generations() == baseline_generations,
            'project setup automatically created a terminal generation')
    helper_call(config, 'capture', '/artifacts/workspace-ready.ppm')
    project = catalog['projects'][0]
    workspace = catalog['workspaces'][0]
    tab = catalog['tabs'][0]
    helper_call(config, 'setup-shell')

    def shell_ready():
        generations = fixture_generations()
        added = [pair for pair in generations if pair not in baseline_generations]
        if len(added) != 1 or len(generations) != len(baseline_generations) + 1:
            return None
        session, generation = added[0]
        models = fixture_session_models(session)
        view = models.get(generation)
        if not view or view.get('state') != 'running':
            return None
        preview = host_request({'command': 'get_session_context', 'session': session,
                                'generation': generation}).get('launch_preview')
        if not preview or preview.get('provider') != 'shell' or preview.get('cwd') != project['canonical_path']:
            return None
        return session, generation

    session, generation = wait_until(shell_ready, time.monotonic() + 10)
    current = host_request({'command': 'catalog'})['catalog']
    selected = current['selection']
    require((selected.get('project_id'), selected.get('workspace_id'), selected.get('tab_id')) ==
            (project['id'], workspace['id'], tab['id']),
            'shell launch changed the selected project, workspace, or tab')
    current_tab = next(item for item in current['tabs'] if item['id'] == tab['id'])
    layout = json.loads(current_tab['layout_json'])
    require(layout.get('root') == {'Pane': {'id': session}} and layout.get('focused') == session,
            'shell was not placed as the only focused pane in Development')
    require(fixture_generations() == sorted(baseline_generations + [(session, generation)]),
            'setup created an unexpected terminal generation')
    control.sendall(b'CHECK\n')
    require(json_line(control, timeout=2) == 'NO_PRECOMMIT_INPUT',
            'setup UI sent input to the unplaced bootstrap PTY')
    helper_call(config, 'capture', '/artifacts/first-terminal.ppm')
    tree_before = host_request({'command': 'catalog'})['catalog']['tabs'][0]['layout_json']
    helper_call(config, 'setup-compact')
    time.sleep(.8)
    helper_call(config, 'capture', '/artifacts/workspace-compact.ppm')
    require(host_request({'command': 'catalog'})['catalog']['tabs'][0]['layout_json'] == tree_before,
            'compact resize changed the tiling tree')
    helper_call(config, 'setup-wide')
    time.sleep(.5)
    for index, stage in enumerate(('setup-split-right', 'setup-split-down', 'setup-split-down')):
        report['stage'] = f'four-pane-layout-{index + 1}'
        save()
        if index == 2:
            helper_call(config, 'setup-focus-left')
            wait_until(lambda: json.loads(host_request({'command': 'catalog'})['catalog']['tabs'][0]['layout_json']).get('focused') == session, time.monotonic() + 5)
        count = len(fixture_generations())
        helper_call(config, stage)
        wait_until(lambda: len(fixture_generations()) == count + 1, time.monotonic() + 8)
        time.sleep(.4)
    grid_catalog = host_request({'command': 'catalog'})['catalog']
    focused = json.loads(grid_catalog['tabs'][0]['layout_json'])['focused']
    report['stage'] = 'four-pane-live-input'
    save()
    helper_call(config, 'capture', '/artifacts/four-panels-before-input.ppm')
    if config.get('fixture') == 'controls':
        run_controls_fixture(config, report, control, app, env, save, grid_catalog['tabs'][0]['id'], baseline_generations)
        return
    if config.get('fixture') == 'pointer':
        run_pointer_fixture(config, report, control, app, env, save,
                            grid_catalog['tabs'][0]['id'], baseline_generations)
        return
    if config.get('fixture') == 'catalog':
        run_catalog_fixture(config, report, control, app, env, save,
                            grid_catalog['tabs'][0]['id'], baseline_generations)
        return
    if config.get('fixture') == 'recipes':
        run_recipes_fixture(config, report, env, save, grid_catalog['tabs'][0]['id'])
        return
    helper_call(config, 'setup-live')
    try:
        wait_until(lambda: any(view.get('session') == focused and 'pigadegridlive' in view_text(view) for view in host_request({'command':'list'}).get('views', [])), time.monotonic() + 8)
    except RuntimeError:
        helper_call(config, 'capture', '/artifacts/four-panels-input-error.ppm')
        raise
    time.sleep(.5)
    helper_call(config, 'capture', '/artifacts/four-panels.ppm')
    helper_call(config, 'setup-compact')
    time.sleep(.8)
    helper_call(config, 'capture', '/artifacts/four-panels-compact.ppm')
    require(host_request({'command': 'catalog'})['catalog']['tabs'][0]['layout_json'] == grid_catalog['tabs'][0]['layout_json'],
            'compact four-pane resize rebuilt the tiling tree')
    helper_call(config, 'setup-wide')
    time.sleep(.6)
    before_agent_generations = fixture_generations()
    baseline_plans = prepared_launches_read_only()
    helper_call(config, 'setup-launcher')
    time.sleep(1)
    helper_call(config, 'capture', '/artifacts/agent-picker.ppm')
    helper_call(config, 'setup-codex')
    def codex_started():
        plans = prepared_launches_read_only()
        added = [plan for key, plan in plans.items() if key not in baseline_plans]
        if len(added) != 1: return None
        preview = added[0]['payload']['preview']
        if preview['provider'] != 'codex' or added[0]['phase'] != 'not_requested': return None
        if preview.get('deliver_context') or preview.get('include_memory') or preview.get('role_override') or preview.get('agent_instructions'): return None
        views = fixture_session_models(preview['session'])
        view = views.get(preview['generation'])
        return preview if view and view.get('state') == 'exited' and 'OLD_RESUME_FRAME' in view_text(view) else None
    direct_agent = wait_until(codex_started, time.monotonic() + 10)
    require(fixture_generations() == sorted(before_agent_generations + [(direct_agent['session'], direct_agent['generation'])]),
            'one agent choice created other or multiple generations')
    starts=Path('/artifacts/codex-starts.jsonl').read_text().splitlines()
    require(len(starts) == 1 and json.loads(starts[0]) == [],
            'direct agent received context/prompt arguments or launched more than once')
    time.sleep(.6)
    helper_call(config, 'capture', '/artifacts/agent-direct.ppm')
    report['setup'] = {'project': project['canonical_path'], 'workspace_task': workspace['task'],
        'tab': tab['name'], 'shell_session': session, 'shell_generation': generation,
        'no_automatic_cli': True, 'one_shell_generation': True,
        'bootstrap_pty_received_no_input': True, 'fake_codex_direct_launch_no_context': True, 'four_panes_accept_native_input': True,
        'captures': ['welcome.ppm', 'new-project.ppm', 'workspace-ready.ppm', 'first-terminal.ppm',
                     'agent-picker.ppm', 'agent-direct.ppm', 'welcome-compact.ppm', 'workspace-compact.ppm',
                     'four-panels.ppm', 'four-panels-compact.ppm']}
    report.update({'status': SETUP_PASS, 'stage': 'complete'})
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
    activation_state = deactivate_private_fcitx(config, env)
    report['fcitx']['activation_state'] = activation_state
    require(activation_state == 0 and report['fcitx']['preedit_mode'] == 'server-default-external',
            'keyboard fallback probe unexpectedly created an unsupported external-preedit IC')
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
    activation_state = deactivate_private_fcitx(config, env)
    report['fcitx']['activation_state'] = activation_state
    require(activation_state == 0 and report['fcitx']['preedit_mode'] == 'server-default-external',
            'keyboard fallback probe unexpectedly created an unsupported external-preedit IC')
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
                           'pre_input_bytes': 0,
                           'unsupported_external_preedit_has_no_ic': activation_state == 0}
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


def prepare_opencode_fixture(report, save):
    report['stage'] = 'opencode-real-cli-prelaunch'
    save()
    project = Path('/artifacts/opencode-workspace')
    project.mkdir(mode=0o700)
    before = fixture_generations()
    catalog_ids = seed_opencode_catalog(str(project))
    preview = host_request({'command': 'prepare_launch', 'request': {
        'tab': catalog_ids['tab'], 'provider': 'opencode', 'side': 'Right',
        'target': None, 'role_override': '', 'agent_instructions': '',
        'include_memory': False, 'memory_selection': None, 'deliver_context': False}}).get(
            'launch_preview')
    require(isinstance(preview, dict) and preview.get('provider') == 'opencode' and
            preview.get('cli_version') == '1.19.3' and preview.get('can_launch') is True and
            preview.get('deliver_context') is False,
            'OpenCode 1.19.3 prepared launch preview is unavailable')
    session, generation = preview['session'], preview['generation']
    host_request({'command': 'start_prepared', 'plan_id': preview['plan_id']})
    wait_until(lambda: (models if (models := fixture_session_models(session)).get(
        generation, {}).get('state') == 'running' else None), time.monotonic() + 12)
    require(fixture_generations() == sorted(before + [(session, generation)]),
            'OpenCode launch created an unexpected private PTY generation')
    catalog = host_request({'command': 'catalog'})['catalog']
    tab = next(item for item in catalog['tabs'] if item['id'] == catalog_ids['tab'])
    layout = json.loads(tab['layout_json'])
    require(layout.get('focused') == session,
            'OpenCode PTY was not the focused pane before app startup')
    report['opencode'] = {
        'session': session, 'generation': generation, 'tab': tab['id'],
        'plan_id': preview['plan_id'], 'project': catalog_ids['project'],
        'workspace': catalog_ids['workspace'], 'cwd': str(project),
        'executable': '/usr/bin/opencode', 'version': '1.19.3', 'launch_arguments': [],
        'task_sent': False, 'focused_pane_before_app_start': True,
        'network': 'private user/network namespaces; only loopback routes',
        'home': 'fresh /home/pigade with no provider auth/history files',
    }
    save()


def seed_opencode_catalog(cwd):
    catalog = host_request({'command': 'catalog'})['catalog']
    project, workspace, tab = (str(uuid.uuid4()) for _ in range(3))
    selection = dict(catalog['selection'], project_id=project, workspace_id=workspace, tab_id=tab)
    host_request({'command': 'catalog_apply', 'batch': {
        'projects': [{'expected_revision': None, 'value': {
            'id': project, 'canonical_path': cwd, 'git_common_dir': cwd,
            'name': 'OpenCode resume fixture', 'roots': [], 'revision': 0}}],
        'workspaces': [{'expected_revision': None, 'value': {
            'id': workspace, 'project_id': project, 'task': 'OpenCode exact-session resume fixture',
            'cwd': cwd, 'mode': 'shared_checkout', 'status': 'ready',
            'error': None, 'context_revision': None, 'revision': 0}}],
        'tabs': [{'expected_revision': None, 'value': {
            'id': tab, 'workspace_id': workspace, 'name': 'OpenCode',
            'layout_json': 'null', 'active_pane': None, 'revision': 0}}],
        'selection': {'expected_revision': selection['revision'], 'value': selection},
        'layout': None}})
    return {'project': project, 'workspace': workspace, 'tab': tab}


def run_opencode_fixture(config, report, control, app, env, artifacts, save):
    report['stage'] = 'opencode-fixed-mouse-and-prompt-input'
    save()
    deactivate_private_fcitx(config, env)
    session, generation, tab = (report['opencode'][key]
                                for key in ('session', 'generation', 'tab'))
    catalog = host_request({'command': 'catalog'})['catalog']
    selected = json.loads(next(item for item in catalog['tabs'] if item['id'] == tab)['layout_json'])
    require(selected.get('focused') == session,
            'preselected OpenCode PTY is not the focused pane before native input')

    def logo_ready():
        current = fixture_session_models(session).get(generation)
        if not current or current.get('state') != 'running':
            return None
        text = view_text(current)
        return (current if text.count('█') >= 8 and ('▀' in text or '▄' in text) and
                '1.19.3' in text else None)

    view = wait_until(logo_ready, time.monotonic() + 15)
    helper_call(config, 'capture', '/artifacts/opencode-logo-before.ppm')
    require(0 < (artifacts / 'opencode-logo-before.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'OpenCode logo capture exceeds bound')
    report['opencode'].update({'state': view['state'], 'logo_block_cells_seen': True,
                               'version': '1.19.3', 'version_observed_in_live_tui_cells': True,
                               'capture_before': 'opencode-logo-before.ppm'})
    save()

    control.sendall(b'CHECK\n')
    require(json_line(control, timeout=2) == 'NO_PRECOMMIT_INPUT',
            'bootstrap PTY received input before fixed OpenCode interaction')
    helper_call(config, 'opencode-input')
    entered = wait_until(lambda: (models if (models := fixture_session_models(session)).get(
        generation) and 'pigadeinputworks' in view_text(models[generation]) else None),
        time.monotonic() + 10)
    require(entered[generation].get('state') == 'running',
            'OpenCode PTY exited after the unsent prompt input')
    current_catalog = host_request({'command': 'catalog'})['catalog']
    focused = json.loads(next(item for item in current_catalog['tabs']
                              if item['id'] == tab)['layout_json']).get('focused')
    require(focused == session,
            'terminal focus moved away from the exact OpenCode pane during input')
    models = fixture_session_models(session)
    require(models[generation].get('state') == 'running' and
            'pigadeinputworks' in view_text(models[generation]),
            'OpenCode terminal cells do not contain the full unsent prompt')
    # Model publication precedes UI polling/painting. This capture settle is
    # visual review support, not an input-to-present latency measurement.
    time.sleep(0.35)
    helper_call(config, 'capture', '/artifacts/opencode-input-after.ppm')
    require(0 < (artifacts / 'opencode-input-after.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
            'OpenCode input capture exceeds bound')
    require(app.poll() is None, 'PigADE exited during OpenCode mouse/input fixture')
    control.sendall(b'CHECK\n')
    require(json_line(control, timeout=2) == 'NO_PRECOMMIT_INPUT',
            'bootstrap PTY received input instead of the selected OpenCode PTY')
    report['opencode'].update({
        'mouse_motion_burst': 'fixed 128 private XTEST moves across terminal positions; no clicks',
        'unsent_prompt': 'pigadeinputworks', 'full_prompt_in_host_cells': True,
        'focused_pane_unchanged': True, 'app_alive': True,
        'capture_after': 'opencode-input-after.ppm',
        'queue_full_footer': 'retained screenshot for visual inspection; no UI text export available',
        'bootstrap_pty_received_no_input': True,
    })
    run_opencode_resume_fixture(config, report, app, env, artifacts, save)
    report.update({'status': OPENCODE_PASS, 'stage': 'complete'})
    save()


def opencode_api(port, method, path, payload=None, timeout=2):
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=timeout)
    body = None if payload is None else json.dumps(payload, separators=(',', ':')).encode()
    headers = {'Accept': 'application/json'}
    if body is not None:
        headers['Content-Type'] = 'application/json'
    try:
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        data = response.read(1024 * 1024 + 1)
        require(len(data) <= 1024 * 1024, 'OpenCode public API response exceeds bound')
        require(200 <= response.status < 300,
                f'OpenCode public API {method} {path} returned HTTP {response.status}')
        require(data, 'OpenCode public API returned an empty response')
        value = json.loads(data)
        require(isinstance(value, (dict, list)), 'OpenCode public API JSON has invalid shape')
        return value
    finally:
        connection.close()


def opencode_sessions(port):
    value = opencode_api(port, 'GET', '/session')
    require(isinstance(value, list) and len(value) <= 64,
            'OpenCode public session list exceeds fixture bound')
    entries = {}
    for item in value:
        require(isinstance(item, dict), 'OpenCode public session entry has invalid shape')
        session_id = item.get('id')
        require(isinstance(session_id, str) and re.fullmatch(r'ses_[A-Za-z0-9_-]{1,252}', session_id),
                'OpenCode public session list contains an invalid ID')
        require(session_id not in entries, 'OpenCode public session list contains duplicate IDs')
        entries[session_id] = item
    return entries


def opencode_messages(port, session_id):
    require(re.fullmatch(r'ses_[A-Za-z0-9_-]{1,252}', session_id) is not None,
            'OpenCode public message query has an invalid session ID')
    value = opencode_api(port, 'GET', f'/session/{session_id}/message')
    require(isinstance(value, list) and len(value) <= 32,
            'OpenCode public message list exceeds fixture bound')
    return value


def run_owned_opencode_command(action, target, cwd, env, *, stdout_limit=64 * 1024, timeout=6):
    require(action in ('export', 'import'), 'OpenCode fixture command is outside export/import allowlist')
    if action == 'export':
        require(isinstance(target, str) and
                re.fullmatch(r'ses_[A-Za-z0-9_-]{1,252}', target) is not None,
                'OpenCode export target is not the exact fixture session')
    else:
        input_path = Path(target).resolve(strict=True)
        require(input_path.is_file() and input_path.is_relative_to(Path(cwd).resolve(strict=True)) and
                input_path.stat().st_size <= 64 * 1024,
                'OpenCode import source is outside the bounded private workspace file')
        target = str(input_path)
    argv = ['/usr/bin/opencode', action, target]
    process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               close_fds=True, start_new_session=True)
    identity = None
    output = {'stdout': bytearray(), 'stderr': bytearray()}
    streams = {process.stdout.fileno(): ('stdout', process.stdout, stdout_limit),
               process.stderr.fileno(): ('stderr', process.stderr, 8192)}
    try:
        try:
            identity = process_identity(process.pid)
        except FileNotFoundError:
            if process.poll() is None:
                raise
        for fd in streams:
            os.set_blocking(fd, False)
        deadline = time.monotonic() + timeout
        while streams or process.poll() is None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                if process.poll() is None and identity is not None:
                    stop_owned_group(process, identity)
                raise RuntimeError('bounded OpenCode export/import CLI command timed out')
            ready, _, _ = select.select(list(streams), [], [], min(0.1, remaining))
            for fd in ready:
                key, stream, limit = streams[fd]
                remaining_cap = limit + 1 - len(output[key])
                try:
                    chunk = os.read(fd, min(4096, max(1, remaining_cap)))
                except BlockingIOError:
                    continue
                if not chunk:
                    stream.close()
                    del streams[fd]
                    continue
                output[key].extend(chunk)
                if len(output[key]) > limit:
                    if process.poll() is None and identity is not None:
                        stop_owned_group(process, identity)
                    raise RuntimeError(f'OpenCode {key} exceeded the fixture output bound')
        status = process.wait(timeout=1)
        require(status == 0, 'fixed OpenCode export/import command failed')
        return bytes(output['stdout']), bytes(output['stderr'])
    finally:
        for _, stream, _ in streams.values():
            stream.close()
        if process.poll() is None and identity is not None:
            stop_owned_group(process, identity)


def import_synthetic_opencode_exchange(port, session_id, title, project, env):
    require(opencode_messages(port, session_id) == [],
            'new public OpenCode session unexpectedly contains messages')
    exported, export_stderr = run_owned_opencode_command('export', session_id, project, env)
    export_data = json.loads(exported)
    require(isinstance(export_data, dict) and isinstance(export_data.get('info'), dict) and
            export_data['info'].get('id') == session_id and
            export_data['info'].get('title') == title and export_data.get('messages') == [],
            'OpenCode public export is not the expected exact empty-session document')
    exported_info = dict(export_data['info'])
    tag = uuid.uuid4().hex[:16]
    user_message_id = 'msg_fixture_1_user_' + tag
    assistant_message_id = 'msg_fixture_2_assistant_' + tag
    user_part_id = 'prt_fixture_user_' + tag
    assistant_part_id = 'prt_fixture_assistant_' + tag
    user_marker = 'PIGADE_SYNTHETIC_USER_FIXTURE_' + tag
    assistant_marker = 'PIGADE_SYNTHETIC_ASSISTANT_RESPONSE_' + tag
    timestamp = int(time.time() * 1000)
    export_data['messages'] = [
        {'info': {'id': user_message_id, 'sessionID': session_id, 'role': 'user',
                  'time': {'created': timestamp}, 'agent': 'build',
                  'model': {'providerID': 'openai', 'modelID': 'gpt-4o'}},
         'parts': [{'id': user_part_id, 'sessionID': session_id,
                    'messageID': user_message_id, 'type': 'text', 'text': user_marker,
                    'synthetic': True}]},
        {'info': {'id': assistant_message_id, 'sessionID': session_id, 'role': 'assistant',
                  'time': {'created': timestamp + 1, 'completed': timestamp + 2},
                  'parentID': user_message_id, 'modelID': 'gpt-4o', 'providerID': 'openai',
                  'mode': 'build', 'agent': 'build', 'path': {'cwd': project, 'root': project},
                  'cost': 0, 'tokens': {'input': 0, 'output': 0, 'reasoning': 0,
                                        'cache': {'read': 0, 'write': 0}}, 'finish': 'stop'},
         'parts': [{'id': assistant_part_id, 'sessionID': session_id,
                    'messageID': assistant_message_id, 'type': 'text', 'text': assistant_marker,
                    'synthetic': True}]},
    ]
    import_bytes = json.dumps(export_data, separators=(',', ':')).encode()
    require(len(exported) <= 64 * 1024 and len(import_bytes) <= 64 * 1024 and
            dict(export_data['info']) == exported_info,
            'synthetic OpenCode export/import document exceeded bounds or changed outer info')
    import_path = Path(project) / ('.pigade-opencode-import-' + tag + '.json')
    import_fd = os.open(import_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
    try:
        with os.fdopen(import_fd, 'wb') as output:
            output.write(import_bytes)
            output.flush()
            os.fsync(output.fileno())
        import_stdout, import_stderr = run_owned_opencode_command(
            'import', str(import_path), project, env)
    finally:
        import_path.unlink(missing_ok=True)
    require(session_id.encode() in import_stdout and
            len(export_stderr) <= 8192 and len(import_stderr) <= 8192,
            'OpenCode import command omitted the exact synthetic session identity')
    expected_messages = export_data['messages']
    imported = opencode_messages(port, session_id)
    require(len(imported) == 2, 'public OpenCode import did not return exactly two messages')
    expected_by_id = {message['info']['id']: message for message in expected_messages}
    actual_by_id = {message.get('info', {}).get('id'): message for message in imported
                    if isinstance(message, dict) and isinstance(message.get('info'), dict)}
    require(actual_by_id == expected_by_id and set(actual_by_id) == set(expected_by_id),
            'public OpenCode message list differs from the exact two synthetic imported messages')
    return {'user_marker': user_marker, 'assistant_marker': assistant_marker,
            'messages': expected_messages, 'export_bytes': len(exported),
            'import_bytes': len(import_bytes), 'import_file_removed': not import_path.exists()}


def stop_owned_group(process, expected_identity):
    if process.poll() is not None:
        return
    require(process_identity(process.pid) == expected_identity and os.getpgid(process.pid) == process.pid,
            'owned OpenCode fixture process identity changed before cleanup')
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        require(process_identity(process.pid) == expected_identity and os.getpgid(process.pid) == process.pid,
                'owned OpenCode fixture process identity changed before bounded kill')
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=2)


def verify_private_loopback():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as control:
        request = struct.pack('16sH14x', b'lo', 0)
        response = fcntl.ioctl(control.fileno(), 0x8913, request)  # SIOCGIFFLAGS
        flags = struct.unpack('16sH14x', response)[1]
    # bwrap brings up lo before dropping setup privileges. Observe it without
    # trying to reconfigure an interface or granting the fixture capabilities.
    require(flags & 1 and flags & 8, 'private OpenCode loopback is not up/loopback')


def private_browser_processes():
    browser_tokens = ('firefox', 'chromium', 'chrome', 'browser', 'xdg-open')
    found = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            name = (entry / 'comm').read_text().strip().lower()
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        if any(token in name for token in browser_tokens):
            found.append((int(entry.name), name))
    require(len(found) <= 8, 'private browser process observation exceeds bound')
    return sorted(found)


def run_missing_opencode_session(project, env, port):
    missing_id = 'ses_missingfixture20261018'
    before = opencode_sessions(port)
    require(missing_id not in before, 'negative OpenCode session ID unexpectedly exists')
    master, slave = os.openpty()
    process = None
    identity = None
    captured = bytearray()
    try:
        process = subprocess.Popen(['/usr/bin/opencode', '--session', missing_id], cwd=project,
                                   env=env, stdin=slave, stdout=slave, stderr=slave,
                                   close_fds=True, start_new_session=True)
        os.close(slave); slave = -1
        identity = process_identity(process.pid)
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline and process.poll() is None and len(captured) < 8192:
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                try:
                    chunk = os.read(master, min(2048, 8192 - len(captured)))
                except OSError:
                    break
                if not chunk:
                    break
                captured.extend(chunk)
        require(process.poll() is not None,
                'OpenCode missing-session CLI did not fail before its bounded deadline')
        status = process.wait(timeout=1)
        text = bytes(captured).decode('utf-8', errors='replace')
        require(status != 0 and '█' not in text and len(captured) <= 8192,
                'OpenCode missing-session CLI entered TUI or unexpectedly succeeded')
        after = opencode_sessions(port)
        require(set(after) == set(before),
                'failed OpenCode missing-session invocation created a public session')
        return {'missing_session_id': missing_id, 'exit_status': status,
                'failed_before_tui': True, 'public_session_list_unchanged': True,
                'bounded_output_bytes': len(captured), 'tui_block_glyph_absent': True}
    finally:
        if process is not None and process.poll() is None:
            if identity is None:
                identity = process_identity(process.pid)
            stop_owned_group(process, identity)
        os.close(master)
        if slave >= 0:
            os.close(slave)


def run_opencode_resume_fixture(config, report, app, env, artifacts, save):
    report['stage'] = 'opencode-public-session-create-and-bind'
    save()
    session, generation, project = (report['opencode'][key]
                                    for key in ('session', 'generation', 'cwd'))
    baseline_generations = fixture_generations()
    baseline_catalog = host_request({'command': 'catalog'})['catalog']
    baseline_layout = baseline_catalog['layout']
    baseline_identities = sorted((view.get('session'), view.get('generation'), view.get('pid'))
                                 for view in host_request({'command': 'list'}).get('views', []))
    require(len(baseline_identities) <= 32 and
            all(type(pid) is int and pid > 0 for _, _, pid in baseline_identities),
            'OpenCode resume baseline omitted a process identity')
    baseline_plans = prepared_launches_read_only()
    port_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    port_socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0)
    port_socket.bind(('127.0.0.1', 0))
    port = port_socket.getsockname()[1]
    port_socket.close()
    server_env = {'PATH': '/usr/bin', 'HOME': '/home/pigade',
                  'XDG_RUNTIME_DIR': '/run/pigade-test', 'TMPDIR': '/tmp', 'LC_ALL': 'C.utf8'}
    server = start_process(['/usr/bin/opencode', 'serve', '--hostname', '127.0.0.1',
                            '--port', str(port), '--no-mdns'], env=server_env,
                           name='opencode-public-session-server', cwd=project)
    server_identity = process_identity(server.pid)
    try:
        browser_baseline = private_browser_processes()
        require(not browser_baseline, 'private OpenCode fixture unexpectedly has a browser process')
        def api_ready():
            if server.poll() is not None:
                raise RuntimeError('private loopback OpenCode server exited during startup')
            try:
                return opencode_api(port, 'GET', '/global/health', timeout=0.5)
            except (OSError, RuntimeError, json.JSONDecodeError):
                return None
        health = wait_until(api_ready, time.monotonic() + 8, interval=0.1)
        require(isinstance(health, dict), 'OpenCode public health response is invalid')
        baseline_sessions = opencode_sessions(port)
        title = 'PigADE exact-session fixture ' + uuid.uuid4().hex[:12]
        created = opencode_api(port, 'POST', '/session', {'title': title})
        conversation_id = created.get('id') if isinstance(created, dict) else None
        require(isinstance(conversation_id, str) and
                re.fullmatch(r'ses_[A-Za-z0-9_-]{1,252}', conversation_id) and
                conversation_id not in baseline_sessions,
                'OpenCode public API did not create one exact new synthetic session')
        public_sessions = opencode_sessions(port)
        require(set(public_sessions) == set(baseline_sessions) | {conversation_id} and
                public_sessions[conversation_id].get('title') == title,
                'OpenCode public session list differs from the single synthetic addition')
        synthetic_exchange = import_synthetic_opencode_exchange(
            port, conversation_id, title, project, server_env)
        require(fixture_generations() == baseline_generations,
                'creating a public OpenCode session changed terminal generations')
        require(host_request({'command': 'bind_conversation', 'session': session,
                              'generation': generation, 'conversation_id': conversation_id,
                              'expected_revision': 0}).get('launch_preview', {}).get(
                                  'conversation', {}).get('conversation_id') == conversation_id,
                'host did not bind the exact public OpenCode session to its initial generation')
        before_stop = fixture_session_models(session).get(generation)
        require(before_stop and before_stop.get('state') == 'running',
                'bound OpenCode generation stopped before explicit host stop')
        stop = host_request({'command': 'stop', 'session': session, 'generation': generation})
        require(stop.get('error') is None, 'host rejected stop for the exact OpenCode generation')
        stopped_models = wait_until(lambda: (models if (models := fixture_session_models(session)).get(
            generation, {}).get('state') == 'exited' else None), time.monotonic() + 8)
        time.sleep(0.15)
        stopped_models = fixture_session_models(session)
        old_frame = stopped_models[generation].get('frame')
        require(old_frame is not None and fixture_generations() == baseline_generations,
                'stopping OpenCode changed source generation history unexpectedly')
        baseline_catalog = host_request({'command': 'catalog'})['catalog']
        require(baseline_catalog['layout'] == baseline_layout,
                'stopping OpenCode changed its catalog layout')
        plans_before_resume = prepared_launches_read_only()
        prepared = host_request({'command': 'prepare_resume', 'session': session}).get('launch_preview')
        require(isinstance(prepared, dict) and prepared.get('session') == session and
                prepared.get('generation') != generation and prepared.get('can_launch') is True and
                prepared.get('provider') == 'opencode' and prepared.get('cli_version') == '1.19.3' and
                prepared.get('deliver_context') is False and
                (prepared.get('resume') or {}).get('binding', {}).get('conversation_id') == conversation_id,
                'OpenCode exact-ID resume preview differs from its frozen source binding')
        require(fixture_generations() == baseline_generations,
                'PrepareResume created a process before explicit start')
        prepared_after = prepared_launches_read_only()
        added_plan_ids = set(prepared_after) - set(plans_before_resume)
        require(len(added_plan_ids) == 1, 'PrepareResume did not save exactly one frozen plan')
        resume_plan_id = next(iter(added_plan_ids))
        plan = prepared_after[resume_plan_id]
        require(plan['phase'] == 'prepared' and
                plan['payload'].get('launch', {}).get('args') == ['--session', conversation_id] and
                plan['payload'].get('launch', {}).get('env', {}) == {},
                'OpenCode resume plan contains arguments beyond exact --session ID')
        require(set(plans_before_resume) == set(baseline_plans),
                'OpenCode initial launch plan set changed before PrepareResume')
        old_generation_plan = baseline_plans.get(report['opencode']['plan_id'])
        require(old_generation_plan is not None and
                old_generation_plan['payload'].get('launch', {}).get('args') == [] and
                old_generation_plan['payload'].get('launch', {}).get('cwd') == project,
                'OpenCode initial launch plan has unexpected source arguments or cwd')
        require(host_request({'command': 'catalog'})['catalog']['layout'] == baseline_layout,
                'PrepareResume changed catalog placement or layout history')
        time.sleep(0.2)
        helper_call(config, 'capture', '/artifacts/opencode-resume-review.ppm')
        require(0 < (artifacts / 'opencode-resume-review.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
                'OpenCode resume review capture exceeds bound')
        started = host_request({'command': 'start_prepared', 'plan_id': resume_plan_id})
        require(started.get('error') is None, 'StartPrepared rejected the reviewed OpenCode exact-ID plan')
        new_generation = prepared['generation']
        assistant_marker = synthetic_exchange['assistant_marker']
        # `running` acknowledges exec, not completion of the native TUI's
        # asynchronous session/history fetch. Wait for its actual cell receipt.
        resumed_view = wait_until(lambda: (view if (view := fixture_session_models(session)).get(
            new_generation, {}).get('state') == 'running' and assistant_marker in view_text(
                view[new_generation]) else None), time.monotonic() + 12)
        view = resumed_view[new_generation]
        require(fixture_generations() == sorted(baseline_generations + [(session, new_generation)]),
                'OpenCode resume created an extra generation or changed another process')
        final_models = fixture_session_models(session)
        require(set(final_models) == {generation, new_generation} and
                final_models[generation].get('frame') == old_frame,
                'OpenCode resume changed the source frame or created another generation')
        identities_after = sorted((item.get('session'), item.get('generation'), item.get('pid'))
                                  for item in host_request({'command': 'list'}).get('views', []))
        require(len(identities_after) == len(baseline_identities) + 1 and
                set(baseline_identities).issubset(set(identities_after)) and
                identities_after.count((session, new_generation, view.get('pid'))) == 1,
                'OpenCode resume did not add exactly one unique process identity')
        require(host_request({'command': 'catalog'})['catalog']['layout'] == baseline_layout,
                'OpenCode resume changed current layout placement')
        generation_history = fixture_generation_list(session)
        require(generation_history.get('head') == new_generation and
                {item.get('generation') for item in generation_history.get('entries', [])} ==
                {generation, new_generation}, 'OpenCode resume did not retain exact source history')
        pid = view.get('pid')
        cmdline = Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0')
        if cmdline and cmdline[-1] == b'':
            cmdline.pop()
        decoded_cmdline = [item.decode('utf-8', errors='strict') for item in cmdline]
        require(decoded_cmdline == ['/usr/bin/opencode', '--session', conversation_id],
                'resumed OpenCode process argv is not exactly --session plus the bound ID')
        public_after_resume = opencode_sessions(port)
        require(set(public_after_resume) == set(public_sessions) and
                public_after_resume[conversation_id].get('title') == title,
                'OpenCode resume changed public session identity or created another session')
        messages_after_resume = opencode_messages(port, conversation_id)
        require(len(messages_after_resume) == 2,
                'OpenCode resume changed the number of synthetic messages')
        expected_by_id = {message['info']['id']: message
                          for message in synthetic_exchange['messages']}
        actual_by_id = {message.get('info', {}).get('id'): message for message in messages_after_resume
                        if isinstance(message, dict) and isinstance(message.get('info'), dict)}
        require(actual_by_id == expected_by_id and set(actual_by_id) == set(expected_by_id),
                'OpenCode resume changed or duplicated the exact imported synthetic messages')
        require(assistant_marker in view_text(view),
                'resumed OpenCode TUI cells do not show the synthetic imported assistant response')
        title_visible = title in view_text(view)
        require(title_visible or conversation_id in decoded_cmdline,
                'resumed TUI has no exact public session ID/title evidence')
        time.sleep(0.3)
        helper_call(config, 'capture', '/artifacts/opencode-resumed-session.ppm')
        require(0 < (artifacts / 'opencode-resumed-session.ppm').stat().st_size <= MAX_CAPTURE_BYTES,
                'OpenCode resumed session capture exceeds bound')
        negative = run_missing_opencode_session(project, server_env, port)
        require(private_browser_processes() == browser_baseline,
                'OpenCode exact-session or missing-ID flow spawned a browser')
        require(app.poll() is None and server.poll() is None,
                'PigADE or private OpenCode server exited during exact-ID resume fixture')
        final_catalog = host_request({'command': 'catalog'})['catalog']
        require(final_catalog['layout'] == baseline_layout,
                'negative OpenCode lookup changed saved placement or undo history')
        report['opencode'].update({
            'public_server': '127.0.0.1 loopback only', 'public_api_operations': [
                'GET /global/health', 'GET /session', 'POST /session', 'GET /session',
                'GET /session/{id}/message'],
            'public_metadata_directory': '/home/pigade/.local/share/opencode',
            'synthetic_conversation_id': conversation_id, 'synthetic_session_title': title,
            'synthetic_user_message_marker': synthetic_exchange['user_marker'],
            'synthetic_assistant_response_marker': assistant_marker,
            'public_export_initially_empty': True, 'public_import_exact_two_messages': True,
            'synthetic_fixture_not_provider_output': True,
            'export_bytes': synthetic_exchange['export_bytes'],
            'import_bytes': synthetic_exchange['import_bytes'],
            'import_file_removed': synthetic_exchange['import_file_removed'],
            'public_messages_unchanged_after_resume': True,
            'source_plan_id': report['opencode']['plan_id'], 'resume_plan_id': resume_plan_id,
            'bound_source_generation': generation, 'resumed_generation': new_generation,
            'resume_arguments_exact': ['--session', conversation_id],
            'resume_context_or_prompt_arguments': False, 'resume_prepare_created_no_process': True,
            'initial_and_resumed_generations_retained': True, 'source_frame_unchanged': True,
            'public_session_list_unchanged_after_resume': True,
            'resumed_title_visible_in_cells': title_visible,
            'resumed_synthetic_response_visible_in_cells': True,
            'resumed_exact_id_visible_in_process_argv': conversation_id in decoded_cmdline,
            'missing_session_negative': negative,
            'captures': ['opencode-logo-before.ppm', 'opencode-input-after.ppm',
                         'opencode-resume-review.ppm', 'opencode-resumed-session.ppm'],
            'task_sent': False, 'provider_auth_or_private_db_access': False,
            'private_browser_processes': browser_baseline,
            'no_browser_or_host_desktop_access': True,
        })
        save()
    finally:
        stop_owned_group(server, server_identity)


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
        '--tmpfs', '/home', '--dir', '/etc', '--dir', '/pigade', '--dir', '/pigade/platform-sdk',
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
    if not isinstance(value, dict) or value.get('protocol') != 2 or value.get('error') is not None:
        response_protocol = value.get('protocol') if isinstance(value, dict) else None
        response_error = value.get('error') if isinstance(value, dict) else None
        protocol_text = json.dumps(response_protocol, ensure_ascii=True, separators=(',', ':'))
        if len(protocol_text) > 64:
            protocol_text = protocol_text[:61] + '...'
        error_text = json.dumps(response_error, ensure_ascii=True, separators=(',', ':'))
        if len(error_text) > 512:
            error_text = error_text[:509] + '...'
        raise RuntimeError('private host rejected fixture request: protocol=' + protocol_text +
                           ', error=' + error_text)
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


def start_process(argv, *, env=None, pass_fds=(), name=None, stdout=subprocess.PIPE, cwd=None):
    Path('/artifacts/logs').mkdir(mode=0o700, exist_ok=True)
    process_name = name or Path(argv[0]).name
    safe_name = re.sub(r'[^A-Za-z0-9_.-]+', '_', process_name)[:48]
    log_path = Path('/artifacts/logs') / f'{safe_name}-{uuid.uuid4().hex[:8]}.log'
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=stdout,
                               stderr=subprocess.STDOUT if stdout == subprocess.PIPE else subprocess.DEVNULL,
                               close_fds=True, start_new_session=True, env=env, pass_fds=pass_fds,
                               cwd=cwd)
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


def catalog_key_action(config, action):
    """Run one allowlisted sequence through the existing identity-checked helper."""
    stages = {
        'open-library': 'catalog-open-library',
        'close-library': 'catalog-close-library',
        'open-save-dialog': 'catalog-open-save',
        'save-template': 'catalog-save',
        'apply-first-template': 'catalog-apply',
        'undo-layout': 'catalog-undo',
        'delete-template': 'catalog-delete',
        'mutate-layout': 'catalog-mutate',
        'rename-tab': 'catalog-rename-tab',
        'focus-right': 'controls-focus-right',
    }
    require(action in stages, 'catalog fixture requested an unknown fixed helper stage')
    helper_call(config, stages[action])


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


def mount_filesystem_type(mountpoint: str) -> str | None:
    matches = []
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        left, separator, right = line.partition(' - ')
        if not separator:
            continue
        fields = left.split()
        filesystem = right.split()
        if len(fields) > 4 and fields[4] == mountpoint and filesystem:
            matches.append(filesystem[0])
    require(len(matches) <= 1, 'private mountpoint has ambiguous filesystem identity')
    return matches[0] if matches else None


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
        require(mount_filesystem_type('/home') == 'tmpfs',
                'private HOME root is not an isolated tmpfs mount')
        home = Path('/home/pigade'); home.mkdir(mode=0o700)
        require(not (home / 'Projects').exists() and not (home / 'demo').exists(),
                'private HOME project default is not empty at fixture startup')
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
        if config.get('fixture') == 'opencode':
            verify_private_loopback()
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
                               'private_home_tmpfs': True,
                               'private_home_path': '/home/pigade',
                               'project_parent_default': '/home/pigade',
                               'private_home_projects_directory_absent': True,
                               'network_interfaces': sorted(netdev_names),
                               'non_loopback_routes': 0,
                               'private_loopback_enabled_for_fixture': config.get('fixture') == 'opencode'}
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
        preedit_mode = prepare_private_fcitx_preedit_config(home, config.get('fixture', 'pinyin'))
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
                           'xim_selection': '@server=fcitx', 'input_method': 'pinyin',
                           'preedit_mode': preedit_mode}
        report['stage'] = 'host-fixture'
        save()
        control_path = runtime / 'ime-fixture.sock'
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(control_path)); os.chmod(control_path, 0o600); listener.listen(1); listener.settimeout(8)
        host_path = '/usr/bin'
        if config.get('fixture') in ('resume', 'setup'):
            private_bin = Path('/artifacts/bin'); private_bin.mkdir(mode=0o700)
            fake_codex = private_bin / 'codex'
            fake_codex.write_text("#!/usr/bin/python3\nimport sys\n"
                "if sys.argv[1:] == ['--version']:\n print('codex-cli 0.162.1'); raise SystemExit(0)\n"
                "import json\n"
                "with open('/artifacts/codex-starts.jsonl','a') as log: log.write(json.dumps(sys.argv[1:])+'\\n')\n"
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
        if config.get('fixture') in ('pinyin', 'keyboard', 'latency', 'context', 'memory',
                                     'generations', 'resume', 'history'):
            report['fixture'].update(seed_context_catalog(session, memory_fixture=config.get('fixture') == 'memory'))
        if config.get('fixture') == 'resume':
            prepare_resume_fixture(report)
        if config.get('fixture') == 'opencode':
            prepare_opencode_fixture(report, save)
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
        if config.get('fixture') == 'history':
            run_history_fixture(config, report, control, app_env, save)
            return 0
        if config.get('fixture') in ('setup', 'controls', 'pointer', 'catalog', 'recipes'):
            run_setup_fixture(config, report, control, app, app_env, save)
            return 0
        if config.get('fixture') == 'opencode':
            run_opencode_fixture(config, report, control, app, app_env, artifacts, save)
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
                 ['history-open.ppm', 'history-search.ppm', 'history-readonly.ppm'] if args.fixture == 'history' else
                 ['welcome.ppm', 'new-project.ppm', 'workspace-ready.ppm', 'first-terminal.ppm',
                  'agent-picker.ppm', 'agent-direct.ppm', 'welcome-compact.ppm', 'workspace-compact.ppm',
                  'four-panels.ppm', 'four-panels-compact.ppm'] if args.fixture == 'setup' else
                 ['welcome.ppm', 'welcome-compact.ppm', 'new-project.ppm', 'workspace-ready.ppm',
                  'first-terminal.ppm', 'four-panels-before-input.ppm', 'controls-settings.ppm']
                 if args.fixture == 'controls' else
                 ['welcome.ppm', 'welcome-compact.ppm', 'new-project.ppm', 'workspace-ready.ppm',
                  'first-terminal.ppm', 'four-panels-before-input.ppm', 'pointer-hover.ppm',
                  'pointer-drag.ppm', 'pointer-after-cancel.ppm', 'pointer-selection.ppm',
                  'pointer-ime-preedit.ppm', 'pointer-ime.ppm', 'pointer-dialog.ppm', 'pointer-dropped.ppm',
                  'pointer-undo.ppm'] if args.fixture == 'pointer' else
                 ['welcome.ppm', 'new-project.ppm', 'workspace-ready.ppm', 'first-terminal.ppm',
                  'four-panels-before-input.ppm', 'catalog-library-empty.ppm',
                  'catalog-library-saved.ppm', 'catalog-library-reopened.ppm',
                  'catalog-library-deleted.ppm', 'catalog-pane-rename-dialog.ppm',
                  'catalog-pane-rename-input.ppm', 'catalog-pane-renamed.ppm',
                  'catalog-pane-palette.ppm', 'catalog-stop-review.ppm',
                  'catalog-stop-complete.ppm', 'catalog-sessions-query.ppm', 'catalog-hidden-sessions.ppm', 'catalog-hidden-stop-review.ppm',
                  'catalog-hidden-stop-complete.ppm', 'catalog-git-query.ppm',
                  'catalog-git-snapshot.ppm', 'catalog-git-refreshed.ppm'] if args.fixture == 'catalog' else
                 ['welcome.ppm', 'new-project.ppm', 'workspace-ready.ppm', 'first-terminal.ppm',
                  'four-panels-before-input.ppm', 'recipe-editor.ppm', 'recipe-run-preview.ppm',
                  'recipe-final-tab.ppm'] if args.fixture == 'recipes' else
                 ['opencode-logo-before.ppm', 'opencode-input-after.ppm',
                  'opencode-resume-review.ppm', 'opencode-resumed-session.ppm']
                 if args.fixture == 'opencode' else
                 ['committed.ppm'] if args.fixture in ('keyboard', 'latency') else
                 ['preedit.ppm', 'committed.ppm'])],
        'limits': ['one fixed local X11 keyboard/IME/context/memory-review/generation-history/resume/text-history/OpenCode/panel-controls/pointer-layout fixture only',
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
                  'memory': 'fixed Ctrl+Shift+Space, Shift+Enter, Tab, Tab, 2, Ctrl+R; no confirm/launch',
                  'generations': 'fixed Ctrl+Shift+Space, O, 2, Enter; reopen O and type fixed current, Enter',
                  'resume': 'fixed Ctrl+Shift+Space, P, type resume, Enter to review; Return confirms the frozen plan',
                  'history': 'fixed Ctrl+Shift+Space, I; Tab to readonly text, select/copy and attempt edit; Tab to query READY, Enter',
                  'setup': 'fixed private welcome Create project mouse click, type demo, Ctrl+Enter; then toolbar Terminal mouse click',
                  'controls': 'fixed private four-shell setup; Ctrl focus/Dwindle move/resize/close, Escape resize rollback, private modifier selection and Alt+F check',
                  'pointer': 'fixed private four-shell setup; hover follows cursor, last-hover unsent text, held Ctrl+LMB cancel preview, Dwindle drop and layout undo',
                  'catalog': 'fixed private four-shell setup; save/apply/undo/delete geometry, rename/find a pane, review/cancel/confirm its exact-generation stop action, stop hidden bootstrap through Sessions, and open/refresh private workspace Git',
                  'recipes': 'fixed private four-shell setup; save geometry, configure four shell slots, preview without launching, then explicitly confirm and verify four acknowledged launches in a new tab',
                  'latency': 'fixed50 ordered alphabet keys with unique causal flat-cell colors and private pixel readback',
                  'opencode': 'real /usr/bin/opencode 1.19.3; fixed unsent prompt, private loopback session creation, bounded public export/import of a synthetic text-only exchange, exact-ID resume, and missing-ID pre-TUI negative check',
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
                       'history view opens, selects/copies without mutation, and searches READY; no PTY input is allowed'
                       if args.fixture == 'history' else
                       'fresh project becomes shared workspace Основной with Development tab and no automatic CLI; explicit shell action creates one shell'),
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
                    'history open/search/readonly captures are retained for human review'
                    if args.fixture == 'history' else
                    'welcome, project dialog, workspace and first-terminal captures are retained for human review'
                    if args.fixture == 'setup' else
                    'preedit capture is retained for human review and is not machine-interpreted')]}


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--run', action='store_true', help='run the private Xvfb/Fcitx/app fixture')
    modes.add_argument('--compile-helper', metavar='PATH', type=Path,
                       help='compile the fixed-sequence XTEST helper only; does not start a display')
    parser.add_argument('--fixture', choices=['pinyin', 'keyboard', 'latency', 'context', 'memory', 'generations', 'resume', 'history', 'setup', 'opencode', 'controls', 'pointer', 'catalog', 'recipes'], default='pinyin',
                        help='fixed Pinyin, keyboard, latency, context, memory, generation, resume, history, setup, OpenCode, panel controls, or pointer layout fixture')
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
