#!/usr/bin/env python3
"""Render the actual app only inside a verified, device-free private namespace.

No input injection. No active desktop sockets, config, bus, GPU or input devices.
Requires the local SDK prepared by prepare-platform-sdk.py and built binaries.
"""
import argparse
import ast
import hashlib
import json
import mmap
import os
from pathlib import Path
import re
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
IME_PREEDIT = '日本😀'


def app_command(ime_preedit, selection_fixture=False):
    command = ['/pigade/app', '/artifacts/state/host.sock']
    if ime_preedit:
        command.append('--diagnostic-preedit')
    if selection_fixture:
        command.append('--diagnostic-selection')
    return command


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def verify_embedded_fonts(app, manifest_path):
    """Reject a stale/different app build before any namespace or window starts."""
    manifest = json.loads(manifest_path.read_text())
    checked = []
    with app.open('rb') as binary, mmap.mmap(binary.fileno(), 0, access=mmap.ACCESS_READ) as executable:
        for face in manifest['files']:
            name = face['path']
            require(Path(name).name == name and name not in ('.', '..'), 'invalid font asset path')
            asset = manifest_path.parent / name
            require(asset.is_file() and asset.stat().st_size <= 32 * 1024**2,
                    'font asset missing or exceeds bound')
            payload = asset.read_bytes()
            require(len(payload) == face['bytes'] and hashlib.sha256(payload).hexdigest() == face['sha256'],
                    f'font source differs from manifest: {name}')
            require(executable.find(payload) >= 0,
                    f'app build does not embed the expected font: {name}; rebuild the selected binary')
            checked.append({'path': name, 'bytes': len(payload), 'sha256': face['sha256']})
    require(checked, 'font manifest has no faces')
    return {'profile_id': manifest['profile_id'], 'manifest_sha256': sha(manifest_path),
            'embedded_faces': checked,
            'scope': 'exact font payloads present in selected executable; runtime family registration still tested inside namespace'}


def recovery_issue(reply, journal, operation):
    diagnostics = reply.get('recovery')
    require(isinstance(diagnostics, dict), f'{operation} omitted recovery diagnostics')
    issues = diagnostics.get('issues')
    require(isinstance(issues, list), f'{operation} recovery issues were malformed')
    match = next((issue for issue in issues if issue.get('journal') == journal), None)
    require(match is not None, f'{operation} did not name {journal}')
    require(isinstance(match.get('message'), str) and match['message'],
            f'{operation} omitted the recovery reason')
    return match['message']


def shell_read_loop(recovery_warning):
    if not recovery_warning:
        return 'while IFS= read -r line; do printf \'%s\\n\' "$line"; done'
    cases = ' '.join(
        f"__PIGADE_RECOVERY_PROBE_{index}__) printf '%s\\n' '__PIGADE_RECOVERY_CHILD_ACK_{index}__' ;;"
        for index in range(1, 5))
    return ('while IFS= read -r line; do case "$line" in ' + cases +
            ' *) printf \'%s\\n\' "$line" ;; esac; done')


