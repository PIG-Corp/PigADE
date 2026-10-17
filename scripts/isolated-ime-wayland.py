#!/usr/bin/env python3
"""Plan or run one private Sway/Fcitx/PigADE Wayland IME fixture.

Default mode only prints the pinned plan. `--run` is an explicit opt-in and
uses a fresh bubblewrap namespace, private Sway/Wayland/DBus/home/runtime and a
fixed `nihao` + Space input sequence. It never accepts arbitrary test text.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import resource
import select
import secrets
import socket
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
WAYLAND_SDK = Path('/tmp/pigade-wayland-ime-sdk-20261017')
X11_SDK = Path('/tmp/pigade-x11-ime-sdk-20261017')
PLATFORM_SDK = Path('/tmp/pigade-platform-sdk')
FONT_MANIFEST = ROOT / 'crates/app/assets/fonts/derived-terminal-v1/derived-font-manifest.json'
NAMESPACES = ('mnt', 'pid', 'user', 'ipc', 'net')
MAX_IPC = 4 * 1024 * 1024
MAX_TOOL_OUTPUT = 4 * 1024 * 1024
MAX_LOG = 64 * 1024
MAX_PNG = 32 * 1024 * 1024
INPUT_HELPER_DIR = ROOT / 'scripts/wayland-ime-input'
INPUT_HELPER_SOURCE = INPUT_HELPER_DIR / 'ime_input.c'
INPUT_PROTOCOL_XML = INPUT_HELPER_DIR / 'virtual-keyboard-unstable-v1.xml'
INPUT_PROTOCOL_SHA256 = '7ad7870003ecd592cae47dc19d277a609b7f18fd7b7be012623cf3225a7294f5'
WTYPE_LICENSE = INPUT_HELPER_DIR / 'WTYPE-LICENSE.txt'
WTYPE_LICENSE_SHA256 = 'e8bd6bac1b3ba6253625830bceb30c5340f74f4bc898d1bd5450596cbd8224f0'
XKB_REQUIRED_PAYLOADS = (
    'usr/share/xkeyboard-config-2/keycodes/evdev',
    'usr/share/xkeyboard-config-2/symbols/us',
    'usr/share/xkeyboard-config-2/rules/evdev',
)
XKB_CONFIG_ROOT = '/pigade/fcitx-core/usr/share/xkeyboard-config-2'
EXPECTED_TEXT = '你好'.encode('utf-8')
ACK = '__PIGADE_WAYLAND_CHILD_ACK_20261017__'
PASS = 'pass-private-wayland-v3-fcitx-pinyin-commit-fixture'
NAMESPACE_HELLO = b'PIGADE-WAYLAND-NS-ATTEST-V1\n'

FIXTURE = r'''import hashlib, json, os, select, socket, sys, termios, time, tty
control_path=sys.argv[1]; expected=bytes.fromhex(sys.argv[2])
assert expected == "你好".encode("utf-8") and len(expected) == 6
fd=sys.stdin.fileno(); original=termios.tcgetattr(fd)
control=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); control.connect(control_path)
try:
 tty.setraw(fd,termios.TCSANOW); os.write(1,b"PIGADE-WAYLAND-IME-READY\r\n"); control.sendall(b"READY\n")
 while True:
  command=bytearray()
  while not command.endswith(b"\n") and len(command)<=64:
   part=control.recv(1)
   if not part: raise SystemExit(31)
   command.extend(part)
  command=bytes(command)
  if command == b"CHECK\n":
   ready,_,_=select.select([fd],[],[],0.20)
   if ready:
    data=os.read(fd,64); control.sendall(b"EARLY:"+data.hex().encode()+b"\n"); raise SystemExit(32)
   control.sendall(b"NO_PRECOMMIT_INPUT\n")
  elif command == b"COLLECT\n":
   payload=bytearray(); deadline=time.monotonic()+4
   while len(payload)<len(expected) and time.monotonic()<deadline:
    ready,_,_=select.select([fd],[],[],max(0,deadline-time.monotonic()))
    if ready: payload.extend(os.read(fd,len(expected)-len(payload)))
   extra_ready,_,_=select.select([fd],[],[],0.35); extra=os.read(fd,64) if extra_ready else b""
   exact=bytes(payload)==expected and not extra
   control.sendall(("RESULT:"+json.dumps({"received_bytes":len(payload),"payload_sha256":hashlib.sha256(payload).hexdigest(),"synthetic_payload_hex":payload.hex(),"extra_bytes":len(extra),"extra_hex":extra.hex(),"exact_once":exact},sort_keys=True)+"\n").encode())
   os.write(1,("\r\n"+"__PIGADE_WAYLAND_CHILD_ACK_20261017__"+"\r\n").encode() if exact else b"\r\n__PIGADE_WAYLAND_CHILD_REJECT__\r\n")
   break
  else: raise SystemExit(33)
finally:
 termios.tcsetattr(fd,termios.TCSANOW,original); control.close()
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


def private_path(path: Path) -> Path:
    absolute = Path(os.path.abspath(path))
    require(absolute != ROOT and not absolute.is_relative_to(ROOT),
            'output path must be outside the repository')
    for parent in (absolute, *absolute.parents):
        require(not parent.is_symlink(), f'output path contains symlink component: {parent}')
    require(not absolute.exists(), f'output path already exists: {absolute}')
    return absolute


def verified_xkb_config_root(signed_root: Path, inventory: dict) -> str:
    for relative in XKB_REQUIRED_PAYLOADS:
        row = inventory.get(relative)
        path = signed_root / relative
        require(isinstance(row, dict) and row.get('type') == 'file' and
                path.is_file() and not path.is_symlink() and sha256(path) == row.get('sha256'),
                f'signed XKB keymap payload is not verified: {relative}')
    return XKB_CONFIG_ROOT


def process_snapshot(pid: int) -> dict:
    boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    raw = Path(f'/proc/{pid}/stat').read_text()
    rest = raw[raw.rfind(')') + 2:].split()
    require(len(rest) >= 20, 'short /proc process stat')
    return {'pid': pid, 'boot_id': boot, 'state': rest[0], 'ppid': int(rest[1]),
            'start_ticks': int(rest[19]), 'comm': raw[raw.find('(') + 1:raw.rfind(')')]}


def process_identity(pid: int) -> tuple[str, int]:
    snapshot = process_snapshot(pid)
    return snapshot['boot_id'], snapshot['start_ticks']


def bounded_process_argv(pid: int) -> list[bytes]:
    with open(f'/proc/{pid}/cmdline', 'rb') as stream:
        raw = stream.read(4097)
    require(0 < len(raw) <= 4096 and raw.endswith(b'\x00'),
            'private process command line is empty or exceeds bound')
    return raw[:-1].split(b'\x00')


def verify_attested_inside_process(peer_pid: int, peer_uid: int, peer_gid: int,
                                   info_pid: int, info_identity: tuple[str, int],
                                   bwrap_pid: int, bwrap_identity: tuple[str, int],
                                   process: subprocess.Popen, max_depth: int = 8) -> list[dict]:
    require(peer_uid == os.getuid() and peer_gid == os.getgid(),
            'namespace attestation socket peer credentials differ from caller')
    require(os.path.realpath(f'/proc/{peer_pid}/exe') == os.path.realpath('/usr/bin/python3'),
            'namespace attestation peer executable is not the private Python runner')
    require(bounded_process_argv(peer_pid) ==
            [b'/usr/bin/python3', b'/pigade/isolated-ime-wayland.py', b'--inside'],
            'namespace attestation peer argv is not the private runner command')
    root_snapshot = process_snapshot(info_pid)
    require((root_snapshot['boot_id'], root_snapshot['start_ticks']) == info_identity,
            'bwrap info child identity changed before descendant verification')
    require((info_pid == bwrap_pid or root_snapshot['ppid'] == bwrap_pid) and
            process_identity(bwrap_pid) == bwrap_identity and process.poll() is None,
            'bwrap info child is not attached to the owned bwrap process')

    chain = []
    current = peer_pid
    seen = set()
    for _ in range(max_depth + 1):
        require(current not in seen, 'cycle in namespace attestation process ancestry')
        seen.add(current)
        snapshot = process_snapshot(current)
        require(snapshot['state'] not in ('Z', 'X', 'x'),
                'namespace attestation process ancestry contains an exited process')
        chain.append(snapshot)
        if current == info_pid:
            break
        current = snapshot['ppid']
    require(chain[-1]['pid'] == info_pid,
            'namespace attestation peer is not a bounded descendant of bwrap info child')

    for saved in chain:
        current_snapshot = process_snapshot(saved['pid'])
        require(current_snapshot['boot_id'] == saved['boot_id'] and
                current_snapshot['start_ticks'] == saved['start_ticks'] and
                current_snapshot['ppid'] == saved['ppid'] and
                current_snapshot['state'] not in ('Z', 'X', 'x'),
                'namespace attestation process identity changed during verification')
    require(process_identity(info_pid) == info_identity and process.poll() is None,
            'bwrap info child changed during namespace attestation')
    return [{'pid': item['pid'], 'ppid': item['ppid'], 'start_ticks': item['start_ticks'],
             'comm': item['comm']} for item in chain]


def accept_inside_hello(listener: socket.socket, timeout: float = 8) -> tuple[socket.socket, int, int, int]:
    listener.settimeout(timeout)
    connection, _ = listener.accept()
    connection.settimeout(timeout)
    try:
        pid, uid, gid = peer_credentials(connection)
        data = bytearray()
        deadline = time.monotonic() + timeout
        while len(data) < len(NAMESPACE_HELLO):
            remaining = deadline - time.monotonic()
            require(remaining > 0, 'namespace attestation hello exceeded deadline')
            connection.settimeout(remaining)
            chunk = connection.recv(len(NAMESPACE_HELLO) - len(data))
            require(chunk, 'namespace attestation hello ended early')
            data.extend(chunk)
        require(bytes(data) == NAMESPACE_HELLO,
                'namespace attestation hello does not match fixed protocol')
        return connection, pid, uid, gid
    except BaseException:
        connection.close()
        raise


def make_attestation_listener(path: Path) -> tuple[socket.socket, int]:
    require(len(os.fsencode(path)) < 108,
            'private namespace attestation socket path exceeds Unix-domain bound')
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM | socket.SOCK_CLOEXEC)
    try:
        listener.bind(str(path))
        os.chmod(path, 0o600)
        listener.listen(1)
        listener.settimeout(12)
        return listener, path.lstat().st_ino
    except BaseException:
        listener.close()
        raise


def unlink_owned_attestation_socket(path: Path, inode: int) -> None:
    try:
        info = path.lstat()
        if stat.S_ISSOCK(info.st_mode) and info.st_ino == inode:
            path.unlink()
    except FileNotFoundError:
        pass


def namespace_identity_from_fd(fd: int, name: str) -> dict:
    require(name in NAMESPACES, 'unknown namespace type in launch attestation')
    info = os.fstat(fd)
    require(stat.S_ISREG(info.st_mode), f'{name} namespace handle is not an nsfs file')
    target = os.readlink(f'/proc/self/fd/{fd}')
    expected_prefix = name + ':['
    require(target.startswith(expected_prefix) and target.endswith(']'),
            f'{name} descriptor is not the expected namespace handle')
    number = target[len(expected_prefix):-1]
    require(number.isdecimal() and int(number) == info.st_ino,
            f'{name} namespace descriptor inode does not match its kernel identity')
    return {'identity': target, 'device': info.st_dev, 'inode': info.st_ino}


def open_outer_namespace_handles() -> tuple[dict, tuple[int, ...]]:
    handles: list[int] = []
    identities = {}
    try:
        for name in NAMESPACES:
            fd = os.open(f'/proc/self/ns/{name}', os.O_RDONLY | os.O_CLOEXEC)
            handles.append(fd)
            identities[name] = namespace_identity_from_fd(fd, name)
        return identities, tuple(handles)
    except BaseException:
        for fd in handles:
            os.close(fd)
        raise


def inspect_child_namespace_handles(child_pid: int,
                                    child_identity: tuple[str, int]) -> dict:
    before = process_identity(child_pid)
    require(before == child_identity, 'bwrap child PID/start identity changed before namespace proof')
    result = {}
    for name in NAMESPACES:
        fd = os.open(f'/proc/{child_pid}/ns/{name}', os.O_RDONLY | os.O_CLOEXEC)
        try:
            child_item = namespace_identity_from_fd(fd, name)
        finally:
            os.close(fd)
        result[name] = child_item
    require(process_identity(child_pid) == before,
            'bwrap child PID/start identity changed during namespace proof')
    return result


def verify_child_namespace_handles(child_pid: int, child_identity: tuple[str, int],
                                   outer: dict) -> dict:
    result = inspect_child_namespace_handles(child_pid, child_identity)
    verify_child_namespace_separation(result, outer)
    return result


def verify_child_namespace_separation(child_namespaces: dict, outer: dict) -> None:
    for name in NAMESPACES:
        child_item, outer_item = child_namespaces[name], outer[name]
        require((child_item['device'], child_item['inode']) !=
                (outer_item['device'], outer_item['inode']),
                f'bwrap child did not enter a distinct {name} namespace')


def namespace_attestation_digest(attestation: dict) -> str:
    canonical = json.dumps(attestation, sort_keys=True, separators=(',', ':')).encode()
    return hashlib.sha256(canonical).hexdigest()


def close_owned_fd(fd: int) -> int:
    if fd >= 0:
        try:
            os.close(fd)
        except OSError:
            pass
    return -1


def verify_bwrap_namespace_info(info: dict, child_namespaces: dict) -> dict:
    require(type(info.get('child-pid')) is int and info['child-pid'] > 0,
            'bwrap info omitted its child PID')
    keys = {'mnt': 'mnt-namespace', 'pid': 'pid-namespace',
            'user': 'user-namespace', 'ipc': 'ipc-namespace',
            'net': 'net-namespace'}
    observed = {}
    matches = {}
    user_status = 'omitted'
    for name, key in keys.items():
        value = info.get(key)
        if name == 'user':
            # Bubblewrap 0.11.2 excludes userns from NsInfo because its ID is
            # not valid yet when those IDs are gathered; retain but do not
            # make this advisory value part of the kernel isolation gate.
            if type(value) is int and 0 <= value <= 0xFFFFFFFFFFFFFFFF:
                observed[name] = value
                matches[name] = value == child_namespaces[name]['inode']
                user_status = ('omitted-zero' if value == 0 else
                               'matches-kernel' if matches[name] else 'differs-from-kernel')
            elif key not in info:
                observed[name] = None
                matches[name] = None
                user_status = 'omitted'
            elif value is None or isinstance(value, (str, bool, float)):
                observed[name] = value if not isinstance(value, str) else value[:128]
                matches[name] = None
                user_status = 'non-integer-advisory-value'
            else:
                observed[name] = f'<{type(value).__name__}>'
                matches[name] = None
                user_status = 'non-scalar-advisory-value'
            continue
        require(type(value) is int and value > 0,
                f'bwrap info omitted a valid {name} namespace identity')
        observed[name] = value
        matches[name] = value == child_namespaces[name]['inode']
        require(matches[name],
                f'bwrap info {name} namespace differs from kernel-observed child namespace')
    return {'bwrap_info_namespace_ids': observed,
            'kernel_child_namespace_ids': {name: child_namespaces[name]['inode']
                                           for name in NAMESPACES},
            'bwrap_info_matches_kernel': matches,
            'user_namespace_info_status': user_status,
            'user_namespace_info_note': 'bwrap 0.11.2 omits user ID before user namespace setup completes; kernel handle is authoritative'}


def mark_failed_run(report_path: Path, stage: str, error: BaseException) -> None:
    try:
        if report_path.exists():
            report = json.loads(report_path.read_text())
        else:
            report = {'schema': 1}
        if report.get('status') in (None, 'running'):
            report.update({'status': 'failed-closed', 'failure_stage': stage,
                           'failure_reason': f'{type(error).__name__}: {str(error)[:300]}'})
            atomic_json(report_path, report)
    except (OSError, ValueError, json.JSONDecodeError):
        # Keep the original failure as the actionable result if diagnostics
        # themselves cannot be persisted.
        pass


def announce_namespace_gate_peer(path: Path = Path('/artifacts/namespace-attest.sock'),
                                 timeout: float = 3) -> None:
    info = path.lstat()
    require(stat.S_ISSOCK(info.st_mode),
            'parent namespace attestation endpoint is not a Unix socket')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(timeout)
        connection.connect(str(path))
        connection.sendall(NAMESPACE_HELLO)


def await_namespace_gate(config_path: Path = Path('/artifacts/config.json'),
                         timeout: float = 3,
                         attestation_path: Path = Path('/artifacts/namespace-attest.sock')) -> dict:
    fd = sys.stdin.fileno()
    require(stat.S_ISFIFO(os.fstat(fd).st_mode),
            'missing parent namespace attestation pipe; direct --inside is unsupported')
    announce_namespace_gate_peer(attestation_path, timeout)
    deadline = time.monotonic() + timeout

    def read_before_deadline() -> bytes:
        remaining = deadline - time.monotonic()
        require(remaining > 0, 'parent namespace attestation exceeded read deadline')
        ready, _, _ = select.select([fd], [], [], remaining)
        require(ready, 'parent namespace attestation exceeded read deadline')
        return os.read(fd, 1)

    token = bytearray()
    while len(token) <= 129:
        chunk = read_before_deadline()
        require(chunk, 'parent namespace attestation pipe closed before token')
        token.extend(chunk)
        if token.endswith(b'\n'):
            break
    require(len(token) == 130 and token[-1:] == b'\n',
            'parent namespace attestation token has invalid framing')
    require(read_before_deadline() == b'',
            'parent namespace attestation pipe contained trailing data')
    config = json.loads(config_path.read_text())
    nonce, proof_digest = bytes(token[:-1]).split(b':', 1)
    require(len(nonce) == 64 and len(proof_digest) == 64,
            'parent namespace attestation token has invalid fields')
    require(hashlib.sha256(nonce).hexdigest() == config.get('namespace_gate_sha256'),
            'parent namespace attestation token does not match launch config')
    attestation = config.get('namespace_attestation')
    require(isinstance(attestation, dict) and
            namespace_attestation_digest(attestation).encode() == proof_digest,
            'parent namespace attestation does not match verified launch namespaces')
    return config


def peer_credentials(connection: socket.socket) -> tuple[int, int, int]:
    require(hasattr(socket, 'SO_PEERCRED'), 'Linux SO_PEERCRED is unavailable')
    raw = connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize('3i'))
    return struct.unpack('3i', raw)


def verify_private_peer(path: Path, expected: dict, process: subprocess.Popen) -> dict:
    info = path.lstat()
    require(stat.S_ISSOCK(info.st_mode) and info.st_uid == os.getuid(),
            'private server socket is not an owned Unix socket')
    require(info.st_ino == expected['socket_inode'], 'private socket inode changed')
    boot, start = process_identity(process.pid)
    require((boot, start) == (expected['boot_id'], expected['start_ticks']) and process.poll() is None,
            'server PID/start identity changed')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(2)
        connection.connect(str(path))
        pid, uid, gid = peer_credentials(connection)
    require(pid == process.pid and uid == os.getuid() and gid == os.getgid(),
            'private socket SO_PEERCRED does not match owned server process')
    return {'path': str(path), 'inode': info.st_ino, 'peer_pid': pid,
            'peer_uid': uid, 'peer_gid': gid, 'boot_id': boot, 'start_ticks': start}


def wait_until(check, timeout: float, interval: float = 0.05):
    deadline = time.monotonic() + timeout
    last_error = None
    while time.monotonic() < deadline:
        try:
            value = check()
            if value:
                return value
        except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
            last_error = str(error)[:160]
        time.sleep(interval)
    raise RuntimeError('bounded readiness expired' + (': ' + last_error if last_error else ''))


def read_exact(connection: socket.socket, count: int) -> bytes:
    data = bytearray()
    while len(data) < count:
        chunk = connection.recv(count - len(data))
        require(chunk, 'truncated private IPC response')
        data.extend(chunk)
    return bytes(data)


def sway_ipc(connection: socket.socket, message_type: int, payload: bytes = b'') -> object:
    require(len(payload) <= MAX_IPC, 'Sway IPC request exceeds bound')
    connection.sendall(b'i3-ipc' + struct.pack('<II', len(payload), message_type) + payload)
    header = read_exact(connection, 14)
    require(header[:6] == b'i3-ipc', 'invalid Sway IPC response magic')
    size, reply_type = struct.unpack('<II', header[6:14])
    require(0 < size <= MAX_IPC and reply_type == message_type,
            'invalid Sway IPC response size/type')
    body = json.loads(read_exact(connection, size))
    return body


def sway_request(path: Path, server: subprocess.Popen, identity: dict,
                 message_type: int, payload: bytes = b''):
    verify_private_peer(path, identity, server)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(4)
        connection.connect(str(path))
        credentials = peer_credentials(connection)
        require(credentials == (server.pid, os.getuid(), os.getgid()),
                'Sway IPC peer changed between checks')
        return sway_ipc(connection, message_type, payload)


def tree_nodes(node):
    if isinstance(node, dict):
        yield node
        for key in ('nodes', 'floating_nodes'):
            for child in node.get(key, []):
                yield from tree_nodes(child)


def app_window(tree, app_pid: int) -> dict:
    matches = [node for node in tree_nodes(tree)
               if node.get('pid') == app_pid and node.get('type') in ('con', 'floating_con')]
    require(len(matches) == 1, f'expected one mapped PigADE window for PID {app_pid}, found {len(matches)}')
    require(matches[0].get('visible') is True or matches[0].get('focused') is True,
            'PigADE window is not mapped/visible')
    return matches[0]


def run_bounded(argv: list[str], env: dict[str, str], *, timeout: float = 8,
                cap: int = MAX_TOOL_OUTPUT) -> tuple[int, bytes, bytes]:
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, close_fds=True, env=env)
    assert process.stdout and process.stderr
    selector = select.poll()
    selector.register(process.stdout, select.POLLIN | select.POLLHUP)
    selector.register(process.stderr, select.POLLIN | select.POLLHUP)
    outputs = {process.stdout.fileno(): bytearray(), process.stderr.fileno(): bytearray()}
    stdout_fd, stderr_fd = process.stdout.fileno(), process.stderr.fileno()
    streams = {process.stdout.fileno(): process.stdout, process.stderr.fileno(): process.stderr}
    deadline = time.monotonic() + timeout
    try:
        while streams:
            remaining = deadline - time.monotonic()
            require(remaining > 0, f'{Path(argv[0]).name} exceeded deadline')
            for fd, event in selector.poll(max(1, int(remaining * 1000))):
                chunk = os.read(fd, min(65536, cap + 1 - len(outputs[fd])))
                if not chunk:
                    selector.unregister(fd); streams.pop(fd).close(); continue
                outputs[fd].extend(chunk)
                require(len(outputs[fd]) <= cap, f'{Path(argv[0]).name} output exceeded bound')
            if process.poll() is not None and not streams:
                break
        status = process.wait(timeout=max(0.1, deadline-time.monotonic()))
        return status, bytes(outputs[stdout_fd]), bytes(outputs[stderr_fd])
    except BaseException:
        if process.poll() is None:
            process.terminate()
            try: process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(timeout=1)
        raise
    finally:
        for stream in (process.stdout, process.stderr):
            if not stream.closed:
                stream.close()


def host_request(command: dict, timeout: float = 4) -> dict:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(timeout)
        connection.connect('/artifacts/state/host.sock')
        payload = json.dumps({'protocol': 2, 'command': command}, separators=(',', ':')).encode()
        connection.sendall(struct.pack('<I', len(payload)) + payload)
        size, = struct.unpack('<I', read_exact(connection, 4))
        require(0 < size <= MAX_IPC, 'host IPC response exceeds bound')
        value = json.loads(read_exact(connection, size))
        require(value.get('protocol') == 2 and value.get('error') is None,
                'private session host rejected fixture request')
        return value


def view_text(view: dict) -> str:
    return ''.join(cell.get('text', '') for cell in view.get('frame', {}).get('cells', [])
                   if isinstance(cell, dict))


def namespace_preflight(outer_namespaces: dict, attestation: dict) -> dict:
    current = {name: os.readlink('/proc/self/ns/' + name) for name in NAMESPACES}
    require(attestation.get('outer') == outer_namespaces,
            'launch attestation outer namespace identities differ from config')
    require(attestation.get('inside_pid') == attestation.get('inside_identity', {}).get('pid'),
            'launch attestation inside PID fields disagree')
    for name in NAMESPACES:
        require(current[name] == attestation['inside_namespaces'][name]['identity'],
                f'current {name} namespace differs from parent-verified inside process')
        require(current[name] != outer_namespaces[name],
                f'private namespace boundary missing for {name}')
    forbidden_env = ('DISPLAY', 'WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'XAUTHORITY', 'SWAYSOCK',
                     'DBUS_SESSION_BUS_ADDRESS', 'DBUS_SYSTEM_BUS_ADDRESS',
                     'XDG_ACTIVATION_TOKEN', 'SSH_AUTH_SOCK', 'MIR_SOCKET',
                     'HYPRLAND_INSTANCE_SIGNATURE')
    require(not any(name in os.environ for name in forbidden_env),
            'host display/session environment leaked into private namespace')
    for path in ('/run/user', '/tmp/.X11-unix', '/dev/input', '/dev/uinput', '/dev/dri',
                 '/dev/nvidia0', '/dev/nvidiactl'):
        require(not Path(path).exists(), f'host runtime/device path exposed: {path}')
    ipv4 = Path('/proc/net/route').read_text().splitlines()[1:]
    ipv6 = Path('/proc/net/ipv6_route').read_text().splitlines()
    require(all(row.split()[0] == 'lo' for row in ipv4), 'non-loopback IPv4 route is exposed')
    require(all(row.split()[-1] == 'lo' for row in ipv6), 'non-loopback IPv6 route is exposed')
    interfaces = {line.split(':', 1)[0].strip()
                  for line in Path('/proc/net/dev').read_text().splitlines()[2:]}
    require(interfaces <= {'lo'}, 'non-loopback network interface is exposed')
    return {'namespaces': current, 'uid': os.getuid(), 'interfaces': sorted(interfaces),
            'non_loopback_routes': 0, 'host_displays_buses_input_and_gpu_absent': True}


class LogSink:
    def __init__(self, path: Path):
        self.path = path
        self.rows = []
        self.threads = []

    def start(self, process: subprocess.Popen, name: str) -> None:
        path = self.path / (name + '.log')
        def drain(stream, destination):
            digest = hashlib.sha256(); saved = 0
            fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
            try:
                while chunk := stream.read(8192):
                    digest.update(chunk)
                    if saved < MAX_LOG:
                        piece = chunk[:MAX_LOG-saved]
                        os.write(fd, piece); saved += len(piece)
            finally:
                os.fsync(fd); os.close(fd); stream.close()
            self.rows.append({'process': name, 'bytes_saved': saved,
                              'stream_sha256': digest.hexdigest(), 'cap_bytes': MAX_LOG})
        thread = threading.Thread(target=drain, args=(process.stdout, path), daemon=True)
        thread.start(); self.threads.append(thread)


def start_process(argv: list[str], env: dict[str, str], logs: LogSink, name: str) -> subprocess.Popen:
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, close_fds=True, env=env)
    logs.start(process, name)
    return process


def private_pinyin(config: dict, home: Path, runtime: Path) -> str:
    env = dict(config['fcitx_env'])
    binary = config['x11_root'] + '/usr/bin/fcitx5-remote'
    for args in (['-s', 'pinyin'], ['-o']):
        code, stdout, stderr = run_bounded([binary, *args], env, timeout=4, cap=4096)
        require(code == 0 and len(stdout) <= 4096 and len(stderr) <= 4096,
                'private Fcitx could not select/activate Pinyin')
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        remaining = min(2, deadline - time.monotonic())
        code, stdout, stderr = run_bounded([binary, '-n'], env, timeout=remaining, cap=4096)
        if code == 0 and stdout.decode(errors='replace').strip() == 'pinyin' and not stderr:
            return 'pinyin'
        time.sleep(0.1)
    require(False, 'private Fcitx did not report Pinyin for a focused Wayland input context')
    return 'pinyin'


def png_dimensions(path: Path) -> tuple[int, int]:
    require(path.stat().st_size >= 33, 'private capture is shorter than a PNG header')
    with path.open('rb') as image:
        header = image.read(24)
    require(header[:8] == b'\x89PNG\r\n\x1a\n' and header[12:16] == b'IHDR',
            'private capture is not a PNG')
    return struct.unpack('>II', header[16:24])


def build_virtual_keyboard_helper(output: Path) -> dict:
    require(sha256(INPUT_PROTOCOL_XML) == INPUT_PROTOCOL_SHA256,
            'pinned virtual-keyboard protocol XML hash differs')
    require(sha256(WTYPE_LICENSE) == WTYPE_LICENSE_SHA256,
            'pinned wtype MIT license hash differs')
    require(INPUT_HELPER_SOURCE.is_file() and not INPUT_HELPER_SOURCE.is_symlink(),
            'fixed virtual-keyboard helper source missing')
    scanner = Path('/usr/bin/wayland-scanner')
    compiler = Path('/usr/bin/cc')
    require(scanner.is_file() and compiler.is_file(),
            'pinned local Wayland scanner or C compiler is unavailable')
    binary = output / 'ime-keyboard-helper'
    with tempfile.TemporaryDirectory(prefix='pigade-ime-keyboard-build-') as temporary:
        build_dir = Path(temporary)
        staged_binary = build_dir / 'ime-keyboard-helper'
        header = build_dir / 'virtual-keyboard-unstable-v1-client-protocol.h'
        protocol = build_dir / 'virtual-keyboard-unstable-v1-protocol.c'
        for argv in ([str(scanner), 'client-header', str(INPUT_PROTOCOL_XML), str(header)],
                     [str(scanner), 'private-code', str(INPUT_PROTOCOL_XML), str(protocol)]):
            code, stdout, stderr = run_bounded(argv, {'PATH': '/usr/bin', 'LC_ALL': 'C'},
                                               timeout=10, cap=64 * 1024)
            require(code == 0 and not stdout and not stderr,
                    'wayland-scanner failed to generate fixed protocol binding')
        code, stdout, stderr = run_bounded(
            [str(compiler), '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror',
             '-I', str(build_dir), str(INPUT_HELPER_SOURCE), str(protocol),
             '-lwayland-client', '-lxkbcommon', '-o', str(staged_binary)],
            {'PATH': '/usr/bin', 'LC_ALL': 'C'}, timeout=30, cap=128 * 1024)
        require(code == 0 and not stdout and not stderr,
                'fixed persistent virtual-keyboard helper did not compile cleanly')
        require(staged_binary.is_file() and not staged_binary.is_symlink(),
                'fixed virtual-keyboard compiler omitted output binary')
        os.replace(staged_binary, binary)
    binary.chmod(0o700)
    ldd = Path('/usr/bin/ldd')
    require(ldd.is_file() and not ldd.is_symlink(), 'system ldd is unavailable for helper closure')
    code, linkage_out, linkage_err = run_bounded(
        [str(ldd), str(binary)], {'PATH': '/usr/bin', 'LC_ALL': 'C'},
        timeout=8, cap=32 * 1024)
    require(code == 0 and not linkage_err and b'not found' not in linkage_out,
            'virtual-keyboard helper has an unresolved dynamic dependency')
    libraries = []
    for line in linkage_out.decode('utf-8', errors='strict').splitlines():
        line = line.strip()
        if not line or line.startswith('linux-vdso.so.'):
            continue
        fields = line.split()
        name = Path(fields[0]).name
        resolved = fields[fields.index('=>') + 1] if '=>' in fields else fields[0]
        resolved_path = Path(resolved)
        require(resolved_path.is_absolute() and resolved_path.is_file(),
                'ldd returned a missing or indirect helper dependency')
        canonical = resolved_path.resolve(strict=True)
        require(canonical.is_relative_to(Path('/usr')),
                'helper dependency resolves outside the fixed /usr runtime')
        libraries.append({'name': name, 'path': str(canonical), 'sha256': sha256(canonical)})
    require(libraries, 'ldd returned no helper dependency closure')
    return {'path': str(binary), 'sha256': sha256(binary),
            'source_sha256': sha256(INPUT_HELPER_SOURCE),
            'protocol_xml_sha256': sha256(INPUT_PROTOCOL_XML),
            'wtype_license_sha256': sha256(WTYPE_LICENSE),
            'protocol_source': 'atx/wtype v0.4 protocol/virtual-keyboard-unstable-v1.xml',
            'scanner_sha256': sha256(scanner), 'compiler_sha256': sha256(compiler),
            'ldd_sha256': hashlib.sha256(linkage_out).hexdigest(),
            'ldd_bytes': len(linkage_out), 'ldd_libraries': libraries,
            'xkb_config_source': 'signed X11/Fcitx SDK payload; verified before private helper startup'}


def make_input_control_listener(runtime: Path) -> tuple[socket.socket, Path, int]:
    path = runtime / 'ime-keyboard.sock'
    require(len(os.fsencode(path)) < 108, 'private input-helper socket path exceeds Unix bound')
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM | socket.SOCK_CLOEXEC)
    listener.bind(str(path)); os.chmod(path, 0o600); listener.listen(1); listener.settimeout(12)
    return listener, path, path.lstat().st_ino


def start_virtual_keyboard(helper: Path, sway: subprocess.Popen, sway_identity: tuple[str, int],
                           runtime: Path, wayland_env: dict, logs: LogSink):
    listener, path, inode = make_input_control_listener(runtime)
    process = None
    try:
        helper_env = {'PATH': '/usr/bin', 'HOME': '/home/pigade', 'TMPDIR': '/tmp',
                      'XDG_RUNTIME_DIR': str(runtime), 'WAYLAND_DISPLAY': 'wayland-1',
                      'LC_ALL': 'C.utf8', 'LD_LIBRARY_PATH': wayland_env['LD_LIBRARY_PATH'],
                      'XKB_CONFIG_ROOT': wayland_env['XKB_CONFIG_ROOT']}
        process = start_process([str(helper), '--sway-pid', str(sway.pid)], helper_env,
                                logs, 'ime-keyboard')
        helper_identity = process_identity(process.pid)
        connection, _ = listener.accept()
        connection.settimeout(4)
        peer_pid, peer_uid, peer_gid = peer_credentials(connection)
        require(peer_pid == process.pid and peer_uid == os.getuid() and peer_gid == os.getgid() and
                process_identity(process.pid) == helper_identity and process.poll() is None,
                'persistent keyboard control peer is not the owned helper process')
        require(read_line(connection, timeout=12) == 'READY',
                'persistent virtual keyboard did not complete setup')
        require(process_identity(sway.pid) == sway_identity and sway.poll() is None,
                'private Sway identity changed during helper startup')
        return process, helper_identity, connection, listener, path, inode
    except BaseException:
        if process is not None and process.poll() is None:
            process.terminate()
            try: process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(timeout=2)
        listener.close()
        try:
            if path.lstat().st_ino == inode and stat.S_ISSOCK(path.lstat().st_mode):
                path.unlink()
        except FileNotFoundError:
            pass
        raise


def send_virtual_keyboard_command(connection: socket.socket, command: str, expected: str,
                                  helper: subprocess.Popen, helper_identity: tuple[str, int],
                                  sway: subprocess.Popen, sway_identity: tuple[str, int],
                                  socket_path: Path, wayland_identity: dict,
                                  ipc_path: Path, ipc_identity: dict,
                                  app: subprocess.Popen) -> None:
    require(command in ('COMPOSE', 'COMMIT', 'QUIT') and expected in
            ('COMPOSED', 'COMMITTED', 'QUIT'), 'unknown fixed virtual-keyboard command')
    require(helper.poll() is None and process_identity(helper.pid) == helper_identity,
            'persistent virtual-keyboard helper identity changed')
    require(sway.poll() is None and process_identity(sway.pid) == sway_identity,
            'private Sway identity changed before fixed input')
    verify_private_peer(socket_path, wayland_identity, sway)
    focused = app_window(sway_request(ipc_path, sway, ipc_identity, 4), app.pid)
    require(app.poll() is None and focused.get('focused') is True,
            'PigADE window lost exact-PID focus before fixed input')
    connection.sendall((command + '\n').encode('ascii'))
    require(read_line(connection, timeout=22) == expected,
            f'persistent virtual-keyboard helper did not acknowledge {command}')


def screenshot(path: Path, env: dict) -> dict:
    code, stdout, stderr = run_bounded(['/pigade/wayland-sdk/usr/bin/grim', str(path)],
                                       env, timeout=8, cap=4096)
    require(code == 0 and not stdout and not stderr and path.is_file(),
            'private grim capture failed')
    size = path.stat().st_size
    require(33 <= size <= MAX_PNG, 'private PNG capture outside size bounds')
    width, height = png_dimensions(path)
    require((width, height) == (1280, 800), 'private Sway output capture dimensions differ')
    return {'path': str(path), 'bytes': size, 'sha256': sha256(path),
            'dimensions': [width, height], 'visual_interpretation': 'requires human review'}


def inside_run() -> int:
    config = await_namespace_gate()
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    artifacts = Path('/artifacts')
    report = {'schema': 1, 'status': 'running', 'stage': 'private-namespace'}
    processes: list[subprocess.Popen] = []
    logs = LogSink(artifacts / 'logs')
    report_path = artifacts / 'run.json'
    def save():
        temporary = report_path.with_suffix('.tmp')
        data = (json.dumps(report, sort_keys=True, indent=2) + '\n').encode()
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
        with os.fdopen(fd, 'wb') as output:
            output.write(data); output.flush(); os.fsync(output.fileno())
        os.replace(temporary, report_path)
    try:
        runtime = Path('/run/pigade-ime'); home = Path('/home/pigade')
        runtime.mkdir(mode=0o700); home.mkdir(mode=0o700)
        for relative in ('.config/fcitx5', '.cache', '.local/share', '.local/state'):
            (home / relative).mkdir(parents=True, mode=0o700, exist_ok=True)
        require(runtime.stat().st_uid == os.getuid() and
                stat.S_IMODE(runtime.stat().st_mode) == 0o700, 'private runtime is not 0700')
        report['preflight'] = namespace_preflight(config['outer_namespaces'],
                                                  config['namespace_attestation']); save()

        sway_conf = artifacts / 'sway.conf'
        sway_conf.write_text('output HEADLESS-1 mode 1280x800\nxwayland disable\n')
        sway_conf.chmod(0o600)
        bus_config = runtime / 'dbus.conf'
        bus_config.write_text('<busconfig><type>session</type><listen>unix:path=' +
                              str(runtime / 'bus') + '</listen><auth>EXTERNAL</auth>'
                              '<policy context="default"><allow own="*"/>'
                              '<allow send_destination="*"/><allow receive_sender="*"/>'
                              '</policy></busconfig>\n')
        bus_config.chmod(0o600)
        minimal_env = {'PATH': '/usr/bin', 'HOME': str(home), 'TMPDIR': '/tmp',
                       'XDG_RUNTIME_DIR': str(runtime), 'LC_ALL': 'C.utf8',
                       'DBUS_SESSION_BUS_ADDRESS': 'unix:path=' + str(runtime / 'bus')}
        dbus = start_process(['/usr/bin/dbus-daemon', '--nofork', '--config-file=' + str(bus_config)],
                             minimal_env, logs, 'dbus')
        processes.append(dbus)
        bus_path = runtime / 'bus'
        wait_until(lambda: bus_path.is_socket() and dbus.poll() is None, 5)
        bus_stat = bus_path.lstat()
        bus_boot, bus_start = process_identity(dbus.pid)
        bus_peer = verify_private_peer(bus_path,
                                       {'socket_inode': bus_stat.st_ino,
                                        'boot_id': bus_boot, 'start_ticks': bus_start}, dbus)

        sway_env = {'PATH': '/usr/bin', 'HOME': str(home), 'TMPDIR': '/tmp',
                    'XDG_CONFIG_HOME': str(home / '.config'),
                    'XDG_RUNTIME_DIR': str(runtime), 'WAYLAND_DISPLAY': 'wayland-1',
                    'WLR_BACKENDS': 'headless', 'WLR_RENDERER': 'pixman',
                    'WLR_HEADLESS_OUTPUTS': '1', 'LD_LIBRARY_PATH': config['ld_library_path'],
                    'DBUS_SESSION_BUS_ADDRESS': minimal_env['DBUS_SESSION_BUS_ADDRESS'],
                    'LC_ALL': 'C.utf8'}
        report['stage'] = 'sway-start'; save()
        sway = start_process(['/pigade/wayland-sdk/usr/bin/sway', '--config',
                              '/artifacts/sway.conf', '--verbose'], sway_env, logs, 'sway')
        processes.append(sway)
        sway_boot, sway_start = process_identity(sway.pid)
        socket_path = runtime / 'wayland-1'
        wait_until(lambda: socket_path.is_socket() and sway.poll() is None, 12)
        socket_stat = socket_path.lstat()
        wayland_identity = {'socket_inode': socket_stat.st_ino, 'boot_id': sway_boot,
                            'start_ticks': sway_start}
        wayland_peer = verify_private_peer(socket_path, wayland_identity, sway)
        ipc_candidates = list(runtime.glob('sway-ipc.*.sock'))
        require(len(ipc_candidates) == 1, 'expected one private Sway IPC socket')
        ipc_path = ipc_candidates[0]
        ipc_info = ipc_path.lstat()
        ipc_identity = {'socket_inode': ipc_info.st_ino, 'boot_id': sway_boot,
                        'start_ticks': sway_start}
        ipc_peer = verify_private_peer(ipc_path, ipc_identity, sway)
        sway_output = sway_request(ipc_path, sway, ipc_identity, 3)
        require(isinstance(sway_output, list) and len(sway_output) == 1 and
                sway_output[0].get('active') is True and
                sway_output[0].get('current_mode', {}).get('width') == 1280 and
                sway_output[0].get('current_mode', {}).get('height') == 800,
                'Sway did not expose the one expected 1280x800 headless output')
        report['sway'] = {'pid': sway.pid, 'boot_id': sway_boot, 'start_ticks': sway_start,
                          'wayland_peer': wayland_peer, 'ipc_peer': ipc_peer,
                          'output': {'name': sway_output[0].get('name'), 'active': True,
                                     'size': [1280, 800]},
                          'config_sha256': sha256(sway_conf),
                          'config_has_only_fixed_output_and_disables_xwayland': sway_conf.read_text() ==
                          'output HEADLESS-1 mode 1280x800\nxwayland disable\n'}
        report['dbus'] = {'pid': dbus.pid, 'boot_id': bus_boot, 'start_ticks': bus_start,
                          'private_socket_peer': bus_peer,
                          'activation': 'disabled; custom config has no service directories or includes'}

        wayland_env = {'PATH': '/usr/bin', 'HOME': str(home), 'TMPDIR': '/tmp',
                       'XDG_RUNTIME_DIR': str(runtime), 'WAYLAND_DISPLAY': 'wayland-1',
                       'LC_ALL': 'C.utf8', 'LD_LIBRARY_PATH': config['ld_library_path'],
                       'XKB_CONFIG_ROOT': config['xkb_config_root']}
        wayland_peer = verify_private_peer(socket_path, wayland_identity, sway)
        code, stdout, stderr = run_bounded(['/pigade/wayland-sdk/usr/bin/wayland-info'],
                                           wayland_env, timeout=8, cap=MAX_TOOL_OUTPUT)
        require(code == 0 and not stderr, 'private wayland-info query failed')
        global_text = stdout.decode('utf-8', errors='strict')
        required_globals = ('zwp_text_input_manager_v3', 'zwp_input_method_manager_v2',
                            'zwp_virtual_keyboard_manager_v1')
        require(all(name in global_text for name in required_globals),
                'private compositor does not advertise required IME/virtual-keyboard globals')
        report['protocol_globals'] = {'socket_peer_rechecked': wayland_peer,
                                      'required': list(required_globals),
                                      'query_sha256': hashlib.sha256(stdout).hexdigest(),
                                      'query_bytes': len(stdout)}

        profile = home / '.config/fcitx5/profile'
        profile.write_text('''[Groups/0]\nName=Default\nDefault Layout=us\nDefaultIM=pinyin\n\n[Groups/0/Items/0]\nName=keyboard-us\nLayout=us\n\n[Groups/0/Items/1]\nName=pinyin\n\n[GroupOrder]\n0=Default\n''')
        profile.chmod(0o600)
        x11_root = config['x11_root']
        wayland_root = config['wayland_root']
        fcitx_env = {'PATH': '/usr/bin', 'HOME': str(home),
                     'XDG_CONFIG_HOME': str(home / '.config'),
                     'XDG_CACHE_HOME': str(home / '.cache'),
                     'XDG_DATA_HOME': str(home / '.local/share'),
                     'XDG_STATE_HOME': str(home / '.local/state'),
                     'XDG_RUNTIME_DIR': str(runtime), 'WAYLAND_DISPLAY': 'wayland-1',
                     'DBUS_SESSION_BUS_ADDRESS': minimal_env['DBUS_SESSION_BUS_ADDRESS'],
                     'TMPDIR': '/tmp', 'LC_ALL': 'C.utf8', 'LC_CTYPE': 'C.utf8',
                     'LD_LIBRARY_PATH': config['ld_library_path'],
                     'XDG_DATA_DIRS': x11_root + '/usr/share:' + wayland_root + '/usr/share:/usr/share',
                     'FCITX_ADDON_DIRS': x11_root + '/usr/lib/fcitx5:' + wayland_root + '/usr/lib/fcitx5',
                     'FCITX_DATA_DIRS': x11_root + '/usr/share/fcitx5:' + wayland_root + '/usr/share/fcitx5',
                     'LIBIME_MODEL_DIRS': x11_root + '/usr/lib/libime',
                     'XKB_CONFIG_ROOT': config['xkb_config_root']}
        pinyin_plugin = Path(x11_root + '/usr/lib/fcitx5/libpinyin.so')
        model = Path(x11_root + '/usr/lib/libime/zh_CN.lm')
        wayland_plugin = Path(wayland_root + '/usr/lib/fcitx5/libwayland.so')
        wayland_im_plugin = Path(wayland_root + '/usr/lib/fcitx5/libwaylandim.so')
        require(all(path.is_file() for path in (pinyin_plugin, model, wayland_plugin, wayland_im_plugin)),
                'pinned Fcitx Wayland/Pinyin payload incomplete')
        report['stage'] = 'fcitx-start'; save()
        fcitx = start_process([x11_root + '/usr/bin/fcitx5', '--replace'], fcitx_env, logs, 'fcitx')
        processes.append(fcitx)
        fcitx_boot, fcitx_start = process_identity(fcitx.pid)
        require(fcitx_boot == sway_boot, 'Fcitx process boot identity differs from Sway')
        wait_until(lambda: fcitx.poll() is None and bus_path.is_socket(), 8)

        helper_path = Path(config['input_helper']['path'])
        helper, helper_identity, helper_control, helper_listener, helper_socket, helper_inode = (
            start_virtual_keyboard(helper_path, sway, (sway_boot, sway_start), runtime,
                                   wayland_env, logs))
        processes.append(helper)
        report['input_helper'] = {'pid': helper.pid, 'boot_id': helper_identity[0],
                                  'start_ticks': helper_identity[1],
                                  'sha256': config['input_helper']['sha256'],
                                  'ldd_sha256': config['input_helper']['ldd_sha256'],
                                  'ldd_bytes': config['input_helper']['ldd_bytes'],
                                  'ldd_libraries': config['input_helper']['ldd_libraries'],
                                  'xkb_config_root': config['xkb_config_root'],
                                  'control_peer_verified': True,
                                  'persistent_object': 'one virtual keyboard, retained across both stages'}
        save()

        control_path = runtime / 'fixture.sock'
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(control_path)); control_path.chmod(0o600); listener.listen(1); listener.settimeout(8)
        report['stage'] = 'host-fixture'; save()
        host = start_process(['/pigade/host', 'serve', '/artifacts/state'],
                             {'PATH': '/usr/bin', 'HOME': str(home), 'TMPDIR': '/tmp',
                              'XDG_RUNTIME_DIR': str(runtime), 'LC_ALL': 'C.utf8'}, logs, 'host')
        processes.append(host)
        wait_until(lambda: (Path('/artifacts/state/host.sock').is_socket() or host.poll() is not None), 8)
        require(host.poll() is None, 'private session host exited before fixture launch')
        session, generation, operation = str(uuid.uuid4()), str(uuid.uuid4()), str(uuid.uuid4())
        launch = host_request({'command': 'launch', 'spec': {
            'session': session, 'generation': generation, 'operation': operation,
            'executable': '/usr/bin/python3', 'args': ['-c', FIXTURE, str(control_path), EXPECTED_TEXT.hex()],
            'cwd': '/artifacts', 'geometry': {'rows': 24, 'cols': 80}}})
        require(launch.get('error') is None, 'private host fixture launch failed')
        control, _ = listener.accept(); control.settimeout(8); listener.close()
        require(read_line(control) == 'READY', 'fixture did not become ready')
        wait_until(lambda: any('PIGADE-WAYLAND-IME-READY' in view_text(view)
                               for view in host_request({'command': 'poll', 'known': []}).get('views', [])), 8)

        app_env = dict(fcitx_env)
        app_env.update({'LP_NUM_THREADS': '2', 'LIBGL_ALWAYS_SOFTWARE': 'true',
                        'VK_DRIVER_FILES': '/pigade/platform-sdk/lavapipe-icd.json',
                        'VK_ICD_FILENAMES': '/pigade/platform-sdk/lavapipe-icd.json',
                        'FONTCONFIG_FILE': '/etc/fonts/fonts.conf'})
        report['stage'] = 'app-start'; save()
        app = start_process(['/pigade/app', '/artifacts/state/host.sock'], app_env, logs, 'app')
        processes.append(app)
        app_boot, app_start = process_identity(app.pid)
        require(app_boot == sway_boot, 'app process boot identity differs from Sway')
        deadline = time.monotonic() + 25
        window = None
        while time.monotonic() < deadline and app.poll() is None:
            current_tree = sway_request(ipc_path, sway, ipc_identity, 4)
            nodes = [node for node in tree_nodes(current_tree)
                     if node.get('pid') == app.pid and node.get('type') in ('con', 'floating_con')]
            if len(nodes) == 1 and (nodes[0].get('visible') is True or nodes[0].get('focused') is True):
                window = nodes[0]; break
            time.sleep(0.1)
        require(window is not None and app.poll() is None, 'no unique mapped PigADE window owned by app PID')
        # Exact PID criterion; only this private Sway IPC endpoint may change focus.
        command = f'[pid="{app.pid}"] focus'.encode('ascii')
        reply = sway_request(ipc_path, sway, ipc_identity, 0, command)
        require(isinstance(reply, list) and len(reply) == 1 and reply[0].get('success') is True,
                'private Sway rejected the exact-PID focus request')
        focused_tree = sway_request(ipc_path, sway, ipc_identity, 4)
        window = app_window(focused_tree, app.pid)
        require(window.get('focused') is True, 'PigADE window is not focused after private IPC request')
        report['app'] = {'pid': app.pid, 'boot_id': app_boot, 'start_ticks': app_start,
                         'unique_window_node_id': window.get('id'), 'focused_via': 'private Sway IPC exact PID'}
        save()

        pinyin = private_pinyin({'fcitx_env': fcitx_env, 'x11_root': x11_root}, home, runtime)
        report['fcitx'] = {'pid': fcitx.pid, 'boot_id': fcitx_boot, 'start_ticks': fcitx_start,
                           'profile': 'fresh private profile', 'active_input_method': pinyin,
                           'fixed_sequence': ['n', 'i', 'h', 'a', 'o', 'Space']}
        time.sleep(0.35)
        control.sendall(b'CHECK\n')
        require(read_line(control, timeout=2) == 'NO_PRECOMMIT_INPUT',
                'PTY received bytes before IME commit')
        before = host_request({'command': 'poll', 'known': []}).get('views', [])
        require(len(before) == 1 and 'PIGADE-WAYLAND-IME-READY' in view_text(before[0]),
                'fixture view missing before composition')
        report['stage'] = 'fixed-compose-and-commit'; save()
        require(all(name in global_text for name in required_globals), 'required Wayland globals changed')
        # The same virtual-keyboard object remains present from setup through commit.
        wayland_peer = verify_private_peer(socket_path, wayland_identity, sway)
        send_virtual_keyboard_command(helper_control, 'COMPOSE', 'COMPOSED', helper,
                                      helper_identity, sway, (sway_boot, sway_start),
                                      socket_path, wayland_identity, ipc_path, ipc_identity, app)
        time.sleep(0.35)
        control.sendall(b'CHECK\n')
        require(read_line(control, timeout=2) == 'NO_PRECOMMIT_INPUT',
                'PTY received bytes during composition before Space')
        still_before_commit = host_request({'command': 'poll', 'known': []}).get('views', [])
        require(len(still_before_commit) == 1 and
                'PIGADE-WAYLAND-IME-READY' in view_text(still_before_commit[0]),
                'host terminal output changed before IME commit')
        report['preedit_capture'] = screenshot(Path('/artifacts/preedit.png'), app_env)
        report['preedit_capture']['pty_bytes_before_commit'] = 0
        report['preedit_capture']['visual_claim'] = 'image retained for human review; not machine-recognized'
        report['composition_check'] = {'preedit_screenshot': True,
                                       'pty_unchanged_before_commit': True,
                                       'socket_peer': wayland_peer,
                                       'caveat': 'preedit requires human review of captured image'}
        report['commit'] = None
        save()
        report['stage'] = 'fixed-commit'; save()
        send_virtual_keyboard_command(helper_control, 'COMMIT', 'COMMITTED', helper,
                                      helper_identity, sway, (sway_boot, sway_start),
                                      socket_path, wayland_identity, ipc_path, ipc_identity, app)
        control.sendall(b'COLLECT\n')
        result_line = read_line(control, timeout=6)
        require(result_line.startswith('RESULT:'), 'raw PTY fixture omitted bounded result')
        result = json.loads(result_line[len('RESULT:'):])
        expected_hash = hashlib.sha256(EXPECTED_TEXT).hexdigest()
        require(result.get('received_bytes') == 6 and result.get('payload_sha256') == expected_hash and
                result.get('exact_once') is True and result.get('extra_bytes') == 0,
                'raw PTY did not receive exactly one UTF-8 你好 commit')
        def child_ack_seen():
            views = host_request({'command': 'poll', 'known': []}).get('views', [])
            return bool(views and ACK in view_text(views[0]))
        wait_until(child_ack_seen, 8)
        time.sleep(0.25)
        report['postcommit_capture'] = screenshot(Path('/artifacts/committed.png'), app_env)
        require(app.poll() is None and sway.poll() is None and fcitx.poll() is None and host.poll() is None,
                'a private fixture process exited before completion')
        report['commit'] = {'received_bytes': 6, 'payload_sha256': expected_hash,
                            'exact_once': True, 'extra_bytes': 0,
                            'child_ack_seen': True, 'child_ack': ACK,
                            'synthetic_payload_hex': EXPECTED_TEXT.hex()}
        send_virtual_keyboard_command(helper_control, 'QUIT', 'QUIT', helper,
                                      helper_identity, sway, (sway_boot, sway_start),
                                      socket_path, wayland_identity, ipc_path, ipc_identity, app)
        helper.wait(timeout=4)
        helper_control.close()
        helper_listener.close()
        if helper_socket.lstat().st_ino == helper_inode:
            helper_socket.unlink()
        report.update({'status': PASS, 'stage': 'complete',
                       'limits': ['one fixed Sway 1.12/Pixman/Fcitx 5.1.23 Pinyin fixture',
                                  'fixed virtual-keyboard helper is test-only and uses one persistent protocol object',
                                  'preedit is captured, not machine-classified',
                                  'no universal Wayland compositor or IME claim',
                                  'SDK runtime uses host /usr ABI and is not a portable sysroot']})
        save()
        return 0
    except Exception as error:
        report.update({'status': 'failed-closed', 'failure_stage': report.get('stage'),
                       'failure_type': type(error).__name__, 'failure_reason': str(error)[:320]})
        save()
        return 1
    finally:
        if 'helper_control' in locals():
            helper_control.close()
        if 'helper_listener' in locals():
            helper_listener.close()
        if 'helper_socket' in locals():
            try:
                info = helper_socket.lstat()
                if info.st_ino == helper_inode and stat.S_ISSOCK(info.st_mode):
                    helper_socket.unlink()
            except FileNotFoundError:
                pass
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try: process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait(timeout=2)
        for thread in logs.threads:
            thread.join(timeout=2)
        report['process_logs'] = sorted(logs.rows, key=lambda row: row['process'])
        save()


def read_line(connection: socket.socket, timeout: float = 4) -> str:
    data = bytearray()
    deadline = time.monotonic() + timeout
    while len(data) <= 4096:
        remaining = deadline - time.monotonic()
        require(remaining > 0, 'bounded control line exceeded deadline')
        connection.settimeout(remaining)
        byte = connection.recv(1)
        require(byte, 'PTY fixture control channel closed')
        if byte == b'\n':
            return data.decode('utf-8')
        data.extend(byte)
    raise RuntimeError('PTY fixture control line exceeded bound')


def verify_wayland_sdk(sdk_dir: Path) -> dict:
    manifest_path = sdk_dir / 'evidence/manifest.json'
    require(manifest_path.is_file() and not manifest_path.is_symlink(), 'Wayland SDK manifest missing')
    manifest = json.loads(manifest_path.read_text())
    preparer_path = ROOT / 'scripts/prepare-wayland-ime-sdk.py'
    preparer = load_script(preparer_path, 'wayland_ime_sdk_preparer_for_validation')
    require(manifest.get('status') == 'prepared-not-executed' and
            manifest.get('extraction_profile') == preparer.EXTRACTION_PROFILE,
            'Wayland SDK profile/status differs')
    require(manifest.get('preparer_sha256') == sha256(preparer_path), 'Wayland SDK preparer source changed')
    require(manifest_path.stat().st_uid == os.getuid(), 'Wayland SDK manifest is not user-owned')
    root = Path(manifest.get('extraction_root', '')).resolve(strict=True)
    require(root.is_relative_to(sdk_dir.resolve(strict=True)), 'Wayland SDK extraction escapes its root')
    require(root.stat().st_uid == os.getuid() and stat.S_IMODE(root.stat().st_mode) == 0o700,
            'Wayland SDK extraction root is not user-owned mode 0700')
    expected_pins = {item['name']: item for item in preparer.PACKAGES}
    rows = manifest.get('packages')
    require(isinstance(rows, list) and {row.get('name') for row in rows} == set(expected_pins),
            'Wayland SDK package set differs from pinned manifest')
    archive_root = sdk_dir / 'downloads'
    helpers = preparer.load_archive_helpers()
    keyring = sdk_dir / 'keyring/archlinux.gpg'
    require(keyring.is_file() and not keyring.is_symlink() and
            sha256(keyring) == manifest.get('keyring_sha256'), 'Wayland SDK keyring hash differs')
    packages = []
    for row in rows:
        pin = expected_pins[row['name']]
        archive = archive_root / preparer.archive_filename(pin)
        signature = Path(str(archive) + '.sig')
        require(archive.is_file() and not archive.is_symlink() and
                sha256(archive) == pin['sha256'] == row.get('archive_sha256'),
                f"Wayland archive pin differs: {row['name']}")
        require(signature.is_file() and not signature.is_symlink() and
                sha256(signature) == row.get('signature_sha256'),
                f"Wayland package signature hash differs: {row['name']}")
        checked = helpers.verify_signature(signature, archive,
                                            keyring,
                                            sdk_dir / 'keyring/gnupg', pin['signer'])
        require(checked['primary_fingerprint'] == pin['signer'],
                f"Wayland signer differs: {row['name']}")
        packages.append((row['name'], archive))
    inventory = preparer.archive_inventory(helpers, packages, preparer.SELECTED_FILES)
    require(inventory['files'] == manifest.get('payload_inventory'),
            'Wayland SDK manifest payload inventory does not match signed archives')
    preparer.verify_payload_inventory(root, inventory['files'])
    require(not manifest.get('unresolved_runtime_dependencies'), 'Wayland SDK has unresolved dependencies')
    return {'manifest_sha256': sha256(manifest_path), 'preparer_sha256': sha256(preparer_path),
            'extraction_root': str(root), 'packages': [
                {'name': name, 'version': expected_pins[name]['version'],
                 'archive_sha256': expected_pins[name]['sha256'],
                 'signer': expected_pins[name]['signer']} for name, _ in packages],
            'payload_files_verified': len(inventory['files']),
            'host_abi_runtime_only': True}


def bwrap_command(config_path: Path, args, wayland_root: Path, x11_root: Path,
                  platform_root: Path, private_icd: Path, info_fd: int,
                  input_helper: Path | None = None) -> list[str]:
    config = json.loads(config_path.read_text())
    return [
        'bwrap', '--unshare-all', '--new-session', '--die-with-parent', '--clearenv',
        '--ro-bind', '/usr', '/usr', '--symlink', 'usr/bin', '/bin',
        '--symlink', 'usr/lib', '/lib', '--symlink', 'usr/lib', '/lib64',
        '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/run',
        '--dir', '/home', '--dir', '/etc', '--dir', '/pigade',
        '--dir', '/pigade/platform-sdk', '--ro-bind', '/etc/fonts', '/etc/fonts',
        '--ro-bind', '/etc/ld.so.cache', '/etc/ld.so.cache',
        '--ro-bind', str(wayland_root), '/pigade/wayland-sdk',
        '--ro-bind', str(x11_root), '/pigade/fcitx-core',
        '--ro-bind', str(platform_root), '/pigade/platform-sdk/root',
        '--ro-bind', str(private_icd), '/pigade/platform-sdk/lavapipe-icd.json',
        '--ro-bind', str(args.output / 'passwd'), '/etc/passwd',
        '--ro-bind', str(args.output / 'group'), '/etc/group',
        '--bind', str(args.output), '/artifacts',
        '--ro-bind', str(args.app), '/pigade/app', '--ro-bind', str(args.host), '/pigade/host',
        *(['--ro-bind', str(input_helper), '/pigade/ime-keyboard'] if input_helper else []),
        '--chdir', '/artifacts',
        '--setenv', 'PATH', '/usr/bin', '--setenv', 'HOME', '/home/pigade',
        '--setenv', 'TMPDIR', '/tmp', '--setenv', 'XDG_RUNTIME_DIR', '/run/pigade-ime',
        '--setenv', 'LD_LIBRARY_PATH', config['ld_library_path'],
        '--setenv', 'VK_DRIVER_FILES', '/pigade/platform-sdk/lavapipe-icd.json',
        '--setenv', 'VK_ICD_FILENAMES', '/pigade/platform-sdk/lavapipe-icd.json',
        '--setenv', 'LIBGL_ALWAYS_SOFTWARE', 'true', '--setenv', 'LC_ALL', 'C.utf8',
        '--info-fd', str(info_fd),
        '--ro-bind', str(Path(__file__).resolve()), '/pigade/isolated-ime-wayland.py',
        '/usr/bin/python3', '/pigade/isolated-ime-wayland.py', '--inside']


def atomic_json(path: Path, value: dict) -> None:
    temporary = path.with_suffix(path.suffix + '.partial')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
    with os.fdopen(fd, 'w', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2, sort_keys=True); stream.write('\n')
        stream.flush(); os.fsync(stream.fileno())
    os.replace(temporary, path)


def read_bwrap_info(fd: int, process: subprocess.Popen, timeout: float = 8) -> tuple[dict, str]:
    require(stat.S_ISFIFO(os.fstat(fd).st_mode), 'bwrap info descriptor is not a pipe')
    deadline = time.monotonic() + timeout
    data = bytearray()
    while time.monotonic() < deadline and len(data) < 4096:
        require(process.poll() is None, 'bwrap exited before reporting its child identity')
        ready, _, _ = select.select([fd], [], [], min(0.1, deadline-time.monotonic()))
        if not ready:
            continue
        chunk = os.read(fd, min(4096-len(data), 1024))
        if not chunk:
            break
        data.extend(chunk)
        try:
            value = json.loads(data)
        except json.JSONDecodeError:
            continue
        require(isinstance(value, dict) and type(value.get('child-pid')) is int and
                value['child-pid'] > 0, 'bwrap returned invalid child identity metadata')
        return value, bytes(data).decode('utf-8', errors='replace')
    raise RuntimeError('bounded bwrap child identity read expired or exceeded 4096 bytes')


def outer_run(args) -> dict:
    app = args.app.resolve(strict=True); host = args.host.resolve(strict=True)
    require(app.is_file() and not args.app.is_symlink(), 'PigADE app binary must be regular')
    require(host.is_file() and not args.host.is_symlink(), 'session host binary must be regular')
    wayland = verify_wayland_sdk(args.wayland_sdk)
    x11_module = load_script(ROOT / 'scripts/isolated-ime-x11.py', 'x11_validation_helpers')
    x11 = x11_module.validate_sdk(args.x11_sdk, args.x11_sdk / 'evidence/manifest.json',
                                  ROOT / 'scripts/prepare-x11-ime-sdk.py')
    x11_preparer = load_script(ROOT / 'scripts/prepare-wayland-ime-sdk.py',
                               'wayland_preparer_x11_integrity_check')
    verified_x11_root, x11_metadata = x11_preparer.verified_x11_fcitx_root(
        args.x11_sdk, args.x11_sdk / 'evidence/manifest.json')
    require(verified_x11_root == Path(x11['extraction_root']).resolve(strict=True),
            'verified X11 Fcitx root differs from general SDK validation')
    xkb_inventory = x11_metadata['_verified_payload_inventory']['files']
    xkb_config_root = verified_xkb_config_root(verified_x11_root, xkb_inventory)
    x11['payload_files_verified'] = len(xkb_inventory)
    x11['xkb_config_root'] = xkb_config_root
    x11['xkb_required_files'] = list(XKB_REQUIRED_PAYLOADS)
    platform = x11_module.validate_platform_sdk(args.platform_sdk)
    font_verifier = load_script(ROOT / 'scripts/isolated-render.py', 'wayland_embedded_font_verifier')
    fonts = font_verifier.verify_embedded_fonts(app, FONT_MANIFEST)
    wayland_root = Path(wayland['extraction_root'])
    x11_manifest = json.loads((args.x11_sdk / 'evidence/manifest.json').read_text())
    x11_root = Path(x11_manifest['extraction_root']).resolve(strict=True)
    platform_manifest = json.loads((args.platform_sdk / 'manifest.json').read_text())
    platform_root = (Path(platform_manifest['sdk']) / 'root').resolve(strict=True)
    require(platform_root.is_relative_to(args.platform_sdk.resolve(strict=True)),
            'software-renderer payload root escapes its SDK')
    private_icd = args.output / 'lavapipe-icd.json'
    icd = json.loads(Path(platform_manifest['lavapipe_icd']).read_text())
    source_icd_path = icd['ICD']['library_path']
    icd['ICD']['library_path'] = '/pigade/platform-sdk/root/usr/lib/libvulkan_lvp.so'
    atomic_json(private_icd, icd)
    (args.output / 'state').mkdir(mode=0o700)
    (args.output / 'logs').mkdir(mode=0o700)
    passwd = f'root:x:0:0:root:/home/pigade:/bin/sh\npigade:x:{os.getuid()}:{os.getgid()}:Isolated test:/home/pigade:/usr/bin/bash\n'
    group = f'root:x:0:\npigade:x:{os.getgid()}:\n'
    for name, data in (('passwd', passwd), ('group', group)):
        path = args.output / name; path.write_text(data); path.chmod(0o600)
    ld_path = ':'.join(('/pigade/wayland-sdk/usr/lib', '/pigade/wayland-sdk/usr/lib/fcitx5',
                       '/pigade/fcitx-core/usr/lib', '/pigade/fcitx-core/usr/lib/fcitx5',
                       '/pigade/platform-sdk/root/usr/lib',
                       '/pigade/platform-sdk/root/usr/lib/weston'))
    helper_build_dir = tempfile.TemporaryDirectory(prefix='pigade-wayland-ime-helper-')
    input_helper = build_virtual_keyboard_helper(Path(helper_build_dir.name))
    outer_namespace_handles, outer_namespace_fds = open_outer_namespace_handles()
    nonce = secrets.token_hex(32)
    config = {'app_sha256': sha256(app), 'host_sha256': sha256(host),
              'harness_sha256': sha256(Path(__file__)),
              'wayland_manifest_sha256': wayland['manifest_sha256'],
              'x11_manifest_sha256': x11['manifest_sha256'],
              'platform_manifest_sha256': platform['manifest_sha256'],
              'font_profile': fonts,
              'wayland_root': '/pigade/wayland-sdk', 'x11_root': '/pigade/fcitx-core',
              'xkb_config_root': xkb_config_root,
              'platform_root': '/pigade/platform-sdk/root', 'ld_library_path': ld_path,
              'outer_namespaces': {name: item['identity']
                                   for name, item in outer_namespace_handles.items()},
              'input_helper': {**input_helper, 'path': '/pigade/ime-keyboard'},
              'namespace_gate_sha256': hashlib.sha256(nonce.encode()).hexdigest(),
              'icd_source_sha256': sha256(Path(platform_manifest['lavapipe_icd'])),
              'icd_private_sha256': sha256(private_icd), 'icd_source_library_path': source_icd_path,
              'fixed_sequence': ['n', 'i', 'h', 'a', 'o', 'space'],
              'tool_sha256': {
                  'sway': sha256(wayland_root / 'usr/bin/sway'),
                  'wayland-info': sha256(wayland_root / 'usr/bin/wayland-info'),
                  'ime_input_helper': input_helper['sha256'],
                  'ime_input_source': input_helper['source_sha256'],
                  'ime_input_protocol': input_helper['protocol_xml_sha256'],
                  'grim': sha256(wayland_root / 'usr/bin/grim'),
                  'fcitx5': sha256(x11_root / 'usr/bin/fcitx5'),
                  'fcitx5-remote': sha256(x11_root / 'usr/bin/fcitx5-remote'),
                  'harness': sha256(Path(__file__)),
                  'input_fixture': hashlib.sha256(FIXTURE.encode()).hexdigest(),
              },
              'fixture_expected_sha256': hashlib.sha256(EXPECTED_TEXT).hexdigest()}
    try:
        atomic_json(args.output / 'config.json', config)
    except BaseException:
        for fd in outer_namespace_fds:
            os.close(fd)
        raise
    info_read, info_write = os.pipe2(os.O_CLOEXEC)
    gate_read, gate_write = os.pipe2(os.O_CLOEXEC)
    listener = None
    listener_path = args.output / 'namespace-attest.sock'
    listener_inode = 0
    peer_connection = None
    try:
        listener, listener_inode = make_attestation_listener(listener_path)
        command = bwrap_command(args.output / 'config.json', args, wayland_root, x11_root,
                                platform_root, private_icd, info_write,
                                Path(input_helper['path']))
        atomic_json(args.output / 'run.json', {'schema': 1, 'status': 'running',
                                               'stage': 'bwrap-launch', **config})
        process = subprocess.Popen(command, stdin=gate_read, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.PIPE, close_fds=True, pass_fds=(info_write,),
                                   env={'PATH': '/usr/bin', 'HOME': str(args.output), 'LC_ALL': 'C.utf8'})
    except BaseException:
        if listener is not None:
            listener.close()
        unlink_owned_attestation_socket(listener_path, listener_inode)
        for fd in (info_read, info_write, gate_read, gate_write, *outer_namespace_fds):
            close_owned_fd(fd)
        mark_failed_run(args.output / 'run.json', 'bwrap-launch', sys.exception())
        raise
    gate_read = close_owned_fd(gate_read)
    info_write = close_owned_fd(info_write)
    assert process.stderr
    log_thread = threading.Thread(target=lambda: drain_outer(process.stderr, args.output / 'bwrap.log'),
                                  daemon=True)
    log_thread.start()
    stage = 'namespace-attestation'
    report_path = args.output / 'run.json'
    try:
        info, info_raw = read_bwrap_info(info_read, process)
        report = json.loads(report_path.read_text())
        report.update({'stage': 'bwrap-info-observed',
                       'bwrap_info_raw': info_raw,
                       'bwrap_info_parsed': info})
        atomic_json(report_path, report)
        info_pid = info['child-pid']
        info_identity = process_identity(info_pid)
        bwrap_identity = process_identity(process.pid)
        peer_connection, inside_pid, peer_uid, peer_gid = accept_inside_hello(listener)
        ancestry = verify_attested_inside_process(inside_pid, peer_uid, peer_gid,
                                                  info_pid, info_identity,
                                                  process.pid, bwrap_identity, process)
        inside_identity = process_identity(inside_pid)
        child_namespaces = inspect_child_namespace_handles(inside_pid, inside_identity)
        report.update({'stage': 'kernel-namespace-identities-observed',
                       'inside_pid': inside_pid,
                       'inside_identity': {'boot_id': inside_identity[0],
                                           'start_ticks': inside_identity[1]},
                       'bwrap_info_child_pid': info_pid,
                       'bwrap_info_child_identity': {'boot_id': info_identity[0],
                                                     'start_ticks': info_identity[1]},
                       'inside_ancestry': ancestry,
                       'kernel_inside_namespaces': child_namespaces})
        atomic_json(report_path, report)
        verify_child_namespace_separation(child_namespaces, outer_namespace_handles)
        info_evidence = verify_bwrap_namespace_info(info, child_namespaces)
        attestation = {'schema': 1,
                       'outer': config['outer_namespaces'],
                       'outer_namespace_handles': outer_namespace_handles,
                       'inside_pid': inside_pid,
                       'inside_identity': {'pid': inside_pid, 'boot_id': inside_identity[0],
                                           'start_ticks': inside_identity[1]},
                       'inside_ancestry': ancestry,
                       'inside_namespaces': child_namespaces,
                       'bwrap_info': info_evidence}
        config['namespace_attestation'] = attestation
        config['namespace_attestation_sha256'] = namespace_attestation_digest(attestation)
        atomic_json(args.output / 'config.json', config)
        report = json.loads(report_path.read_text())
        report.update({'stage': 'namespace-attestation-verified',
                       'namespace_attestation': attestation})
        atomic_json(report_path, report)
        gate = (nonce + ':' + config['namespace_attestation_sha256'] + '\n').encode()
        require(len(gate) == 130 and stat.S_ISFIFO(os.fstat(gate_write).st_mode),
                'parent namespace gate pipe is invalid')
        os.write(gate_write, gate)
        gate_write = close_owned_fd(gate_write)
        peer_connection.close(); peer_connection = None
        stage = 'private-runtime'
        report = json.loads(report_path.read_text())
        report['stage'] = stage
        atomic_json(report_path, report)
        status = process.wait(timeout=100)
    except subprocess.TimeoutExpired:
        process.kill(); process.wait(timeout=2)
        report = json.loads((args.output / 'run.json').read_text())
        report.update({'status': 'unknown-private-timeout', 'failure_stage': stage,
                       'failure_reason': 'bounded bwrap timeout; no automatic retry'})
        atomic_json(args.output / 'run.json', report)
        raise RuntimeError('private Wayland fixture timed out; retained state and will not retry')
    except BaseException:
        if process.poll() is None:
            process.kill(); process.wait(timeout=2)
        mark_failed_run(report_path, stage, sys.exception())
        raise
    finally:
        if peer_connection is not None:
            peer_connection.close()
        if listener is not None:
            listener.close()
        unlink_owned_attestation_socket(listener_path, listener_inode)
        for fd in (info_read, info_write, gate_read, gate_write):
            close_owned_fd(fd)
        for fd in outer_namespace_fds:
            close_owned_fd(fd)
        log_thread.join(timeout=2)
    report = json.loads((args.output / 'run.json').read_text())
    report['bwrap_exit_status'] = status
    if report.get('status') == 'running':
        report.update({'status': 'failed-closed', 'failure_stage': report.get('stage'),
                       'failure_reason': 'private run exited without terminal result'})
    atomic_json(args.output / 'run.json', report)
    require(status == 0 and report.get('status') == PASS,
            'isolated Wayland fixture failed; retained private run.json')
    return {'status': report['status'], 'run': str(args.output / 'run.json'),
            'app_sha256': sha256(app), 'host_sha256': sha256(host), 'fonts': fonts,
            'wayland_sdk': wayland, 'x11_sdk': x11, 'platform_sdk': platform,
            'captures': [str(args.output / name) for name in ('preedit.png', 'committed.png')],
            'limits': report.get('limits', [])}


def drain_outer(stream, path: Path) -> None:
    digest = hashlib.sha256(); saved = 0
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
    try:
        while chunk := stream.read(8192):
            digest.update(chunk)
            if saved < MAX_LOG:
                piece = chunk[:MAX_LOG-saved]; os.write(fd, piece); saved += len(piece)
    finally:
        os.fsync(fd); os.close(fd); stream.close()


def plan(args) -> dict:
    return {'status': 'plan-only-no-process-no-write', 'run_requires_explicit_flag': '--run',
            'app': str(args.app), 'host': str(args.host),
            'output': str(args.output) if args.output else None,
            'wayland_sdk_manifest': str(args.wayland_sdk / 'evidence/manifest.json'),
            'x11_fcitx_sdk_manifest': str(args.x11_sdk / 'evidence/manifest.json'),
            'software_renderer_manifest': str(args.platform_sdk / 'manifest.json'),
            'compositor': {'sway': '1:1.12-4', 'wlroots': '0.20.2-1',
                           'backend': 'headless', 'renderer': 'pixman', 'output': '1280x800'},
            'protocol_globals': ['zwp_text_input_manager_v3', 'zwp_input_method_manager_v2',
                                 'zwp_virtual_keyboard_manager_v1'],
            'fixed_input': ['n', 'i', 'h', 'a', 'o', 'Space'],
            'input_transport': 'one persistent private zwp_virtual_keyboard_v1 object across compose and commit',
            'arbitrary_text_input_option': False,
            'gates': ['fresh user/mount/pid/ipc/network namespaces and no non-loopback network',
                      'no host display/bus/GPU/input/provider-home paths or inherited endpoints',
                      'private Wayland Unix-socket inode and SO_PEERCRED match Sway PID/boot/start',
                      'private Sway IPC Unix-socket peer matches same Sway process',
                      'one persistent fixed-US virtual keyboard is connected before app startup and reused through both phases',
                      'exact protocol globals from owned Sway server',
                      'one mapped app window with exact PigADE PID, focused only via private Sway IPC',
                      'raw PTY receives exact UTF-8 你好 six bytes once after Space plus child ACK'],
            'limits': ['plan mode never starts a process or creates output',
                       'fixture captures preedit for human review; it does not machine-classify ink',
                       'one pinned host-ABI Sway/Fcitx path only; no universal Wayland claim']}


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--run', action='store_true', help='run the private Sway/Fcitx/app fixture')
    modes.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--app', type=Path, default=DEFAULT_APP)
    parser.add_argument('--host', type=Path, default=DEFAULT_HOST)
    parser.add_argument('--wayland-sdk', type=Path, default=WAYLAND_SDK)
    parser.add_argument('--x11-sdk', type=Path, default=X11_SDK)
    parser.add_argument('--platform-sdk', type=Path, default=PLATFORM_SDK)
    parser.add_argument('--output', type=Path)
    return parser.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    try:
        if args.inside:
            return inside_run()
        if not args.run:
            print(json.dumps(plan(args), indent=2))
            return 0
        require(args.output is not None, '--run requires a new private --output path')
        args.output = private_path(args.output)
        args.output.mkdir(mode=0o700, parents=True)
        require(stat.S_IMODE(args.output.stat().st_mode) == 0o700 and
                args.output.stat().st_uid == os.getuid(), 'output must be user-owned mode 0700')
        print(json.dumps(outer_run(args), indent=2))
        return 0
    except (RuntimeError, OSError, subprocess.SubprocessError, ValueError,
            KeyError, json.JSONDecodeError) as error:
        print(f'Isolated Wayland IME harness failed closed: {str(error)[:400]}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
