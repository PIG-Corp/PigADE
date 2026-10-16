#!/usr/bin/env python3
"""Prepare or run an isolated, two-boot Alpine VM recovery experiment.

The test exercises only synthetic PTY sessions.  ``--prepare`` downloads and
verifies the official Alpine cloud image and creates an owned run directory;
only ``--run`` starts QEMU.  No host home, provider state, repository, display,
input device, or network interface is exposed to the guest.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
PROTOCOL = 2
IMAGE = 'alpine-3.24.2-x86_64-cloudinit-r2.qcow2'
IMAGE_URL = f'https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/cloud/{IMAGE}'
IMAGE_SHA512 = '6b9928cea1c6243affb8a9b09eba6046cd090e8bdb7141fbf0c8f2649c4a99d62e12cda289da41f99de82ce2b3e3780772dd594eaee3693cb49691d81cbe5d4b'
ALPINE_KEY_FINGERPRINT = 'F26ADFADBAE702EF7AF637459DA7EF23BFFCDF22'
ALPINE_KEY_URL = 'https://www.alpinelinux.org/keys/tomalok.asc'
MAX_IPC = 8 * 1024 * 1024


def check(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def sha512(path: Path) -> str:
    digest = hashlib.sha512()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def run(command: list[str], *, env: dict[str, str] | None = None,
        timeout: int = 120) -> str:
    result = subprocess.run(command, text=True, capture_output=True,
                            stdin=subprocess.DEVNULL, env=env, timeout=timeout, check=False)
    if result.returncode:
        raise RuntimeError(f'{Path(command[0]).name} failed ({result.returncode}): '
                           f'{result.stdout[-2000:]}{result.stderr[-3000:]}')
    return result.stdout + result.stderr


def private_dir(path: Path) -> Path:
    absolute = Path(os.path.abspath(path))
    for component in (absolute, *absolute.parents):
        check(not component.is_symlink(), f'refusing symlink path component: {component}')
    absolute.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = absolute.stat()
    check(info.st_uid == os.getuid() and stat.S_ISDIR(info.st_mode),
          f'directory must be owned by this user: {absolute}')
    absolute.chmod(0o700)
    fs = subprocess.check_output(['stat', '-f', '-c', '%T', str(absolute)], text=True).strip()
    check(fs not in ('tmpfs', 'ramfs'), f'durable filesystem required; found {fs}')
    return absolute


def download(url: str, destination: Path, max_bytes: int) -> None:
    import urllib.request
    partial = destination.with_name(destination.name + '.partial')
    check(not partial.is_symlink(), f'refusing symlink download target: {partial}')
    partial.unlink(missing_ok=True)
    request = urllib.request.Request(url, headers={'User-Agent': 'PigADE-VM-Reboot/0.0.1-dev'})
    count = 0
    with urllib.request.urlopen(request, timeout=45) as response, partial.open('xb') as target:
        while chunk := response.read(min(1024 * 1024, max_bytes - count + 1)):
            count += len(chunk)
            check(count <= max_bytes, f'download exceeds bound: {url}')
            target.write(chunk)
        target.flush()
        os.fsync(target.fileno())
    partial.replace(destination)


def verify_image(cache: Path) -> Path:
    cache.mkdir(mode=0o700, parents=True, exist_ok=True)
    cache.chmod(0o700)
    image = cache / IMAGE
    signature = cache / (IMAGE + '.asc')
    key = cache / 'alpine-cloud-image-signing-key.asc'
    if not image.exists():
        download(IMAGE_URL, image, 220 * 1024 * 1024)
    if not signature.exists():
        download(IMAGE_URL + '.asc', signature, 64 * 1024)
    if not key.exists():
        download(ALPINE_KEY_URL, key, 128 * 1024)
    check(not image.is_symlink() and image.is_file(), 'guest image must be a regular file')
    digest = sha512(image)
    check(digest == IMAGE_SHA512, f'Alpine image SHA-512 mismatch: {digest}')
    download_text = None
    # Validate the signed manifest too; the pinned release digest above remains
    # the primary expected value and makes a changed upstream sidecar fail closed.
    import urllib.request
    with urllib.request.urlopen(IMAGE_URL + '.sha512', timeout=30) as response:
        download_text = response.read(4096).decode('ascii', errors='strict')
    match = re.search(r'\b([0-9a-fA-F]{128})\b', download_text)
    check(match is not None and match.group(1).lower() == IMAGE_SHA512,
          'official SHA-512 sidecar did not match the pinned release digest')
    gpg_home = cache / 'gnupg'
    gpg_home.mkdir(mode=0o700, exist_ok=True)
    gpg = shutil.which('gpg')
    check(gpg is not None, 'gpg is required to verify the official Alpine image signature')
    env = {**os.environ, 'GNUPGHOME': str(gpg_home)}
    run([gpg, '--batch', '--import', str(key)], env=env)
    fingerprints = run([gpg, '--batch', '--with-colons', '--fingerprint'], env=env)
    found = {line.split(':')[9].upper() for line in fingerprints.splitlines()
             if line.startswith('fpr:') and len(line.split(':')) > 9}
    check(ALPINE_KEY_FINGERPRINT in found,
          f'Alpine public key fingerprint mismatch: {sorted(found)}')
    verified = run([gpg, '--batch', '--status-fd=1', '--verify', str(signature), str(image)], env=env)
    valid_signers = [line.split() for line in verified.splitlines()
                     if line.startswith('[GNUPG:] VALIDSIG ')]
    check(any(parts[2].upper() == ALPINE_KEY_FINGERPRINT or
              parts[-1].upper() == ALPINE_KEY_FINGERPRINT for parts in valid_signers),
          'image signature was not made by the pinned Alpine signing key')
    return image


def sdk_paths(manifest_path: Path) -> dict[str, Path]:
    manifest = json.loads(manifest_path.read_text())
    check(manifest.get('status') == 'ready', 'VM SDK manifest is not ready')
    check(manifest.get('commands', {}).get('qemu_version') == 'QEMU emulator version 11.1.2',
          'unexpected QEMU version')
    check(manifest.get('commands', {}).get('mcopy_version') == 'mcopy (GNU mtools) 4.0.49',
          'unexpected mtools version')
    paths = {name: Path(manifest[name]) for name in
             ('qemu_system_x86_64', 'qemu_data_dir', 'seabios_bios_256k', 'mcopy')}
    paths['mformat'] = paths['mcopy'].with_name('mformat')
    for name, path in paths.items():
        check(path.exists(), f'SDK manifest path missing ({name}): {path}')
    return paths


def stage_runtime(binary: Path, staging: Path) -> dict:
    binary = binary.resolve(strict=True)
    check(binary.is_file() and os.access(binary, os.X_OK), f'release host binary unavailable: {binary}')
    ldd = run(['ldd', str(binary)])
    check('not found' not in ldd, f'unresolved release host dependency:\n{ldd}')
    libs = staging / 'lib'
    libs.mkdir(mode=0o700, parents=True)
    entries = []
    for line in ldd.splitlines():
        match = re.match(r'\s*(\S+)\s+=>\s+(\S+)', line)
        if match:
            soname, source = match.groups()
        else:
            match = re.match(r'\s*(/\S+)\s+\(', line)
            if not match:
                continue
            source = match.group(1)
            soname = Path(source).name
        source_path = Path(source).resolve(strict=True)
        target = libs / Path(soname).name
        check(not target.exists(), f'duplicate runtime soname: {target.name}')
        shutil.copyfile(source_path, target)
        target.chmod(0o755)
        entries.append({'soname': target.name, 'source': str(source_path), 'sha256': sha256(target)})
    check(any(name.startswith('libc.so') for name in (entry['soname'] for entry in entries)),
          'release host ldd closure did not include libc')
    check(any(name.startswith('ld-linux') for name in (entry['soname'] for entry in entries)),
          'release host ldd closure did not include the glibc loader')
    host = staging / 'pigade-session-host'
    shutil.copyfile(binary, host)
    host.chmod(0o755)
    return {'source': str(binary), 'sha256': sha256(host), 'ldd': ldd,
            'libraries': entries, 'binary_version': run([str(binary), '--version']).strip()}


GUEST_SCENARIO = r'''#!/usr/bin/env python3
import hashlib, json, os, pathlib, socket, struct, subprocess, sys, time, uuid

ROOT = pathlib.Path('/var/lib/pigade-vm')
STATE = ROOT / 'state'
SOCKET = STATE / 'host.sock'
PHASE = ROOT / 'phase.json'
CLI_INVOCATIONS = ROOT / 'cli-invocations'
RESULT = ROOT / 'result.json'
NONCE = __NONCE__
PROTOCOL = 2

def ensure(ok, why):
    if not ok: raise RuntimeError(why)

def durable_json(path, value):
    tmp = path.with_suffix(path.suffix + '.tmp')
    with tmp.open('w') as out:
        json.dump(value, out, sort_keys=True, separators=(',', ':'))
        out.write('\n'); out.flush(); os.fsync(out.fileno())
    os.replace(tmp, path)
    fd = os.open(str(path.parent), os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0))
    try: os.fsync(fd)
    finally: os.close(fd)
    os.sync()

def ipc(command, allow_error=False):
    data = json.dumps({'protocol': PROTOCOL, 'command': command}, separators=(',', ':')).encode()
    ensure(len(data) <= 8 * 1024 * 1024, 'IPC request too large')
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(15); conn.connect(str(SOCKET))
        conn.sendall(struct.pack('<I', len(data)) + data)
        def read(size):
            out = bytearray()
            while len(out) < size:
                chunk = conn.recv(size - len(out)); ensure(chunk, 'truncated host response'); out.extend(chunk)
            return bytes(out)
        size, = struct.unpack('<I', read(4)); ensure(0 < size <= 8 * 1024 * 1024, 'invalid response size')
        reply = json.loads(read(size))
    ensure(reply.get('protocol') == PROTOCOL, 'host protocol mismatch')
    if not allow_error: ensure(reply.get('error') is None, 'host error: ' + str(reply.get('error')))
    return reply

def start_host():
    STATE.mkdir(mode=0o700, parents=True, exist_ok=True)
    env = dict(os.environ); env['LD_LIBRARY_PATH'] = '/opt/pigade-runtime/lib'
    env['HOME'] = '/root'; env['XDG_CONFIG_HOME'] = '/root/.config'
    with (ROOT / 'host.log').open('ab', buffering=0) as log:
        subprocess.Popen(['/usr/local/bin/pigade-session-host', 'serve', str(STATE)],
                         stdin=subprocess.DEVNULL, stdout=log, stderr=log, env=env,
                         start_new_session=True, close_fds=True)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        try: ipc({'command':'list'}); return
        except (OSError, RuntimeError): time.sleep(.1)
    raise RuntimeError('host did not become ready within 20 seconds')

def txt(view):
    return ''.join(cell.get('text', '') for cell in view['frame']['cells'])

def frame_hash(view):
    body = json.dumps(view['frame'], sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode()
    return hashlib.sha256(body).hexdigest()

def checkpoint_sequence(generation):
    paths = list(STATE.glob(generation + '.*.checkpoint'))
    if not paths: return None
    return max(int(path.name.split('.')[1]) for path in paths)

def snap(spec):
    reply = ipc({'command':'snapshot', 'session':spec['session'], 'generation':spec['generation']})
    ensure(len(reply.get('views', [])) == 1, 'missing session snapshot')
    view = reply['views'][0]
    ensure(view.get('storage_error') is None, 'host storage degraded: ' + str(view.get('storage_error')))
    ensure(view.get('generation') == spec['generation'], 'generation changed')
    return view

def build_shell_command(index):
    template = (
        "printf '%s\\n' '__INDEX__' >> /var/lib/pigade-vm/cli-invocations; sync; "
        "printf '\\033[?1049h\\033[2J\\033[HBOOT-__INDEX__-ALT-雪\\033[?1049l\\r\\n'; "
        "i=0; while [ $i -lt 12500 ]; do printf 'row-%d-雪-\\033[32mgreen\\033[0m\\r\\n' $i; i=$((i+1)); done; "
        "printf 'DONE-__INDEX__\\r\\n'; IFS= read -r answer; printf 'AFTER:%s\\r\\n' \"$answer\""
    )
    return template.replace('__INDEX__', str(index))

def first_boot(boot_id):
    start_host()
    ready = []
    for i in range(4):
        spec = {'session':str(uuid.uuid4()), 'generation':str(uuid.uuid4()), 'operation':str(uuid.uuid4()),
                'executable':'/bin/sh', 'args':['-c', build_shell_command(i)],
                'cwd':'/var/lib/pigade-vm', 'geometry':{'rows':36, 'cols':120}}
        reply = ipc({'command':'launch', 'spec':spec})
        ensure(len(reply.get('views', [])) == 1, 'launch did not return one view')
        ready.append(spec)
    deadline = time.monotonic() + 120
    baselines = []
    while time.monotonic() < deadline:
        baselines = []
        complete = (sorted(CLI_INVOCATIONS.read_text().splitlines()) == ['0', '1', '2', '3']) if CLI_INVOCATIONS.exists() else False
        for spec in ready:
            view = snap(spec)
            if 'DONE-' + str(ready.index(spec)) not in txt(view): complete = False
            if view.get('history', {}).get('retired_rows', 0) == 0: complete = False
            seq = checkpoint_sequence(spec['generation'])
            if seq != view['sequence']: complete = False
            baselines.append({'spec':spec, 'sequence':view['sequence'], 'frame_sha256':frame_hash(view),
                              'history':view.get('history'),
                              'journal_bytes':(STATE/(spec['generation']+'.journal')).stat().st_size,
                              'checkpoint_sequence':seq})
        if complete: break
        time.sleep(.1)
    ensure(complete, 'DONE marker and matching durable checkpoint deadline')
    markers = CLI_INVOCATIONS.read_text().splitlines()
    ensure(sorted(markers) == ['0', '1', '2', '3'], 'expected exactly four CLI process invocation marks')
    durable_json(PHASE, {'nonce':NONCE, 'phase':'first_boot_complete', 'boot_id':boot_id,
                         'sessions':baselines, 'cli_invocations':sorted(markers),
                         'cli_invocation_count':len(markers)})
    print('PIGADE_FIRST_BOOT:' + boot_id + ':4', flush=True)
    os.sync()
    subprocess.Popen(['/sbin/reboot', '-f'], stdin=subprocess.DEVNULL,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def second_boot(boot_id):
    phase = json.loads(PHASE.read_text())
    ensure(phase.get('nonce') == NONCE and phase.get('phase') == 'first_boot_complete', 'invalid durable phase record')
    ensure(phase.get('boot_id') != boot_id, 'guest boot_id did not change after real reboot')
    start_host()
    summary = {'nonce':NONCE, 'first_boot_id':phase['boot_id'], 'second_boot_id':boot_id,
               'sessions':[], 'cli_invocations_before_retry':CLI_INVOCATIONS.read_text().splitlines(),
               'scope':'synthetic PTY + actual guest reboot; no provider AI/task resume'}
    ensure(sorted(summary['cli_invocations_before_retry']) == ['0', '1', '2', '3'],
           'CLI invocation evidence changed across guest reboot')
    listed = ipc({'command':'list'}).get('views', [])
    ensure(len(listed) == 4, 'recovered host did not expose exactly four historical generations')
    for item in phase['sessions']:
        spec = item['spec']
        view = snap(spec)
        ensure(view['state'] != 'running' and view.get('pid') is None,
               'recovered process is falsely live or was restarted')
        ensure(view['sequence'] == item['sequence'], 'durable journal watermark changed across reboot')
        ensure(frame_hash(view) == item['frame_sha256'], 'recovered frame changed across reboot')
        ensure(view.get('history') == item['history'], 'cold-history continuation changed across reboot')
        check_seq = checkpoint_sequence(spec['generation'])
        ensure(check_seq == item['checkpoint_sequence'], 'checkpoint watermark changed across reboot')
        verification = subprocess.run(['/usr/local/bin/pigade-session-host', 'verify-recovery',
                                       str(STATE/(spec['generation']+'.journal'))],
                                      text=True, capture_output=True, timeout=120, check=False,
                                      env={**os.environ, 'LD_LIBRARY_PATH':'/opt/pigade-runtime/lib'})
        ensure(verification.returncode == 0, 'verify-recovery failed: '+verification.stderr[-1500:])
        proof = json.loads(verification.stdout)
        ensure(proof.get('complete_state_equal') and proof.get('frame_equal') and proof.get('cold_history_equal'),
               'full/checkpoint recovery equality failed')
        ensure(proof.get('cold_history_records', 0) > 0, 'reboot fixture did not exercise cold history')
        refused = ipc({'command':'input', 'session':spec['session'], 'generation':spec['generation'],
                       'bytes':list(b'FORBIDDEN\n')}, allow_error=True)
        ensure(refused.get('error') is not None, 'historical session accepted input')
        retried = ipc({'command':'launch', 'spec':spec})
        ensure(len(retried.get('views', [])) == 1 and retried['views'][0].get('pid') is None,
               'same-operation retry spawned or attached to a process')
        summary['sessions'].append({'session':spec['session'], 'generation':spec['generation'],
                                    'operation':spec['operation'], 'state':view['state'],
                                    'sequence':view['sequence'], 'frame_sha256':frame_hash(view),
                                    'checkpoint_sequence':check_seq, 'input_refused':True,
                                    'retry_pid':retried['views'][0].get('pid'),
                                    'verify_recovery':proof})
    after = CLI_INVOCATIONS.read_text().splitlines()
    ensure(sorted(after) == ['0', '1', '2', '3'], 'same-operation retry started a CLI process')
    ensure(len(ipc({'command':'list'}).get('views', [])) == 4, 'retry created a duplicate generation')
    summary['cli_invocations_after_retry'] = after
    summary['status'] = 'recovery_verified'
    durable_json(RESULT, summary)
    marker = json.dumps(summary, sort_keys=True, separators=(',', ':'))
    print('PIGADE_VM_RESULT:' + NONCE + ':' + marker, flush=True)

def main():
    ROOT.mkdir(mode=0o700, parents=True, exist_ok=True)
    # cloud-init runs this service on each boot; the persisted boot id selects
    # the second-boot verifier after the first boot has requested /sbin/reboot.
    boot_id = pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    out = open('/dev/ttyS0', 'a', buffering=1)
    sys.stdout = out; sys.stderr = out
    print('PIGADE_BOOT:' + boot_id, flush=True)
    if not PHASE.exists(): first_boot(boot_id)
    else: second_boot(boot_id)

if __name__ == '__main__':
    try:
        main()
    except Exception as exc:
        print('PIGADE_VM_FAILURE:' + NONCE + ':' + repr(exc), flush=True)
        raise
'''


def user_data(nonce: str) -> str:
    scenario = GUEST_SCENARIO.replace('__NONCE__', repr(nonce))
    service = '''#!/sbin/openrc-run
description="PigADE actual reboot recovery scenario"
command="/usr/bin/python3"
command_args="/usr/local/libexec/pigade-vm-test.py"
command_background="yes"
pidfile="/run/pigade-vm-test.pid"
start_pre() {
    modprobe 9pnet_virtio 2>/dev/null || true
    modprobe 9p 2>/dev/null || true
    mkdir -p /mnt/pigade-stage /opt/pigade-runtime/lib /var/lib/pigade-vm
    mountpoint -q /mnt/pigade-stage || mount -t 9p -o trans=virtio,version=9p2000.L,ro pigade_stage /mnt/pigade-stage
    test -r /mnt/pigade-stage/pigade-session-host || return 1
    cp /mnt/pigade-stage/pigade-session-host /usr/local/bin/pigade-session-host
    chmod 0755 /usr/local/bin/pigade-session-host
    cp /mnt/pigade-stage/lib/* /opt/pigade-runtime/lib/
    chmod 0755 /opt/pigade-runtime/lib/*
    mkdir -p /lib64
    ln -sf /opt/pigade-runtime/lib/ld-linux-x86-64.so.2 /lib64/ld-linux-x86-64.so.2
}
'''
    config = {
        'hostname': 'pigade-vm-recovery',
        'manage_etc_hosts': True,
        'datasource_list': ['NoCloud', 'None'],
        'network': {'config': 'disabled'},
        'write_files': [
            {'path':'/usr/local/libexec/pigade-vm-test.py', 'owner':'root:root',
             'permissions':'0700', 'content':scenario},
            {'path':'/etc/init.d/pigade-vm-test', 'owner':'root:root',
             'permissions':'0755', 'content':service},
        ],
        'runcmd': [
            ['/sbin/rc-update', 'add', 'pigade-vm-test', 'default'],
            ['/sbin/rc-service', 'pigade-vm-test', 'start'],
        ],
        'output': {'all': '| tee -a /var/log/cloud-init-output.log /dev/ttyS0'},
    }
    return '#cloud-config\n' + json.dumps(config, ensure_ascii=False, indent=2) + '\n'


def create_seed(sdk: dict[str, Path], run_dir: Path, nonce: str) -> Path:
    seed = run_dir / 'nocloud-seed.img'
    with seed.open('wb') as output:
        output.truncate(16 * 1024 * 1024)
        output.flush()
        os.fsync(output.fileno())
    run([str(sdk['mformat']), '-i', str(seed), '-v', 'CIDATA', '-F', '::'])
    files = {
        'user-data': run_dir / 'user-data',
        'meta-data': run_dir / 'meta-data',
        'network-config': run_dir / 'network-config',
    }
    files['user-data'].write_text(user_data(nonce))
    files['meta-data'].write_text('instance-id: iid-pigade-' + nonce + '\nlocal-hostname: pigade-vm-recovery\n')
    files['network-config'].write_text('version: 2\nethernets: {}\n')
    for name, path in files.items():
        run([str(sdk['mcopy']), '-i', str(seed), str(path), '::' + name])
    return seed


def process_identity(pid: int) -> tuple[str, int, int]:
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(') ', 1)[1].split()
    return (Path('/proc/sys/kernel/random/boot_id').read_text().strip(), pid, int(fields[19]))


def qmp_query(path: Path, command: str) -> dict:
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(2)
        client.connect(str(path))
        file = client.makefile('rwb', buffering=0)
        greeting = json.loads(file.readline())
        file.write(b'{"execute":"qmp_capabilities"}\r\n')
        while True:
            message = json.loads(file.readline())
            if 'return' in message: break
            check('error' not in message, f'QMP capabilities failed: {message}')
        file.write(json.dumps({'execute':command}).encode() + b'\r\n')
        while True:
            message = json.loads(file.readline())
            if 'return' in message: return {'greeting':greeting, 'result':message['return']}
            check('error' not in message, f'QMP {command} failed: {message}')


def prepare(args) -> tuple[Path, dict]:
    directory = private_dir(args.directory)
    fs = subprocess.check_output(['stat', '-f', '-c', '%T', str(directory)], text=True).strip()
    sdk = sdk_paths(args.sdk_manifest)
    binary = args.binary.resolve(strict=True)
    cache = directory / 'cache'
    cache.mkdir(mode=0o700, exist_ok=True)
    cache.chmod(0o700)
    image = verify_image(cache)
    run_dir = directory / ('run-' + time.strftime('%Y%m%dT%H%M%SZ', time.gmtime()) + '-' + uuid.uuid4().hex[:8])
    run_dir.mkdir(mode=0o700)
    (run_dir / 'runtime').mkdir(mode=0o700)
    staging = run_dir / 'stage'
    staging.mkdir(mode=0o700)
    runtime = stage_runtime(binary, staging)
    disk = run_dir / 'guest-disk.qcow2'
    shutil.copyfile(image, disk)
    os.chmod(disk, 0o600)
    with disk.open('rb') as disk_file: os.fsync(disk_file.fileno())
    nonce = uuid.uuid4().hex
    seed = create_seed(sdk, run_dir, nonce)
    bios = sdk['seabios_bios_256k']
    manifest = {
        'schema':1, 'status':'prepared', 'created_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'nonce':nonce, 'directory':str(run_dir), 'filesystem':fs,
        'image':{'url':IMAGE_URL, 'filename':IMAGE, 'size_bytes':image.stat().st_size,
                 'sha512':sha512(image), 'official_signature_valid':True,
                 'signer_fingerprint':ALPINE_KEY_FINGERPRINT},
        'binary':runtime, 'sdk_manifest':str(args.sdk_manifest.resolve()),
        'sdk':{'qemu':str(sdk['qemu_system_x86_64']), 'qemu_data_dir':str(sdk['qemu_data_dir']),
               'bios':str(bios), 'mcopy':str(sdk['mcopy']), 'mformat':str(sdk['mformat'])},
        'artifacts':{'disk':str(disk), 'seed':str(seed), 'stage':str(staging),
                     'serial_log':str(run_dir/'serial.log'), 'qemu_log':str(run_dir/'qemu.log'),
                     'qmp_socket':None, 'qmp_socket_directory':None},
        'isolation':{'network':'none', 'display':'none', 'input_devices':'none',
                     'host_mounts':[str(staging)], 'state':'owned guest disk'},
        'scope':'actual guest reboot and synthetic PTY recovery; not provider AI resume or power-loss acceptance',
    }
    (run_dir / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(f'Prepared VM inputs: {run_dir}', flush=True)
    return run_dir, manifest


def run_vm_with_qmp(run_dir: Path, manifest: dict, args, qmp: Path) -> int:
    sdk = sdk_paths(args.sdk_manifest)
    qemu = sdk['qemu_system_x86_64']
    env = {'HOME':str(run_dir/'home'), 'XDG_CONFIG_HOME':str(run_dir/'config'),
           'XDG_RUNTIME_DIR':str(run_dir/'runtime'), 'PATH':'/usr/bin:/bin',
           'LANG':'C.UTF-8', 'LC_ALL':'C.UTF-8',
           'LD_LIBRARY_PATH':str(qemu.parent.parent/'lib'),
           'QEMU_DATA_DIR':str(sdk['qemu_data_dir'])}
    for key in ('HOME','XDG_CONFIG_HOME','XDG_RUNTIME_DIR'):
        Path(env[key]).mkdir(mode=0o700, parents=True, exist_ok=True)
    command = [str(qemu), '-name','pigade-vm-recovery', '-machine','pc', '-accel','kvm',
               '-L',str(sdk['qemu_data_dir']),
               '-cpu','host', '-smp','2', '-m','1024M', '-bios',str(sdk['seabios_bios_256k']),
               # Alpine's official cloud image enables its serial userspace
               # console for this DMI manufacturer. No host identity is exposed.
               '-smbios','type=1,manufacturer=Amazon,serial=ds=nocloud',
               '-nodefaults', '-no-user-config', '-display','none', '-monitor','none',
               '-serial','file:'+manifest['artifacts']['serial_log'], '-parallel','none',
               '-qmp',f'unix:{qmp},server=on,wait=off',
               '-drive',f"file={manifest['artifacts']['disk']},format=qcow2,if=ide,index=0",
               # Raw FAT uses 512-byte disk sectors; presenting it as ATAPI
               # CD-ROM made the guest fail to mount it. The private disposable
               # seed is a regular disk, mounted read-only by cloud-init itself.
               '-drive',f"file={manifest['artifacts']['seed']},format=raw,if=ide,index=1",
               '-virtfs',f"local,path={manifest['artifacts']['stage']},mount_tag=pigade_stage,security_model=none,readonly=on",
               '-nic','none', '-boot','order=c']
    with (run_dir/'qemu.log').open('ab', buffering=0) as log:
        qemu_process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                        env=env, close_fds=True, start_new_session=True)
        identity = process_identity(qemu_process.pid)
        manifest['qemu'] = {'pid':qemu_process.pid, 'identity':identity,
                            'command':command, 'environment_keys':sorted(env)}
        manifest['status'] = 'running'
        (run_dir/'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        serial = Path(manifest['artifacts']['serial_log'])
        deadline = time.monotonic() + args.timeout
        marker = 'PIGADE_VM_RESULT:' + manifest['nonce'] + ':'
        failure = 'PIGADE_VM_FAILURE:' + manifest['nonce'] + ':'
        qmp_status = None
        try:
            while time.monotonic() < deadline:
                if qemu_process.poll() is not None:
                    raise RuntimeError(f'QEMU exited early with {qemu_process.returncode}; inspect {run_dir}/qemu.log')
                text = serial.read_text(errors='replace') if serial.exists() else ''
                if failure in text:
                    raise RuntimeError(text.split(failure, 1)[1][-2000:])
                if marker in text:
                    line = next((line for line in text.splitlines() if marker in line), None)
                    payload = line.split(marker, 1)[1]
                    result = json.loads(payload)
                    check(result.get('status') == 'recovery_verified', 'guest did not report recovery_verified')
                    boot_lines = [line.split('PIGADE_BOOT:',1)[1] for line in text.splitlines()
                                  if 'PIGADE_BOOT:' in line]
                    check(len(set(boot_lines)) >= 2, 'serial evidence does not show two distinct guest boots')
                    result_path = run_dir / 'guest-result.json'
                    result_path.write_text(json.dumps(result, indent=2) + '\n')
                    manifest['guest_result'] = result
                    manifest['guest_boot_ids'] = list(dict.fromkeys(boot_lines))
                    manifest['qmp_last_status'] = qmp_status
                    manifest['status'] = 'recovery_verified'
                    manifest['serial_sha256'] = sha256(serial)
                    manifest['qemu_log_sha256'] = sha256(run_dir/'qemu.log')
                    (run_dir/'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
                    print(json.dumps({'status':manifest['status'], 'run_directory':str(run_dir),
                                      'guest_boot_ids':manifest['guest_boot_ids'],
                                      'sessions':len(result['sessions'])}, sort_keys=True), flush=True)
                    return 0
                if qmp.exists():
                    try: qmp_status = qmp_query(qmp, 'query-status')['result']
                    except (OSError, RuntimeError, ValueError, json.JSONDecodeError): pass
                time.sleep(.5)
            raise RuntimeError(f'guest result deadline ({args.timeout}s) expired; inspect {serial} and {run_dir}/qemu.log')
        finally:
            if qemu_process.poll() is None:
                try:
                    if process_identity(qemu_process.pid) == tuple(identity):
                        qemu_process.terminate()
                        try:
                            qemu_process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            if process_identity(qemu_process.pid) == tuple(identity):
                                qemu_process.kill()
                                qemu_process.wait(timeout=5)
                except (OSError, ProcessLookupError):
                    pass
    return 1


def run_vm(run_dir: Path, manifest: dict, args) -> int:
    # AF_UNIX paths are short even when durable evidence lives under a long
    # ~/.local/state path. This private temporary directory holds only QMP.
    with tempfile.TemporaryDirectory(prefix='p-q-', dir='/tmp') as temporary:
        qmp_dir = Path(temporary)
        os.chmod(qmp_dir, 0o700)
        info = qmp_dir.stat()
        check(info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == 0o700,
              f'unsafe QMP socket directory: {qmp_dir}')
        qmp = qmp_dir / 'q'
        check(len(os.fsencode(qmp)) < 100, f'QMP socket path is too long: {qmp}')
        manifest['artifacts']['qmp_socket'] = str(qmp)
        manifest['artifacts']['qmp_socket_directory'] = str(qmp_dir)
        try:
            (run_dir/'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
            return run_vm_with_qmp(run_dir, manifest, args, qmp)
        finally:
            if qmp.exists() or qmp.is_symlink():
                qmp_meta = qmp.lstat()
                check(stat.S_ISSOCK(qmp_meta.st_mode) and qmp_meta.st_uid == os.getuid(),
                      f'refusing to remove unexpected QMP endpoint: {qmp}')
                qmp.unlink()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--prepare', action='store_true', help='verify/download image and prepare inputs; never starts QEMU')
    mode.add_argument('--run', action='store_true', help='prepare inputs and run a real headless QEMU guest reboot test')
    parser.add_argument('--directory', type=Path, required=True, help='owned durable filesystem for VM artifacts')
    parser.add_argument('--sdk-manifest', type=Path, default=Path('/tmp/pigade-vm-sdk/evidence/manifest.json'))
    parser.add_argument('--binary', type=Path, default=ROOT/'target/release/pigade-session-host')
    parser.add_argument('--timeout', type=int, default=600, help='overall guest completion timeout in seconds')
    args = parser.parse_args()
    check(args.timeout >= 60 and args.timeout <= 1800, 'timeout must be 60..1800 seconds')
    run_dir, manifest = prepare(args)
    if args.prepare:
        print(json.dumps({'status':'prepared', 'run_directory':str(run_dir),
                          'image_sha512':manifest['image']['sha512'],
                          'binary_sha256':manifest['binary']['sha256']}, sort_keys=True))
        return 0
    try:
        return run_vm(run_dir, manifest, args)
    except Exception as error:
        manifest['status'] = 'failed'
        manifest['error'] = str(error)
        manifest['serial_sha256'] = (sha256(Path(manifest['artifacts']['serial_log']))
                                     if Path(manifest['artifacts']['serial_log']).exists() else None)
        manifest['qemu_log_tail'] = ((run_dir/'qemu.log').read_text(errors='replace')[-6000:]
                                     if (run_dir/'qemu.log').exists() else '')
        (run_dir/'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        raise


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f'vm-reboot: {error}', file=sys.stderr)
        raise SystemExit(1)