def wait_for(check, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise RuntimeError('bounded isolated test condition expired')


def gdbus_call(bus_address, destination, object_path, method, *arguments, timeout=2):
    require(re.fullmatch(r'(?::[0-9]+\.[0-9]+|org\.[A-Za-z0-9_.-]+)', destination) is not None,
            'invalid D-Bus destination')
    require(re.fullmatch(r'/[A-Za-z0-9_/]*', object_path) is not None, 'invalid D-Bus object path')
    require(re.fullmatch(r'[A-Za-z0-9_.]+\.[A-Za-z0-9_.]+', method) is not None,
            'invalid D-Bus method')
    command = ['/usr/bin/gdbus', 'call']
    if bus_address is None:
        command.append('--session')
    else:
        command.extend(['--address', bus_address])
    command.extend(['--dest', destination, '--object-path', object_path, '--method', method, *arguments])
    try:
        result = subprocess.run(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(f'bounded D-Bus call timed out: {method}') from error
    require(result.returncode == 0, f'D-Bus call failed ({method}): {result.stderr.strip()[:400]}')
    require(len(result.stdout.encode()) <= 64 * 1024, f'D-Bus response exceeded 64 KiB: {method}')
    return result.stdout.strip()


def gvariant_scalar(output):
    match = re.fullmatch(r'\(\s*<\s*(.*?)\s*>\s*,?\s*\)', output, re.DOTALL)
    if match is None:
        match = re.fullmatch(r'\(\s*(.*?)\s*,?\s*\)', output, re.DOTALL)
    require(match is not None, f'unrecognized bounded GVariant scalar: {output[:160]}')
    value = match.group(1).strip().rstrip(',').strip()
    value = re.sub(r'^(?:string|u?int(?:8|16|32|64)|boolean|bool)\s+', '', value)
    if value == 'true':
        return True
    if value == 'false':
        return False
    try:
        return ast.literal_eval(value)
    except (ValueError, SyntaxError):
        if re.fullmatch(r'-?[0-9]+', value):
            return int(value)
        raise RuntimeError(f'unrecognized GVariant scalar payload: {output[:160]}')


def gvariant_string(output):
    value = gvariant_scalar(output)
    require(isinstance(value, str), 'D-Bus result was not a string')
    return value


def gvariant_int(output):
    value = gvariant_scalar(output)
    require(isinstance(value, int) and not isinstance(value, bool), 'D-Bus result was not an integer')
    return value


def gvariant_bool(output):
    value = gvariant_scalar(output)
    require(isinstance(value, bool), 'D-Bus result was not a boolean')
    return value


def gvariant_strings(output):
    normalized = re.sub(r'\b(?:objectpath|uint32|int32)\s+', '', output)
    normalized = re.sub(r'<\s*(\[[^<>]*\])\s*>', r'\1', normalized)
    try:
        value = ast.literal_eval(normalized)
    except (ValueError, SyntaxError) as error:
        raise RuntimeError(f'unrecognized GVariant string array: {output[:160]}') from error
    require(isinstance(value, tuple) and len(value) == 1 and isinstance(value[0], list),
            'D-Bus result was not a single string array')
    require(all(isinstance(item, str) for item in value[0]), 'D-Bus array contained a non-string')
    return value[0]


def gvariant_object_refs(output):
    quoted = r"(?:'(?:\\.|[^'\\])*'|\"(?:\\.|[^\"\\])*\")"
    pair_pattern = re.compile(rf'\(\s*({quoted})\s*,\s*(?:objectpath\s+)?({quoted})\s*\)')
    # D-Bus object references are (bus-name, object-path) structs. Parse only
    # those structs and then require the whole response to contain no malformed
    # reference fragments.
    pairs = [(ast.literal_eval(match.group(1)), ast.literal_eval(match.group(2)))
             for match in pair_pattern.finditer(output)]
    require(pairs or re.search(r'\[\s*\]', output),
            f'unrecognized GVariant object-reference array: {output[:240]}')
    for name, path in pairs:
        require(re.fullmatch(r'(?::[0-9]+\.[0-9]+|org\.[A-Za-z0-9_.-]+)', name) is not None,
                'invalid D-Bus bus name in object reference')
        require(re.fullmatch(r'/[A-Za-z0-9_/]*', path) is not None,
                'invalid D-Bus object path in object reference')
    return pairs


def accessibility_probe(app_pid, bus_address, expected_alert_reason=None, max_seconds=25):
    deadline = time.monotonic() + max_seconds
    calls = {}

    def call(destination, path, method, *arguments):
        remaining = deadline - time.monotonic()
        require(remaining > 0, 'accessibility tree query exceeded its overall time limit')
        key = f'{destination} {path} {method}'
        calls[key] = calls.get(key, 0) + 1
        return gdbus_call(bus_address, destination, path, method, *arguments,
                          timeout=max(.05, min(2, remaining)))

    def property_value(bus_name, path, name, parser):
        value = call(bus_name, path, 'org.freedesktop.DBus.Properties.Get',
                     'org.a11y.atspi.Accessible', name)
        return parser(value)

    registry = 'org.a11y.atspi.Registry'
    desktop_path = '/org/a11y/atspi/accessible/root'
    app_refs = []
    pid_by_connection = {}
    registry_refs = []
    while time.monotonic() < deadline and not app_refs:
        registry_refs = gvariant_object_refs(call(
            registry, desktop_path, 'org.a11y.atspi.Accessible.GetChildren'))
        require(len(registry_refs) <= 32, 'AT-SPI registry exposed more than 32 application roots')
        for bus_name, path in registry_refs:
            if bus_name not in pid_by_connection:
                pid_output = gdbus_call(
                    bus_address, 'org.freedesktop.DBus', '/org/freedesktop/DBus',
                    'org.freedesktop.DBus.GetConnectionUnixProcessID', bus_name,
                    timeout=max(.05, min(2, deadline - time.monotonic())))
                pid_by_connection[bus_name] = gvariant_int(pid_output)
            if pid_by_connection[bus_name] == app_pid:
                app_refs.append((bus_name, path))
        if not app_refs:
            time.sleep(.1)
    require(app_refs, 'AT-SPI registry did not expose the private PigADE process; '
            f'target PID {app_pid}, registry references {registry_refs}, '
            f'reference PIDs {pid_by_connection}')

    # The numeric role IDs are from the AT-SPI role enumeration. Keep the raw
    # value for every node and annotate product roles needed by this probe.
    # AccessKit Role::Alert maps to AT-SPI Notification (101) in the pinned
    # accesskit_atspi_common bridge; AT-SPI Alert (2) is reserved for AlertDialog.
    role_names = {2: 'alert', 23: 'frame', 54: 'status-bar', 60: 'terminal', 75: 'application',
                  101: 'notification',
                  78: 'embedded', 83: 'heading'}
    max_nodes = 256
    max_depth = 12
    max_children = 64
    def collect_tree():
        queue = [(ref, None, 0) for ref in app_refs]
        seen = set()
        collected = []
        while queue:
            (bus_name, path), parent, depth = queue.pop(0)
            ref = (bus_name, path)
            if ref in seen:
                continue
            seen.add(ref)
            require(len(collected) < max_nodes, 'AT-SPI tree exceeded 256 nodes')
            require(depth <= max_depth, 'AT-SPI tree exceeded depth 12')
            name = property_value(bus_name, path, 'Name', gvariant_string)
            accessible_id = property_value(bus_name, path, 'AccessibleId', gvariant_string)
            role = gvariant_int(call(bus_name, path, 'org.a11y.atspi.Accessible.GetRole'))
            interfaces = gvariant_strings(call(
                bus_name, path, 'org.a11y.atspi.Accessible.GetInterfaces'))
            child_count = property_value(bus_name, path, 'ChildCount', gvariant_int)
            require(0 <= child_count <= max_children,
                    f'AT-SPI node {path} reports unsupported child count {child_count}')
            index = len(collected)
            node = {
                'index': index,
                'parent': parent,
                'depth': depth,
                'busName': bus_name,
                'objectPath': path,
                'name': name[:256],
                'accessibleId': accessible_id[:256],
                'role': {'id': role, 'name': role_names.get(role)},
                'interfaces': interfaces[:32],
                'childCount': child_count,
                'children': [],
            }
            require('org.a11y.atspi.Accessible' in interfaces,
                    f'AT-SPI node {path} omitted org.a11y.atspi.Accessible')
            collected.append(node)
            for child_index in range(child_count):
                child_output = call(bus_name, path, 'org.a11y.atspi.Accessible.GetChildAtIndex',
                                    str(child_index))
                child_refs = gvariant_object_refs(f'({child_output})')
                require(len(child_refs) == 1,
                        f'AT-SPI child lookup returned {len(child_refs)} references')
                child_ref = child_refs[0]
                node['children'].append({'busName': child_ref[0], 'objectPath': child_ref[1]})
                if child_ref not in seen:
                    queue.append((child_ref, index, depth + 1))
        return collected

    def find_required_nodes(collected):
        # AccessKit's pinned Linux mapping represents Role::Application as
        # AT-SPI Embedded (78); the separate bridge root remains Application (75).
        app = [node for node in collected if node['role']['id'] == 78 and
               node['name'] == 'PigADE' and node['accessibleId'] == 'pigade-root']
        terminal = [node for node in collected if node['role']['id'] == 60 and
                    node['accessibleId'] == 'terminal-canvas']
        return app, terminal

    tree_started_at = time.monotonic()
    nodes = collect_tree()
    tree_snapshots = 1
    app_nodes, terminal_nodes = find_required_nodes(nodes)
    retry_deadline = min(deadline, time.monotonic() + 10)
    while (not app_nodes or not terminal_nodes) and time.monotonic() < retry_deadline:
        time.sleep(.2)
        nodes = collect_tree()
        tree_snapshots += 1
        app_nodes, terminal_nodes = find_required_nodes(nodes)
    node_summary = [{'path': node['objectPath'], 'name': node['name'],
                     'accessibleId': node['accessibleId'], 'role': node['role'],
                     'interfaces': node['interfaces'], 'childCount': node['childCount']}
                    for node in nodes]
    alert_nodes = [node for node in nodes if node['role']['id'] == 101]
    if expected_alert_reason is not None:
        require(any(expected_alert_reason in node['name'] for node in alert_nodes),
                'AT-SPI notification for AccessKit Role::Alert omitted the recovery warning reason')
    return {
        'status': 'passed' if app_nodes and terminal_nodes else 'failed',
        'scope': 'read-only AT-SPI bridge tree inspection of the actual isolated PigADE PID; no actions issued',
        'registry': {'busName': registry, 'objectPath': desktop_path,
                     'applicationRootsForPigADE': app_refs},
        'assertions': {
            'pigadeApplicationFound': bool(app_nodes),
            'pigadeApplication': [{'index': node['index'], 'name': node['name'],
                                   'accessibleId': node['accessibleId'], 'role': node['role']}
                                  for node in app_nodes],
            'terminalCanvasFound': bool(terminal_nodes),
            'terminalCanvas': [{'index': node['index'], 'name': node['name'],
                                'accessibleId': node['accessibleId'], 'role': node['role']}
                               for node in terminal_nodes],
            'recoveryWarnings': [{'index': node['index'], 'name': node['name'],
                                  'accessibleId': node['accessibleId'], 'role': node['role']}
                                 for node in alert_nodes],
            'observedNodeSummary': node_summary,
        },
        'statusPropertiesSetOnPrivateSessionBus': ['org.a11y.Status.ScreenReaderEnabled=true'],
        'pinnedBridgeRoleMapping': {
            'source': 'accesskit_atspi_common 0.19.1, node.rs:93',
            'accesskitRole': 'Alert',
            'atspiRole': 'Notification',
            'atspiRoleId': 101,
        },
        'callCounts': calls,
        'limits': {'overallSeconds': max_seconds, 'maxNodes': max_nodes, 'maxDepth': max_depth,
                   'maxChildrenPerNode': max_children, 'maxRegistryRoots': 32,
                   'maxResponseBytes': 65536, 'perCallSeconds': 2,
                   'treeRetrySeconds': 10, 'treeSnapshots': tree_snapshots,
                   'treeWaitSeconds': round(time.monotonic() - tree_started_at, 3)},
        'nodes': nodes,
        'notTested': ['Orca or another screen reader', 'accessibility actions',
                      'keyboard operation', 'WCAG conformance', 'X11 bridge'],
    }


def inside():
    artifacts = Path('/artifacts')
    config = json.loads((artifacts / 'run.json').read_text())
    output_scale = config.get('output_scale', 1)
    require(output_scale in (1, 2), 'unsupported isolated integer output scale')
    output_width, output_height = 1280 * output_scale, 800 * output_scale
    processes = []
    process_groups = []
    logs = []
    report = {'scope': 'actual debug app; private Weston/Pixman + Lavapipe; no input injection',
              'status': 'failed', 'not_certified': ['real GPU/performance', 'input/IME', 'clipboard', 'accessibility', 'X11', 'scale/multi-monitor', 'VM reboot', '24h soak']}

    def start(argv, own_process_group=False, extra_env=None):
        log = open(artifacts / (Path(argv[0]).name + f'-{len(processes)}.log'), 'wb')
        logs.append(log)
        environment = os.environ.copy()
        if extra_env:
            environment.update(extra_env)
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                   close_fds=True, start_new_session=own_process_group,
                                   env=environment)
        processes.append(process)
        if own_process_group:
            process_groups.append(process.pid)
        return process

    def request_response(command):
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
            return reply

    def request(command):
        return request_response(command)['views']

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
        private_a11y_address = None
        if config.get('accessibility_probe'):
            launcher = start(['/usr/lib/at-spi-bus-launcher', '--launch-immediately',
                              '--a11y=0', '--screen-reader=0'],
                             own_process_group=True, extra_env={'GSETTINGS_BACKEND': 'memory'})

            def session_has_bus():
                if launcher.poll() is not None:
                    raise RuntimeError('private at-spi-bus-launcher exited')
                result = gdbus_call(None, 'org.freedesktop.DBus', '/org/freedesktop/DBus',
                                    'org.freedesktop.DBus.NameHasOwner', 'org.a11y.Bus')
                return gvariant_bool(result)

            wait_for(session_has_bus, timeout=8)
            private_a11y_address = gvariant_string(gdbus_call(
                None, 'org.a11y.Bus', '/org/a11y/bus', 'org.a11y.Bus.GetAddress'))
            require(private_a11y_address.startswith('unix:'),
                    'AT-SPI launcher returned a non-local bus address')
            registry_process = start(['/usr/lib/at-spi2-registryd', '--use-gnome-session'],
                                     own_process_group=True)

            def registry_has_bus():
                if registry_process.poll() is not None:
                    raise RuntimeError('private at-spi2-registryd exited')
                result = gdbus_call(private_a11y_address, 'org.freedesktop.DBus',
                                    '/org/freedesktop/DBus',
                                    'org.freedesktop.DBus.NameHasOwner',
                                    'org.a11y.atspi.Registry')
                return gvariant_bool(result)

            wait_for(registry_has_bus, timeout=8)
            report['accessibilityProbe'] = {
                'status': 'pending',
                'launcher': '/usr/lib/at-spi-bus-launcher --launch-immediately --a11y=0 --screen-reader=0',
                'privateSettingsBackend': 'GSETTINGS_BACKEND=memory (launcher process only)',
                'registry': '/usr/lib/at-spi2-registryd --use-gnome-session',
                'privateAccessibilityBusStarted': True,
                'configuredOnlyAfterAppInitialization': True,
            }
        sdk = config['sdk']
        # Weston 15's windowed/headless size arguments are logical. Its backend
        # multiplies them by output scale when allocating the capture buffer.
        weston = start([sdk + '/root/usr/bin/weston', '--backend=headless', '--renderer=pixman', '--shell=kiosk', '--no-config', '--socket=' + endpoint.name,
                        '--width=1280', '--height=800', f'--scale={output_scale}', '--refresh-rate=60000', '--idle-time=0', '--fake-seat', '--debug', '--log=/artifacts/weston.log'])
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
        recovery_journal = 'bad.journal'
        recovery_source = Path('/artifacts/state') / recovery_journal
        recovery_source_bytes = b'PigADE isolated recovery fixture: intentionally malformed journal\n'
        if config.get('recovery_warning'):
            recovery_source.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            recovery_source.write_bytes(recovery_source_bytes)
        recovery_source_sha_before = sha(recovery_source) if config.get('recovery_warning') else None
        host = start(['/pigade/host', 'serve', '/artifacts/state'])
        wait_for(lambda: Path('/artifacts/state/host.sock').is_socket() or host.poll() is not None)
        require(host.poll() is None, 'private host failed')
        specs = []
        for index in range(4):
            shell_script = (
                f"printf '\\033]2;Bash {index+1}\\007\\033[38;2;89;180;255mPigADE · Bash {index+1}\\033[0m\\r\\nРусский · é · 界\\r\\nA漢B xあ中한y · fi != ->\\r\\n\\033[1mA漢B xあ中한y · bold\\033[0m\\r\\n\\033[3mA漢B xあ中한y · italic\\033[0m\\r\\n\\033[1;3mA漢B xあ中한y · bold italic\\033[0m\\r\\n'; "
                + shell_read_loop(config.get('recovery_warning', False)))
            spec = {'session': str(uuid.uuid4()), 'generation': str(uuid.uuid4()), 'operation': str(uuid.uuid4()),
                    'executable': '/usr/bin/bash', 'args': ['--noprofile', '--norc', '-c', shell_script],
                    'cwd': '/artifacts', 'geometry': {'rows': 24, 'cols': 80}}
            request({'command': 'launch', 'spec': spec})
            specs.append(spec)
        initial_reply = request_response({'command': 'list'})
        initial = initial_reply['views']
        recovery_message = None
        if config.get('recovery_warning'):
            recovery_message = recovery_issue(initial_reply, recovery_journal, 'list')
            poll_reply = request_response({'command': 'poll', 'known': []})
            require(recovery_issue(poll_reply, recovery_journal, 'poll') == recovery_message,
                    'list and poll recovery reasons differed')
            source_unchanged = recovery_source.read_bytes() == recovery_source_bytes
            require(source_unchanged,
                    'host modified the malformed recovery source')
            report['recoveryWarning'] = {
                'journal': recovery_journal,
                'message': recovery_message,
                'sourceSha256Before': recovery_source_sha_before,
                'sourceSha256After': sha(recovery_source),
                'sourceBytesUnchanged': source_unchanged,
                'listAndPollDiagnosticsMatch': True,
                'omitted': initial_reply['recovery'].get('omitted'),
            }
        initial_ids = sorted((v['session'], v['generation'], v['pid']) for v in initial)
        first = start(app_command(config.get('ime_preedit', False), config.get('selection_fixture', False)))
        # Wait for host acknowledgement of app-computed viewport dimensions.
        wait_for(lambda: all(v['frame']['geometry'] != {'rows': 24, 'cols': 80} for v in request({'command': 'list'})))
        require(first.poll() is None, 'application exited before capture')
        if config.get('accessibility_probe'):
            # Setting ScreenReaderEnabled true makes the private launcher also
            # enable IsEnabled and emit its status change after AccessKit has
            # subscribed. No host GSettings or dconf is mounted or modified.
            result = gdbus_call(
                None, 'org.a11y.Bus', '/org/a11y/bus',
                'org.freedesktop.DBus.Properties.Set', 'org.a11y.Status',
                'ScreenReaderEnabled', '<true>')
            require(result == '()', 'private ScreenReaderEnabled=true update failed')
            status_values = {}
            for property_name in ('IsEnabled', 'ScreenReaderEnabled'):
                status_values[property_name] = gvariant_bool(gdbus_call(
                    None, 'org.a11y.Bus', '/org/a11y/bus',
                    'org.freedesktop.DBus.Properties.Get', 'org.a11y.Status', property_name))
                require(status_values[property_name],
                        f'private accessibility status property is not enabled: {property_name}')
            report['accessibilityProbe']['privateStatusValuesAfterActivation'] = status_values
            accessibility_result = accessibility_probe(
                first.pid, private_a11y_address,
                expected_alert_reason=(
                    f'{recovery_journal} — {recovery_message.split()[0]}'
                    if config.get('recovery_warning') and recovery_message.split() else None))
            report['accessibilityProbe'].update(accessibility_result)
            require(accessibility_result['status'] == 'passed',
                    'AT-SPI tree did not expose required PigADE application and terminal semantics')
        time.sleep(1)
        if config.get('recovery_warning'):
            require(len(initial) == 4 and all(v['state'] == 'running' for v in initial),
                    'recovery fixture did not retain all four running PTYs')
            for index, spec in enumerate(specs):
                probe = f'__PIGADE_RECOVERY_PROBE_{index + 1}__'
                # Each hosted shell is independently selected by stable IDs.
                # The child emits a different ACK token; tty echo alone cannot pass.
                # Input uses host protocol, with no desktop input.
                response = request_response({'command': 'input', 'session': spec['session'],
                                             'generation': spec['generation'],
                                             'bytes': list(f'{probe}\n'.encode())})
                require(response.get('error') is None, 'host rejected protocol input acknowledgement probe')
            def input_echoed():
                views = request({'command': 'list'})
                rendered = {v['session']: ''.join(cell['text'] for cell in v['frame']['cells']) for v in views}
                return all(f'__PIGADE_RECOVERY_CHILD_ACK_{index + 1}__' in rendered.get(spec['session'], '')
                           for index, spec in enumerate(specs))
            wait_for(input_echoed, timeout=8)
            require(all(v['state'] == 'running' and v['storage_error'] is None
                        for v in request({'command': 'list'})),
                    'a healthy PTY stopped or became degraded during recovery warning')
            report['recoveryWarning']['fourHealthyPtysRunning'] = True
            report['recoveryWarning']['hostProtocolChildAcknowledged'] = True
            report['recoveryWarning']['inputProbe'] = {
                'submitted': '__PIGADE_RECOVERY_PROBE_N__',
                'childOutput': '__PIGADE_RECOVERY_CHILD_ACK_N__',
                'ackIsDifferentFromSubmittedTTYText': True,
            }
            # Allow the app's normal poll/paint cycle to show PTY output before
            # capturing the recovery fixture; this is not a latency measurement.
            time.sleep(.5)
            report['recoveryWarning']['screenshotSettleSeconds'] = .5
        capture = start([sdk + '/root/usr/bin/weston-screenshooter'])
        require(capture.wait(timeout=10) == 0, 'private screenshot failed')
        images = list((artifacts / 'screenshots').glob('*.png'))
        require(images, 'no screenshot produced')
        for picture in images:
            with picture.open('rb') as source:
                png_header = source.read(24)
            require(png_header[:8] == b'\x89PNG\r\n\x1a\n'
                    and png_header[12:16] == b'IHDR', 'unexpected screenshot format')
            physical_size = struct.unpack('>II', png_header[16:24])
            require(physical_size == (output_width, output_height),
                    f'private output physical size mismatch: {physical_size}')
        report['output'] = {'scale': output_scale,
                            'physical_pixels': [output_width, output_height],
                            'logical_size_requested': [1280, 800],
                            'scope': 'one fixed integer-scale private software output; no fractional or multi-monitor claim'}
        first.kill()
        first.wait(timeout=5)
        reopened = start(app_command(config.get('ime_preedit', False), config.get('selection_fixture', False)))
        time.sleep(1)
        require(reopened.poll() is None, 'reopened app exited')
        final = request({'command': 'list'})
        require(sorted((v['session'], v['generation'], v['pid']) for v in final) == initial_ids, 'UI reopen changed live process identity')
        require(all(v['state'] == 'running' and v['storage_error'] is None for v in final), 'generation unavailable/degraded')
        if config.get('ime_preedit'):
            require(all(IME_PREEDIT not in ''.join(cell['text'] for cell in view['frame']['cells'])
                        for view in final), 'local IME fixture reached host cells')
            require(all(IME_PREEDIT.encode() not in path.read_bytes()
                        for path in Path('/artifacts/state').glob('*.journal')),
                    'local IME fixture reached a journal')
            report['imePreedit'] = {'text': IME_PREEDIT, 'selectionUtf16': [0, 2],
                                    'absentFromHostCellsAndJournals': True,
                                    'scope': 'Explicit same-app local composition overlay; screenshot review required. No actual IME protocol/input or commit performed.'}
        report['views'] = [{k: v[k] for k in ('session', 'generation', 'pid', 'sequence', 'state')} | {'geometry': v['frame']['geometry']} for v in final]
        report['screenshots'] = [{'name': p.name, 'sha256': sha(p), 'bytes': p.stat().st_size} for p in images]
        report['status'] = 'render_and_reopen_pass'
    except Exception as error:
        report['error'] = str(error)
    finally:
        for process_group in reversed(process_groups):
            try:
                os.killpg(process_group, signal.SIGKILL)
            except ProcessLookupError:
                pass
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
    embedded_fonts = verify_embedded_fonts(args.app, args.font_manifest)
    output = args.output.resolve()
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    (output / 'run.json').write_text(json.dumps({'sdk': str(sdk), 'outer_namespaces': {name: os.readlink('/proc/self/ns/' + name) for name in NAMESPACES},
        'app_sha256': sha(args.app), 'host_sha256': sha(args.host), 'sdk_packages': manifest['packages'],
        'accessibility_probe': args.accessibility, 'recovery_warning': args.recovery_warning,
        'ime_preedit': args.ime_preedit,
        'selection_fixture': args.selection_fixture,
        'output_scale': args.scale,
        'font_preflight': embedded_fonts}, indent=2) + '\n')
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
    parser.add_argument('--font-manifest', type=Path,
                        default=ROOT / 'crates/app/assets/fonts/derived-terminal-v1/derived-font-manifest.json')
    parser.add_argument('--output', type=Path, required=True, help='fresh owned artifact directory')
    parser.add_argument('--scale', type=int, choices=(1, 2), default=1,
                        help='fixed integer scale of the isolated output only (no host display changes)')
    parser.add_argument('--accessibility', action='store_true',
                        help='opt in to a private-bus, read-only AT-SPI tree inspection (not Orca/full acceptance)')
    parser.add_argument('--recovery-warning', action='store_true',
                        help='opt in to malformed-journal recovery, healthy PTY input and Alert coverage')
    parser.add_argument('--selection-fixture', action='store_true',
                        help='stage local cell selection in the same app, without input injection')
    parser.add_argument('--ime-preedit', action='store_true',
                        help='show a local same-app composition fixture without desktop input or PTY commit')
    sys.exit(outside(parser.parse_args()))
