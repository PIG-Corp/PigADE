#!/usr/bin/env python3
"""Render the actual app only inside a verified, device-free private namespace.

No input injection. No active desktop sockets, config, bus, GPU or input devices.
Requires the local SDK prepared by prepare-platform-sdk.py and built binaries.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import stat
import struct
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
NAMESPACES = ('mnt', 'pid', 'user', 'ipc', 'net')


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def wait_for(check, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise RuntimeError('bounded isolated test condition expired')


def inside():
    artifacts = Path('/artifacts')
    config = json.loads((artifacts / 'run.json').read_text())
    processes = []
    logs = []
    report = {'scope': 'actual debug app; private Weston/Pixman + Lavapipe; no input injection',
              'status': 'failed', 'not_certified': ['real GPU/performance', 'input/IME', 'clipboard', 'accessibility', 'X11', 'scale/multi-monitor', 'VM reboot', '24h soak']}

    def start(argv):
        log = open(artifacts / (Path(argv[0]).name + f'-{len(processes)}.log'), 'wb')
        logs.append(log)
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=log, stderr=log, close_fds=True)
        processes.append(process)
        return process

    def request(command):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(4)
            connection.connect('/artifacts/state/host.sock')
            payload = json.dumps({'protocol': 2, 'command': command}).encode()
            connection.sendall(struct.pack('<I', len(payload)) + payload)

            def read(size):
                result = bytearray()
                while len(result) < size:
                    chunk = connection.recv(size - len(result))
                    require(chunk, 'truncated host IPC')
                    result.extend(chunk)
                return result
            length, = struct.unpack('<I', read(4))
            require(0 < length <= 8 * 1024**2, 'invalid host IPC length')
            reply = json.loads(read(length))
            require(reply['protocol'] == 2 and reply['error'] is None, f'host: {reply.get("error")}')
            return reply['views']

    try:
        runtime = Path('/run/pigade-test')
        runtime.mkdir(mode=0o700)
        for path in ('/home/test', '/home/test/.cache', '/home/test/.config', '/artifacts/screenshots'):
            Path(path).mkdir(parents=True, exist_ok=True)
        namespaces = {name: os.readlink('/proc/self/ns/' + name) for name in NAMESPACES}
        require(all(namespaces[name] != config['outer_namespaces'][name] for name in NAMESPACES), 'namespace boundary missing')
        require(not Path('/run/user').exists(), 'host runtime exposed')
        require(not Path('/tmp/.X11-unix').exists(), 'host X11 socket exposed')
        for device in ('input', 'uinput', 'dri', 'nvidia0', 'nvidiactl', 'kvm'):
            require(not Path('/dev', device).exists(), f'host device exposed: {device}')
        for variable in ('DISPLAY', 'WAYLAND_SOCKET', 'XAUTHORITY', 'HYPRLAND_INSTANCE_SIGNATURE', 'SWAYSOCK'):
            require(variable not in os.environ, f'host desktop variable inherited: {variable}')
        metadata = runtime.stat()
        require(metadata.st_uid == os.getuid() and stat.S_IMODE(metadata.st_mode) == 0o700, 'runtime ownership/mode')
        endpoint = runtime / os.environ['WAYLAND_DISPLAY']
        require(not endpoint.exists(), 'compositor socket was not fresh')
        report['preflight'] = {'namespaces': namespaces, 'uid': os.getuid(), 'devices': os.listdir('/dev'), 'runtime_mode': '0700', 'host_display_and_devices_absent': True}
        (artifacts / 'preflight.json').write_text(json.dumps(report['preflight'], indent=2) + '\n')
        sdk = config['sdk']
        weston = start([sdk + '/root/usr/bin/weston', '--backend=headless', '--renderer=pixman', '--shell=kiosk', '--no-config', '--socket=' + endpoint.name,
                        '--width=1280', '--height=800', '--scale=1', '--refresh-rate=60000', '--idle-time=0', '--fake-seat', '--debug', '--log=/artifacts/weston.log'])
        wait_for(lambda: endpoint.exists() or weston.poll() is not None)
        require(weston.poll() is None and endpoint.is_socket(), 'owned headless compositor failed')
        with socket.socket(socket.AF_UNIX) as connection:
            connection.connect(str(endpoint))
            pid, uid, _ = struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            require(pid == weston.pid and uid == os.getuid(), 'socket belongs to unexpected compositor')
        weston_log = (artifacts / 'weston.log').read_text()
        require('headless-backend.so' in weston_log and 'pixman' in weston_log.lower(), 'headless Pixman backend not confirmed')
        fd_targets = []
        for fd in Path(f'/proc/{weston.pid}/fd').iterdir():
            try:
                target = os.readlink(fd)
            except FileNotFoundError:
                continue
            require(not any(forbidden in target for forbidden in ('/dev/input', '/dev/dri', '/dev/nvidia', '/run/user')), 'compositor has external display/device FD')
            fd_targets.append(target)
        report['compositor'] = {'pid': pid, 'uid': uid, 'socket': str(endpoint), 'fd_targets': fd_targets}
        host = start(['/pigade/host', 'serve', '/artifacts/state'])
        wait_for(lambda: Path('/artifacts/state/host.sock').is_socket() or host.poll() is not None)
        require(host.poll() is None, 'private host failed')
        specs = []
        for index in range(4):
            spec = {'session': str(uuid.uuid4()), 'generation': str(uuid.uuid4()), 'operation': str(uuid.uuid4()),
                    'executable': '/usr/bin/bash', 'args': ['--noprofile', '--norc', '-c',
                    f"printf '\\033]2;Bash {index+1}\\007\\033[38;2;89;180;255mPigADE · Bash {index+1}\\033[0m\\r\\nРусский · é · 界\\r\\n'; while IFS= read -r line; do printf '%s\\n' \"$line\"; done"],
                    'cwd': '/artifacts', 'geometry': {'rows': 24, 'cols': 80}}
            request({'command': 'launch', 'spec': spec})
            specs.append(spec)
        initial = request({'command': 'list'})
        initial_ids = sorted((v['session'], v['generation'], v['pid']) for v in initial)
        first = start(['/pigade/app', '/artifacts/state/host.sock'])
        # Wait for host acknowledgement of app-computed viewport dimensions.
        wait_for(lambda: all(v['frame']['geometry'] != {'rows': 24, 'cols': 80} for v in request({'command': 'list'})))
        require(first.poll() is None, 'application exited before capture')
        time.sleep(1)
        capture = start([sdk + '/root/usr/bin/weston-screenshooter'])
        require(capture.wait(timeout=10) == 0, 'private screenshot failed')
        images = list((artifacts / 'screenshots').glob('*.png'))
        require(images, 'no screenshot produced')
        first.kill()
        first.wait(timeout=5)
        reopened = start(['/pigade/app', '/artifacts/state/host.sock'])
        time.sleep(1)
        require(reopened.poll() is None, 'reopened app exited')
        final = request({'command': 'list'})
        require(sorted((v['session'], v['generation'], v['pid']) for v in final) == initial_ids, 'UI reopen changed live process identity')
        require(all(v['state'] == 'running' and v['storage_error'] is None for v in final), 'generation unavailable/degraded')
        report['views'] = [{k: v[k] for k in ('session', 'generation', 'pid', 'sequence', 'state')} | {'geometry': v['frame']['geometry']} for v in final]
        report['screenshots'] = [{'name': p.name, 'sha256': sha(p), 'bytes': p.stat().st_size} for p in images]
        report['status'] = 'render_and_reopen_pass'
    except Exception as error:
        report['error'] = str(error)
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
        for log in logs:
            log.close()
        (artifacts / 'result.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return 0 if report['status'] == 'render_and_reopen_pass' else 1


def outside(args):
    sdk = args.sdk.resolve()
    manifest = json.loads((sdk / 'manifest.json').read_text())
    for path in (args.app, args.host):
        require(path.is_file(), f'build actual binary first: {path}')
    output = args.output.resolve()
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    (output / 'run.json').write_text(json.dumps({'sdk': str(sdk), 'outer_namespaces': {name: os.readlink('/proc/self/ns/' + name) for name in NAMESPACES},
        'app_sha256': sha(args.app), 'host_sha256': sha(args.host), 'sdk_packages': manifest['packages']}, indent=2) + '\n')
    (output / 'passwd').write_text(f'pigade:x:{os.getuid()}:{os.getgid()}:Isolated test:/home/test:/usr/bin/bash\n')
    (output / 'group').write_text(f'pigade:x:{os.getgid()}:\n')
    (output / 'dbus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth><policy context="default"><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy></busconfig>\n')
    command = ['bwrap', '--unshare-all', '--new-session', '--die-with-parent', '--clearenv', '--ro-bind', '/usr', '/usr', '--symlink', 'usr/bin', '/bin', '--symlink', 'usr/lib', '/lib', '--symlink', 'usr/lib', '/lib64', '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/run', '--dir', '/home', '--dir', '/etc', '--ro-bind', '/etc/fonts', '/etc/fonts', '--ro-bind', '/etc/ld.so.cache', '/etc/ld.so.cache', '--ro-bind', str(output / 'passwd'), '/etc/passwd', '--ro-bind', str(output / 'group'), '/etc/group', '--ro-bind', str(sdk), str(sdk), '--bind', str(output), '/artifacts', '--ro-bind', str(args.app.resolve()), '/pigade/app', '--ro-bind', str(args.host.resolve()), '/pigade/host', '--ro-bind', str(Path(__file__).resolve()), '/pigade/isolated-render.py']
    environment = dict(manifest['private_environment'], HOME='/home/test', XDG_RUNTIME_DIR='/run/pigade-test', XDG_CONFIG_HOME='/home/test/.config', XDG_CACHE_HOME='/home/test/.cache', XDG_PICTURES_DIR='/artifacts/screenshots', WAYLAND_DISPLAY='pigade-private', LP_NUM_THREADS='2', LIBGL_ALWAYS_SOFTWARE='true')
    for name, value in environment.items():
        command.extend(['--setenv', name, value])
    command.extend(['--', '/usr/bin/dbus-run-session', '--config-file=/artifacts/dbus.conf', '--', '/usr/bin/python3', '/pigade/isolated-render.py', '--inside'])
    with open(output / 'namespace.log', 'wb') as log:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log, close_fds=True, start_new_session=True)
        try:
            code = process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
            raise RuntimeError(f'isolated run timed out; owned namespace terminated; see {output}')
    result = output / 'result.json'
    print(result.read_text() if result.exists() else (output / 'namespace.log').read_text())
    return code


if __name__ == '__main__':
    if '--inside' in sys.argv:
        sys.exit(inside())
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sdk', type=Path, default=Path('/tmp/pigade-platform-sdk'))
    parser.add_argument('--app', type=Path, default=ROOT / 'crates/app/target/debug/pigade-app')
    parser.add_argument('--host', type=Path, default=ROOT / 'target/release/pigade-session-host')
    parser.add_argument('--output', type=Path, required=True, help='fresh owned artifact directory')
    sys.exit(outside(parser.parse_args()))
