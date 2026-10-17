#!/usr/bin/env python3
"""Prepare or run an isolated, two-boot Alpine VM recovery experiment.

The default mode exercises synthetic PTY sessions. Optional ``--corpus`` mode
copies a separately completed historical corpus to a private guest data disk.
``--prepare`` verifies the official Alpine cloud image and creates owned run
artifacts; only ``--run`` starts QEMU. No host home, provider state, repository,
display, input device, or network interface is exposed to the guest.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
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
CORPUS_DISK_BYTES = 64 * 1024**3
CORPUS_HOST_HEADROOM_BYTES = 8 * 1024**3
CORPUS_SOURCE_SUFFIXES = ('.journal', '.checkpoint', '.archive', '.timeline.jsonl')
CORPUS_REQUIRED_PROOFS = (
    'complete_state_equal', 'frame_equal', 'cold_history_equal',
    'live_state_equal', 'live_frame_equal', 'live_cold_history_equal',
)
CORPUS_RUNTIME_FILES = {
    'host.lock', 'metadata.sqlite', 'metadata.sqlite-wal',
    'metadata.sqlite-shm', 'metadata.sqlite-journal',
    'metadata.sqlite.writer.lock',
}
CORPUS_REPLAY_CACHE = re.compile(r'\.history-replay-[A-Za-z0-9_-]{6,64}')


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


def current_core_profile() -> str:
    source = ROOT / 'crates/terminal/src/lib.rs'
    match = re.search(r'pub const CORE_PROFILE: &str = "([^"]+)"', source.read_text())
    check(match is not None, 'could not read the pinned terminal core profile')
    return match.group(1)


def corpus_disk_serial(nonce: str) -> str:
    check(re.fullmatch(r'[0-9a-f]{32}', nonce) is not None, 'invalid VM run nonce')
    return 'pgd-data-' + nonce[:8]


def reject_symlink_components(path: Path, label: str) -> None:
    absolute = Path(os.path.abspath(path))
    for component in (absolute, *absolute.parents):
        check(not component.is_symlink(), f'{label} path has a symlink component: {component}')


def corpus_whitelist(directory: Path, manifest: dict) -> list[str]:
    """Return source files, validating but excluding known Host runtime artifacts."""
    expected = {'manifest.json'}
    generations = set()
    for row in manifest.get('results', []):
        generation = row.get('generation')
        journal = row.get('journal')
        timeline = row.get('logical_timeline')
        check(isinstance(generation, str) and re.fullmatch(r'[0-9a-f-]{36}', generation),
              'corpus has an invalid generation ID')
        check(generation not in generations, 'corpus contains a duplicate generation ID')
        generations.add(generation)
        check(isinstance(journal, str) and journal == Path(journal).name and journal.endswith('.journal'),
              'corpus has an invalid journal path')
        check(isinstance(timeline, str) and timeline == Path(timeline).name
              and timeline.endswith('.timeline.jsonl'), 'corpus has an invalid timeline path')
        expected.update((journal, timeline))

    check(len(generations) == 4, 'exactly four unique corpus generations are required')
    observed: set[str] = set()
    with os.scandir(directory) as entries:
        for entry in entries:
            check(not entry.is_symlink(), f'corpus contains a symlink: {entry.name}')
            if entry.is_file(follow_symlinks=False):
                name = entry.name
                if name in CORPUS_RUNTIME_FILES:
                    continue
                if name == 'manifest.json' or name.endswith(CORPUS_SOURCE_SUFFIXES) or name == 'rows.index':
                    observed.add(name)
                    continue
                raise RuntimeError(f'corpus contains an unexpected file: {name}')
            if entry.name == 'host.sock' and stat.S_ISSOCK(entry.stat(follow_symlinks=False).st_mode):
                continue
            if not entry.is_dir(follow_symlinks=False):
                raise RuntimeError(f'corpus contains a non-file artifact: {entry.name}')
            if CORPUS_REPLAY_CACHE.fullmatch(entry.name):
                with os.scandir(entry.path) as children:
                    for child in children:
                        check(not child.is_symlink() and child.is_file(follow_symlinks=False),
                              f'replay cache contains an unexpected entry: {entry.name}/{child.name}')
                        check(child.name == '.pigade-replay-owner'
                              or child.name == 'rows.index'
                              or re.fullmatch(r'\d{20}\.archive', child.name),
                              f'replay cache contains an unknown file: {entry.name}/{child.name}')
                continue
            match = re.fullmatch(r'([0-9a-f-]{36})\.history', entry.name)
            check(match is not None and match.group(1) in generations,
                  f'corpus contains an unexpected directory: {entry.name}')
            observed.add(entry.name)
            with os.scandir(entry.path) as children:
                for child in children:
                    check(not child.is_symlink() and child.is_file(follow_symlinks=False),
                          f'corpus history contains an unexpected entry: {entry.name}/{child.name}')
                    if child.name == 'rows.index' or re.fullmatch(r'\d{20}\.archive', child.name):
                        observed.add(f'{entry.name}/{child.name}')
                    else:
                        raise RuntimeError(f'corpus history contains an unexpected file: {entry.name}/{child.name}')

    for generation in generations:
        expected.add(f'{generation}.history')
        history_indexes = [name for name in observed if name == f'{generation}.history/rows.index']
        archives = [name for name in observed if name.startswith(f'{generation}.history/')
                    and name.endswith('.archive')]
        checkpoints = [name for name in observed if name.startswith(generation + '.')
                       and name.endswith('.checkpoint')]
        check(history_indexes and archives and checkpoints,
              f'corpus generation {generation} is missing index, archive, or checkpoint data')
        expected.update(checkpoints)
        expected.add(f'{generation}.history/rows.index')
        expected.update(archives)
    check(observed == expected, 'corpus file set does not match its four-session manifest')
    return sorted(observed - {name for name in observed if name.endswith('.history')})


def inventory_paths(directory: Path, paths: list[str]) -> dict[str, dict[str, int | str]]:
    inventory = {}
    for relative in paths:
        path = directory / relative
        check(not path.is_symlink() and path.is_file(), f'corpus source is missing or symlinked: {relative}')
        inventory[relative] = {'bytes': path.stat().st_size, 'sha256': sha256(path)}
    return inventory


def validate_corpus(corpus_arg: Path, reference_arg: Path, binary: Path,
                    allow_small: bool) -> dict:
    check(corpus_arg.is_absolute() and reference_arg.is_absolute(),
          '--corpus and --reference must be absolute paths')
    reject_symlink_components(corpus_arg, 'corpus')
    reject_symlink_components(reference_arg, 'reference')
    corpus = corpus_arg.resolve(strict=True)
    reference_path = reference_arg.resolve(strict=True)
    check(corpus.is_dir() and not corpus.is_symlink(), 'corpus must be a real directory')
    corpus_meta = corpus.stat()
    check(corpus_meta.st_uid == os.getuid() and not (corpus_meta.st_mode & 0o077),
          'corpus directory must be private and owned by the current user')
    check(not corpus.is_relative_to(ROOT.resolve()), 'corpus fixture must be outside the Git repository')
    runner_path = corpus.parent / 'runner.json'
    check(runner_path.is_file() and not runner_path.is_symlink(),
          'corpus runner.json is missing; only completed long-history.py output is accepted')
    runner = json.loads(runner_path.read_text())
    check(runner.get('status') == 'completed' and runner.get('exit_code') == 0,
          'corpus runner has not completed successfully')
    check(runner.get('verification_requested') is True,
          'corpus runner did not request full/live recovery verification')
    manifest_path = corpus / 'manifest.json'
    check(manifest_path.is_file() and not manifest_path.is_symlink(),
          'completed corpus manifest.json is missing')
    corpus_manifest = json.loads(manifest_path.read_text())
    check(corpus_manifest.get('kind') == 'production-model-journal-cold-history-corpus',
          'unexpected corpus format')
    check(corpus_manifest.get('sessions') == 4 and len(corpus_manifest.get('results', [])) == 4,
          'exactly four corpus sessions are required')
    check(corpus_manifest.get('logical_hours_metadata') == 24.0,
          'corpus must carry the nominal 24-hour event timeline')
    if not allow_small:
        check(corpus_manifest.get('requested_output_bytes_per_session') == 1024**3,
              'REC-08 mode requires exactly 1 GiB output per session')
        check(corpus_manifest.get('resize_events_per_session') == 100_000,
              'REC-08 mode requires exactly 100,000 resizes per session')
    check(corpus_manifest.get('core_profile') == current_core_profile(),
          'corpus terminal core profile does not match this repository')
    for row in corpus_manifest['results']:
        check(row.get('requested_output_bytes') == corpus_manifest['requested_output_bytes_per_session'],
              'session output byte count does not match corpus manifest')
        check(row.get('resize_events') == corpus_manifest['resize_events_per_session'],
              'session resize count does not match corpus manifest')
        check(row.get('cold_history_records', 0) > 0, 'corpus session has no cold history')
        proof = row.get('verification') or {}
        check(all(proof.get(key) is True for key in CORPUS_REQUIRED_PROOFS),
              f"session {row.get('generation')} lacks full/fast/live equality proofs")

    paths = corpus_whitelist(corpus, corpus_manifest)
    source_inventory = inventory_paths(corpus, paths)
    reference = json.loads(reference_path.read_text())
    check(reference.get('status') == 'completed' and reference.get('source_inventory_unchanged') is True,
          'recover-corpus reference must be completed with an unchanged source inventory')
    check(Path(reference.get('corpus', '')).resolve() == corpus,
          'recover-corpus reference points to a different corpus')
    check(reference.get('core_profile') == corpus_manifest.get('core_profile'),
          'reference and corpus terminal profiles differ')
    binary = binary.resolve(strict=True)
    binary_hash = sha256(binary)
    check(reference.get('binary_sha256') == binary_hash,
          'reference was produced with a different session-host binary')
    referenced_inventory = reference.get('source_inventory')
    check(isinstance(referenced_inventory, dict), 'reference has no source hash inventory')
    # recover-corpus inventories only source artifacts, excluding manifest.json.
    source_artifacts = {name: value for name, value in source_inventory.items() if name != 'manifest.json'}
    check(referenced_inventory == source_artifacts,
          'reference source hashes do not match the completed corpus')
    samples = reference.get('samples') or []
    check(samples, 'reference contains no recovered-frame sample')
    expected = {row['generation']: row for row in corpus_manifest['results']}
    reference_rows = samples[0].get('generations') or {}
    check(set(reference_rows) == set(expected), 'reference does not cover exactly the four generations')
    for generation, row in expected.items():
        reference_row = reference_rows[generation]
        check(reference_row.get('session') == row['session_id'], 'reference session identity mismatch')
        check(reference_row.get('sequence') == row['final_sequence'], 'reference sequence mismatch')
        check(re.fullmatch(r'[0-9a-f]{64}', reference_row.get('frame_sha256', '')) is not None,
              'reference frame hash is missing or malformed')
        check(reference_row.get('history', {}).get('records') == row['cold_history_records'],
              'reference cold-history count mismatch')
        check(reference_row.get('journal_bytes_read', 0) < row['journal_bytes'],
              'reference did not use a bounded checkpoint-tail read')
        tail = reference_row.get('canonical_tail')
        check(isinstance(tail, dict), 'reference lacks the canonical checkpoint-tail oracle')
        tail_records = tail.get('records')
        check(isinstance(tail_records, list) and len(tail_records) <= 1024,
              'reference canonical tail is missing or exceeds the 1024-record bound')
        check(tail.get('snapshot_records') == reference_row['history']['records'],
              'reference canonical-tail count differs from cold history')
        prefix = tail.get('recovery_prefix_records')
        check(isinstance(prefix, int) and 0 <= prefix <= tail['snapshot_records'],
              'reference canonical-tail recovery prefix is invalid')
        check(len(tail_records) == tail['snapshot_records'] - prefix,
              'reference canonical tail does not cover every checkpoint-tail record')
        total_tail_bytes = 0
        for ordinal, record in enumerate(tail_records, start=prefix):
            check(record.get('ordinal') == ordinal and isinstance(record.get('source_event_sequence'), int),
                  'reference canonical-tail record identity is invalid')
            value = record.get('bytes_hex', '')
            check(isinstance(value, str) and len(value) % 2 == 0
                  and re.fullmatch(r'[0-9a-f]*', value) is not None,
                  'reference canonical-tail record bytes are malformed')
            record_bytes = len(value) // 2
            check(record.get('record_bytes') == record_bytes,
                  'reference canonical-tail record length mismatch')
            total_tail_bytes += record_bytes
        check(total_tail_bytes <= 2 * 1024**2,
              'reference canonical tail exceeds the 2 MiB bound')
    return {
        'corpus_path': str(corpus), 'reference_path': str(reference_path),
        'reference_sha256': sha256(reference_path), 'binary_sha256': binary_hash,
        'core_profile': corpus_manifest['core_profile'], 'manifest': corpus_manifest,
        'reference': reference, 'source_inventory': source_inventory,
        'source_bytes': sum(item['bytes'] for item in source_inventory.values()),
        'files': paths,
    }


def copy_reflink_fixture(source: Path, destination: Path, expected: dict) -> dict:
    destination.mkdir(mode=0o700)
    before = inventory_paths(source, expected['files'])
    check(before == expected['source_inventory'], 'corpus changed after validation')
    for relative in expected['files']:
        source_file = source / relative
        copied = destination / relative
        check(not source_file.is_symlink() and source_file.is_file(),
              f'corpus source changed type during fixture copy: {relative}')
        copied.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        run(['cp', '-a', '--reflink=always', '--no-preserve=ownership',
             str(source_file), str(copied)])
        check(copied.is_file() and not copied.is_symlink(),
              f'private fixture copy is not a regular file: {relative}')
        copied.chmod(0o444)
    for root, dirs, _ in os.walk(destination, topdown=False):
        Path(root).chmod(0o555)
    copied_inventory = inventory_paths(destination, expected['files'])
    check(copied_inventory == expected['source_inventory'],
          'private reflink fixture differs from its source inventory')
    check(inventory_paths(source, expected['files']) == expected['source_inventory'],
          'corpus source changed during private fixture copy')
    return copied_inventory


def run(command: list[str], *, env: dict[str, str] | None = None,
        timeout: int = 120) -> str:
    result = subprocess.run(command, text=True, capture_output=True,
                            stdin=subprocess.DEVNULL, env=env, timeout=timeout, check=False)
    if result.returncode:
        raise RuntimeError(f'{Path(command[0]).name} failed ({result.returncode}): '
                           f'{result.stdout[-2000:]}{result.stderr[-3000:]}')
    return result.stdout + result.stderr


def completed_serial_payload(serial_text: str, marker: str) -> str | None:
    """Return a result payload only after its complete serial line is written."""
    for line in serial_text.splitlines(keepends=True):
        if not line.endswith('\n') or marker not in line:
            continue
        return line.split(marker, 1)[1].rstrip('\r\n')
    return None


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


def sdk_tool_environment(sdk: dict[str, Path]) -> dict[str, str]:
    # The SDK is extracted privately, not installed in the system linker path.
    # Do not inherit loader overrides from the invoking desktop environment.
    prefix = sdk['qemu_system_x86_64'].parent.parent
    return {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8',
            'LD_LIBRARY_PATH': f'{prefix}/lib:{prefix}/lib/qemu'}


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
CORPUS_MODE = __CORPUS_MODE__
DATA_SERIAL = __DATA_SERIAL__
CORPUS_VALIDATION = pathlib.Path('/mnt/pigade-stage/corpus-validation.json')
CORPUS_SOURCE = pathlib.Path('/mnt/pigade-corpus')
CORPUS_DATA = pathlib.Path('/mnt/pigade-corpus-data')
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

def ipc(command, allow_error=False, endpoint=None):
    data = json.dumps({'protocol': PROTOCOL, 'command': command}, separators=(',', ':')).encode()
    ensure(len(data) <= 8 * 1024 * 1024, 'IPC request too large')
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(15); conn.connect(str(endpoint or SOCKET))
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

def start_host(state=STATE, runtime=None):
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    env = dict(os.environ); env['LD_LIBRARY_PATH'] = '/opt/pigade-runtime/lib'
    env['HOME'] = '/root'; env['XDG_CONFIG_HOME'] = '/root/.config'
    command = ['/usr/local/bin/pigade-session-host', 'serve', str(state)]
    if runtime is not None:
        runtime.mkdir(mode=0o700, parents=True, exist_ok=True)
        command.extend(['--runtime', str(runtime)])
    endpoint = runtime / 'host.sock' if runtime is not None else SOCKET
    with (ROOT / 'host.log').open('ab', buffering=0) as log:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                   env=env, start_new_session=True, close_fds=True)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        try: ipc({'command':'list'}, endpoint=endpoint); return process
        except (OSError, RuntimeError): time.sleep(.1)
    raise RuntimeError('host did not become ready within 20 seconds')

def corpus_config():
    ensure(CORPUS_VALIDATION.is_file(), 'corpus validation sidecar is missing')
    value = json.loads(CORPUS_VALIDATION.read_text())
    ensure(value.get('schema') == 1, 'unsupported corpus validation sidecar')
    reference_path = pathlib.Path('/mnt/pigade-stage/corpus-reference.json')
    digest = hashlib.sha256(reference_path.read_bytes()).hexdigest()
    ensure(digest == value.get('reference_sha256'), 'staged recovery reference hash changed')
    return value

def disk_serial(block):
    resolved = block.resolve()
    for candidate in (block / 'device' / 'serial', block / 'serial',
                      resolved / 'serial', resolved.parent / 'serial'):
        try:
            value = candidate.read_text().strip()
        except (OSError, IsADirectoryError):
            continue
        if value: return value
    return None

def corpus_block_device():
    matches = []
    for block in pathlib.Path('/sys/class/block').iterdir():
        # A partition can resolve through its parent to the same device serial.
        # Only whole disks are eligible for the one-time filesystem creation.
        if (block / 'partition').exists():
            continue
        if disk_serial(block) == DATA_SERIAL:
            matches.append(pathlib.Path('/dev') / block.name)
    ensure(len(matches) == 1, 'unique corpus data-disk serial was not found')
    return matches[0]

def data_fs(device):
    with device.open('rb', buffering=0) as source:
        source.seek(1024)
        superblock = source.read(1024)
    if len(superblock) != 1024 or int.from_bytes(superblock[56:58], 'little') != 0xef53:
        return None, None
    label = superblock[120:136].split(b'\0', 1)[0].decode('ascii', errors='strict')
    return 'ext4', label

def blank_data_device(device, size_bytes):
    with device.open('rb', buffering=0) as source:
        first = source.read(4096)
        source.seek(size_bytes - 4096)
        last = source.read(4096)
    return first == bytes(4096) and last == bytes(4096)

def ensure_data_disk():
    device = corpus_block_device()
    block = pathlib.Path('/sys/class/block') / device.name
    size_bytes = int((block / 'size').read_text().strip()) * 512
    ensure(size_bytes >= 64 * 1024**3, 'corpus data disk is smaller than 64 GiB')
    if not PHASE.exists():
        fs_type, label = data_fs(device)
        if fs_type is None:
            ensure(device == pathlib.Path('/dev/vda'),
                   'refusing to format anything except the uniquely identified corpus data device')
            ensure(not os.path.ismount(CORPUS_DATA), 'refusing to format a mounted corpus device')
            ensure(blank_data_device(device, size_bytes),
                   'new corpus data disk is not blank; refusing to format it')
            result = subprocess.run(['mkfs.ext4', '-F', '-m', '0', '-L', 'PIGADE_CORPUS', str(device)],
                                    text=True, capture_output=True, check=False)
            ensure(result.returncode == 0, 'could not format the serial-verified new corpus data disk')
            fs_type, label = data_fs(device)
        ensure(fs_type == 'ext4' and label == 'PIGADE_CORPUS',
               'corpus data disk filesystem label/type mismatch')
    else:
        fs_type, label = data_fs(device)
        ensure(fs_type == 'ext4' and label == 'PIGADE_CORPUS',
               'refusing to format or use an unexpected persistent corpus data disk')
    CORPUS_DATA.mkdir(mode=0o700, parents=True, exist_ok=True)
    if not os.path.ismount(CORPUS_DATA):
        subprocess.run(['mount', '-t', 'ext4', '-o', 'noatime', str(device), str(CORPUS_DATA)],
                       check=True)
    ensure(os.path.ismount(CORPUS_DATA), 'corpus data disk did not mount')
    return device

def canonical_inventory_hash(inventory):
    body = json.dumps(inventory, sort_keys=True, separators=(',', ':')).encode()
    return hashlib.sha256(body).hexdigest()

def verify_inventory(root, inventory):
    observed = {}
    for relative, expected in inventory.items():
        path = root / relative
        ensure(not path.is_symlink() and path.is_file(), 'fixture file missing/symlinked: ' + relative)
        size = path.stat().st_size
        digest = hashlib.sha256()
        with path.open('rb') as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b''):
                digest.update(chunk)
        observed[relative] = {'bytes':size, 'sha256':digest.hexdigest()}
        ensure(observed[relative] == expected, 'fixture hash mismatch: ' + relative)
    return observed

def copy_corpus_to_data_disk(config):
    inventory = config['source_inventory']
    digest = canonical_inventory_hash(inventory)
    target = CORPUS_DATA / 'corpus'
    partial = CORPUS_DATA / 'corpus.partial'
    marker = CORPUS_DATA / 'corpus-copy.json'
    if marker.exists():
        metadata = json.loads(marker.read_text())
        ensure(metadata.get('inventory_sha256') == digest, 'persistent fixture inventory marker changed')
        ensure(target.is_dir() and not partial.exists(), 'persistent fixture copy state is inconsistent')
        verify_inventory(target, inventory)
        return target, digest
    ensure(not partial.exists(), 'incomplete prior corpus copy retained; refusing destructive cleanup')
    if target.exists():
        verify_inventory(target, inventory)
    else:
        ensure(CORPUS_SOURCE.is_dir() and os.path.ismount(CORPUS_SOURCE),
               'read-only staged corpus fixture is not mounted')
        partial.mkdir(mode=0o700)
        for relative, expected in inventory.items():
            source = CORPUS_SOURCE / relative
            ensure(not source.is_symlink() and source.is_file(),
                   'staged fixture file missing/symlinked: ' + relative)
            output = partial / relative
            output.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
            digest_file = hashlib.sha256()
            size = 0
            with source.open('rb') as input_file, output.open('xb') as output_file:
                for chunk in iter(lambda: input_file.read(1024 * 1024), b''):
                    output_file.write(chunk); digest_file.update(chunk); size += len(chunk)
                output_file.flush(); os.fsync(output_file.fileno())
            ensure({'bytes':size, 'sha256':digest_file.hexdigest()} == expected,
                   'source changed while copying fixture: ' + relative)
            output.chmod(0o444)
        for root, dirs, _ in os.walk(partial, topdown=False):
            for name in dirs:
                (pathlib.Path(root) / name).chmod(0o555)
            if pathlib.Path(root) != partial:
                pathlib.Path(root).chmod(0o555)
            fd = os.open(root, os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0))
            try: os.fsync(fd)
            finally: os.close(fd)
        os.replace(partial, target)
        fd = os.open(str(CORPUS_DATA), os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0))
        try: os.fsync(fd)
        finally: os.close(fd)
    durable_json(marker, {'schema':1, 'inventory_sha256':digest,
                          'files':len(inventory), 'source_bytes':sum(x['bytes'] for x in inventory.values())})
    return target, digest

def corpus_views(host_state, expected_rows):
    started = time.monotonic()
    runtime = pathlib.Path('/run/pigade-vm-runtime')
    process = start_host(host_state, runtime)
    endpoint = runtime / 'host.sock'
    views = ipc({'command':'list'}, endpoint=endpoint).get('views', [])
    elapsed_ms = (time.monotonic() - started) * 1000
    ensure(len(views) == 4, 'host did not recover exactly four corpus sessions')
    by_generation = {view.get('generation'):view for view in views}
    ensure(set(by_generation) == set(expected_rows), 'host exposed unexpected corpus generations')
    observed = []
    for generation, expected in expected_rows.items():
        view = by_generation[generation]
        ensure(view.get('session') == expected['session_id'], 'corpus session identity changed')
        ensure(view.get('sequence') == expected['final_sequence'], 'corpus sequence changed')
        ensure(view.get('state') == 'history' and view.get('pid') is None,
               'corpus generation was not recovered as a stopped historical session')
        ensure(view.get('storage_error') is None, 'corpus recovery reported a storage error')
        ensure(view.get('recovery_path') == 'checkpoint-tail', 'corpus recovery missed checkpoint-tail')
        ensure(view.get('recovery_journal_bytes_read') == expected['journal_bytes_read'],
               'checkpoint-tail byte count differs from the host reference')
        ensure(view['recovery_journal_bytes_read'] < expected['journal_bytes'],
               'checkpoint-tail recovery read the entire source journal')
        ensure(view.get('history', {}).get('records') == expected['cold_history_records'],
               'corpus cold-history record count changed')
        digest = frame_hash(view)
        ensure(digest == expected['frame_sha256'], 'corpus frame differs from the host reference')
        ensure(view.get('history') == expected['history'], 'corpus history summary differs from host reference')
        tail = expected['canonical_tail']
        count = view['history']['records']
        query = {'session':view['session'], 'generation':generation,
                 'expected_sequence':view['sequence'], 'ordinal':0, 'offset':0, 'max_bytes':1}

        def read_chunk():
            reply = ipc({'command':'history_read', 'query':query}, endpoint=endpoint)
            ensure(not reply.get('views'), 'history read unexpectedly returned terminal frames')
            chunk = reply.get('history_chunk')
            ensure(isinstance(chunk, dict), 'history_read did not return a canonical chunk')
            ensure(chunk.get('session_id') == view['session']
                   and chunk.get('generation') == generation
                   and chunk.get('profile') == tail['profile']
                   and chunk.get('history_sequence') == view['sequence']
                   and chunk.get('snapshot_records') == count
                   and chunk.get('ordinal') == query['ordinal']
                   and chunk.get('offset') == query['offset'],
                   'canonical-tail chunk identity/watermark mismatch')
            ensure(chunk.get('source_event_sequence', view['sequence'] + 1) <= view['sequence'],
                   'canonical-tail source event sequence is ahead of the recovered model')
            ensure(len(chunk.get('bytes', [])) <= query['max_bytes'],
                   'canonical-tail API exceeded the requested byte limit')
            return chunk

        first = read_chunk()
        prefix = first.get('recovery_prefix_records')
        ensure(prefix == tail['recovery_prefix_records'] and 0 <= prefix <= count,
               'canonical-tail recovery prefix differs from the host reference')
        ensure(count - prefix == len(tail['records']),
               'canonical-tail API record range differs from the host reference')
        for record in tail['records']:
            ordinal = record['ordinal']
            query.update(ordinal=ordinal, offset=0, max_bytes=65536)
            actual = bytearray()
            while True:
                chunk = read_chunk()
                ensure(chunk.get('recovery_prefix_records') == prefix,
                       'canonical-tail recovery prefix changed during read')
                part = bytes(chunk.get('bytes', []))
                ensure(record['record_bytes'] == chunk.get('record_bytes'),
                       'canonical-tail record length differs from reference')
                ensure(chunk.get('source_event_sequence') == record['source_event_sequence'],
                       'canonical-tail event sequence differs from reference')
                actual.extend(part)
                ensure(len(actual) <= record['record_bytes'], 'canonical-tail record exceeded declared length')
                next_offset = chunk.get('next_offset')
                if next_offset is None:
                    break
                ensure(next_offset == len(actual) and part,
                       'canonical-tail chunk offsets are not contiguous')
                query['offset'] = next_offset
            ensure(len(actual) == record['record_bytes'], 'canonical-tail record ended at wrong length')
            ensure(actual == bytes.fromhex(record['bytes_hex']),
                   'canonical-tail record bytes differ from host reference')
        observed.append({'session':view['session'], 'generation':generation,
                         'sequence':view['sequence'], 'frame_sha256':digest,
                         'history':view['history'], 'recovery_path':view['recovery_path'],
                         'journal_bytes_read':view['recovery_journal_bytes_read'],
                         'canonical_tail_records_verified':len(tail['records']),
                         'pid':view.get('pid'), 'state':view['state']})
    rss = int(pathlib.Path('/proc', str(process.pid), 'statm').read_text().split()[1]) * os.sysconf('SC_PAGE_SIZE')
    return {'four_frames_ms':elapsed_ms, 'host_rss_bytes':rss, 'sessions':observed}

def corpus_expected(config):
    manifest = config['corpus_manifest']
    reference = config['reference']
    reference_rows = reference['samples'][0]['generations']
    expected = {}
    for row in manifest['results']:
        expected[row['generation']] = {
            'session_id':row['session_id'], 'final_sequence':row['final_sequence'],
            'journal_bytes':row['journal_bytes'], 'cold_history_records':row['cold_history_records'],
            **reference_rows[row['generation']],
        }
    return expected

def first_boot_corpus(boot_id):
    config = corpus_config()
    device = ensure_data_disk()
    state, inventory_digest = copy_corpus_to_data_disk(config)
    reference_hash = config['reference_sha256']
    views = corpus_views(state, corpus_expected(config))
    durable_json(PHASE, {'nonce':NONCE, 'mode':'corpus', 'phase':'first_boot_complete',
                         'boot_id':boot_id, 'data_device':str(device),
                         'inventory_sha256':inventory_digest, 'reference_sha256':reference_hash,
                         'recovery':views})
    print('PIGADE_FIRST_BOOT:' + boot_id + ':4:corpus', flush=True)
    os.sync()
    subprocess.Popen(['/sbin/reboot', '-f'], stdin=subprocess.DEVNULL,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def second_boot_corpus(boot_id):
    phase = json.loads(PHASE.read_text())
    ensure(phase.get('nonce') == NONCE and phase.get('mode') == 'corpus'
           and phase.get('phase') == 'first_boot_complete', 'invalid durable corpus phase record')
    ensure(phase.get('boot_id') != boot_id, 'guest boot_id did not change after real reboot')
    config = corpus_config()
    ensure_data_disk()  # Existing ext4 label is required; this path never formats.
    state, inventory_digest = copy_corpus_to_data_disk(config)
    ensure(inventory_digest == phase.get('inventory_sha256'), 'corpus inventory changed across reboot')
    ensure(config['reference_sha256'] == phase.get('reference_sha256'), 'reference sidecar changed across reboot')
    views = corpus_views(state, corpus_expected(config))
    ensure(views['sessions'] == phase['recovery']['sessions'],
           'four historical frames changed across actual guest reboot')
    summary = {'nonce':NONCE, 'first_boot_id':phase['boot_id'], 'second_boot_id':boot_id,
               'mode':'corpus', 'inventory_sha256':inventory_digest,
               'reference_sha256':config['reference_sha256'], 'first_boot_recovery':phase['recovery'],
               'second_boot_recovery':views,
               'scope':'completed prebuilt corpus copied to guest ext4; actual guest reboot; checkpoint-tail frames matched same-binary host reference; no provider, layout, real-time soak, in-VM full cold oracle, or REC-08 p95 claim',
               'full_cold_oracle_in_vm':False, 'REC08_acceptance':False,
               'status':'recovery_verified'}
    durable_json(RESULT, summary)
    marker = json.dumps(summary, sort_keys=True, separators=(',', ':'))
    print('PIGADE_VM_RESULT:' + NONCE + ':' + marker, flush=True)

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
    if CORPUS_MODE:
        if not PHASE.exists(): first_boot_corpus(boot_id)
        else: second_boot_corpus(boot_id)
    elif not PHASE.exists(): first_boot(boot_id)
    else: second_boot(boot_id)

if __name__ == '__main__':
    try:
        main()
    except Exception as exc:
        print('PIGADE_VM_FAILURE:' + NONCE + ':' + repr(exc), flush=True)
        raise
'''


def user_data(nonce: str, *, corpus_mode: bool = False, data_serial: str = '') -> str:
    scenario = (GUEST_SCENARIO.replace('__NONCE__', repr(nonce))
                .replace('__CORPUS_MODE__', repr(corpus_mode))
                .replace('__DATA_SERIAL__', repr(data_serial)))
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
    if corpus_mode:
        service = service.replace(
            'mkdir -p /mnt/pigade-stage /opt/pigade-runtime/lib /var/lib/pigade-vm',
            'mkdir -p /mnt/pigade-stage /mnt/pigade-corpus /opt/pigade-runtime/lib /var/lib/pigade-vm')
        service = service.replace(
            'test -r /mnt/pigade-stage/pigade-session-host || return 1',
            'mountpoint -q /mnt/pigade-corpus || mount -t 9p -o trans=virtio,version=9p2000.L,ro pigade_corpus /mnt/pigade-corpus\n'
            '    mountpoint -q /mnt/pigade-corpus || return 1\n'
            '    test -r /mnt/pigade-stage/corpus-validation.json || return 1\n'
            '    test -r /mnt/pigade-corpus/manifest.json || return 1\n'
            '    test -r /mnt/pigade-stage/pigade-session-host || return 1')
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


def create_seed_from_files(sdk: dict[str, Path], run_dir: Path,
                           files: dict[str, Path]) -> Path:
    seed = run_dir / 'nocloud-seed.img'
    with seed.open('wb') as output:
        output.truncate(16 * 1024 * 1024)
        output.flush()
        os.fsync(output.fileno())
    run([str(sdk['mformat']), '-i', str(seed), '-v', 'CIDATA', '-F', '::'])
    for name, path in files.items():
        run([str(sdk['mcopy']), '-i', str(seed), str(path), '::' + name])
    return seed


def create_seed(sdk: dict[str, Path], run_dir: Path, nonce: str,
                *, corpus_mode: bool = False, data_serial: str = '') -> Path:
    files = {
        'user-data': run_dir / 'user-data',
        'meta-data': run_dir / 'meta-data',
        'network-config': run_dir / 'network-config',
    }
    files['user-data'].write_text(user_data(nonce, corpus_mode=corpus_mode, data_serial=data_serial))
    files['meta-data'].write_text('instance-id: iid-pigade-' + nonce + '\nlocal-hostname: pigade-vm-recovery\n')
    files['network-config'].write_text('version: 2\nethernets: {}\n')
    return create_seed_from_files(sdk, run_dir, files)


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


def build_qemu_command(qemu: Path, sdk: dict[str, Path], run_dir: Path,
                       manifest: dict, qmp: Path) -> list[str]:
    command = [str(qemu), '-name','pigade-vm-recovery', '-machine','pc', '-accel','kvm',
               '-L',str(sdk['qemu_data_dir']), '-cpu','host', '-smp','2', '-m','1024M',
               '-bios',str(sdk['seabios_bios_256k']),
               '-smbios','type=1,manufacturer=Amazon,serial=ds=nocloud',
               '-nodefaults', '-no-user-config', '-display','none', '-monitor','none',
               '-serial','file:'+manifest['artifacts']['serial_log'], '-parallel','none',
               '-qmp',f'unix:{qmp},server=on,wait=off',
               '-drive',f"file={manifest['artifacts']['disk']},format=qcow2,if=ide,index=0",
               '-drive',f"file={manifest['artifacts']['seed']},format=raw,if=ide,index=1",
               '-virtfs',f"local,path={manifest['artifacts']['stage']},mount_tag=pigade_stage,security_model=none,readonly=on"]
    data_disk = manifest['artifacts'].get('corpus_data_disk')
    fixture = manifest['artifacts'].get('corpus_fixture')
    if data_disk:
        serial = manifest.get('data_disk', {}).get('device_serial', '')
        check(re.fullmatch(r'pgd-data-[0-9a-f]{8}', serial) is not None,
              'corpus data disk has no unique device serial')
        check(fixture is not None, 'corpus data disk requires a staged fixture')
        command.extend(['-drive',f'file={data_disk},format=qcow2,if=none,id=pigade-corpus-data',
                        '-device',f'virtio-blk-pci,drive=pigade-corpus-data,serial={serial}',
                        '-virtfs',f'local,path={fixture},mount_tag=pigade_corpus,security_model=none,readonly=on'])
    command.extend(['-nic','none', '-boot','order=c'])
    return command


def prepare(args) -> tuple[Path, dict]:
    sdk = sdk_paths(args.sdk_manifest)
    binary = args.binary.resolve(strict=True)
    corpus = None
    if args.corpus is not None:
        check(args.reference is not None,
              '--corpus requires --reference from recover-corpus.py')
        corpus = validate_corpus(args.corpus, args.reference, binary, args.allow_small_corpus)
        sdk['qemu_img'] = sdk['qemu_system_x86_64'].with_name('qemu-img')
        check(sdk['qemu_img'].is_file(), 'pinned QEMU SDK qemu-img tool is missing')
    else:
        check(args.reference is None and not args.allow_small_corpus,
              '--reference and --allow-small-corpus require --corpus')
    directory = private_dir(args.directory)
    if corpus:
        free = shutil.disk_usage(directory).free
        required = corpus['source_bytes'] * 2 + CORPUS_HOST_HEADROOM_BYTES
        check(free >= required,
              f'insufficient host space for corpus fixture and guest copy: need {required}, have {free}')
    fs = subprocess.check_output(['stat', '-f', '-c', '%T', str(directory)], text=True).strip()
    if corpus:
        check(not directory.is_relative_to(ROOT.resolve()),
              'corpus VM artifacts must be outside the Git repository')
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
    fixture = None
    data_disk = None
    qemu_img_version = None
    nonce = uuid.uuid4().hex
    data_disk_serial = corpus_disk_serial(nonce) if corpus else None
    if corpus:
        fixture = run_dir / 'corpus-source'
        fixture_inventory = copy_reflink_fixture(Path(corpus['corpus_path']), fixture, corpus)
        reference_copy = staging / 'corpus-reference.json'
        shutil.copyfile(corpus['reference_path'], reference_copy)
        reference_copy.chmod(0o444)
        (staging / 'corpus-validation.json').write_text(json.dumps({
            'schema':1, 'source_inventory':fixture_inventory,
            'corpus_manifest':corpus['manifest'], 'reference':corpus['reference'],
            'reference_sha256':corpus['reference_sha256'],
        }, sort_keys=True, separators=(',', ':')) + '\n')
        (staging / 'corpus-validation.json').chmod(0o444)
        data_disk = run_dir / 'corpus-data.qcow2'
        image_tool_env = sdk_tool_environment(sdk)
        qemu_img_version = run([str(sdk['qemu_img']), '--version'], env=image_tool_env).splitlines()[0]
        check('11.1.2' in qemu_img_version,
              f'unexpected pinned qemu-img version: {qemu_img_version}')
        run([str(sdk['qemu_img']), 'create', '-f', 'qcow2', str(data_disk), str(CORPUS_DISK_BYTES)],
            env=image_tool_env)
        data_disk.chmod(0o600)
        with data_disk.open('rb') as disk_file: os.fsync(disk_file.fileno())
        disk_info = json.loads(run([str(sdk['qemu_img']), 'info', '--output=json', str(data_disk)],
                                   env=image_tool_env))
        check(disk_info.get('format') == 'qcow2'
              and disk_info.get('virtual-size') == CORPUS_DISK_BYTES,
              'secondary corpus disk capacity/format did not match the 64 GiB requirement')
    disk = run_dir / 'guest-disk.qcow2'
    shutil.copyfile(image, disk)
    os.chmod(disk, 0o600)
    with disk.open('rb') as disk_file: os.fsync(disk_file.fileno())
    seed = create_seed(sdk, run_dir, nonce, corpus_mode=corpus is not None,
                       data_serial=data_disk_serial or '')
    bios = sdk['seabios_bios_256k']
    manifest = {
        'schema':1, 'status':'prepared', 'created_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'nonce':nonce, 'directory':str(run_dir), 'filesystem':fs,
        'image':{'url':IMAGE_URL, 'filename':IMAGE, 'size_bytes':image.stat().st_size,
                 'sha512':sha512(image), 'official_signature_valid':True,
                 'signer_fingerprint':ALPINE_KEY_FINGERPRINT},
        'binary':runtime, 'sdk_manifest':str(args.sdk_manifest.resolve()),
        'sdk':{'qemu':str(sdk['qemu_system_x86_64']), 'qemu_data_dir':str(sdk['qemu_data_dir']),
               'bios':str(bios), 'mcopy':str(sdk['mcopy']), 'mformat':str(sdk['mformat']),
               'qemu_img':str(sdk['qemu_img']) if corpus else None,
               'qemu_img_version':qemu_img_version},
        'artifacts':{'disk':str(disk), 'seed':str(seed), 'stage':str(staging),
                     'corpus_fixture':str(fixture) if fixture else None,
                     'corpus_data_disk':str(data_disk) if data_disk else None,
                     'serial_log':str(run_dir/'serial.log'), 'qemu_log':str(run_dir/'qemu.log'),
                     'qmp_socket':None, 'qmp_socket_directory':None},
        'data_disk':({'virtual_size_bytes':CORPUS_DISK_BYTES, 'format':'qcow2',
                      'device_serial':data_disk_serial, 'host_space_preflight_bytes':
                      corpus['source_bytes'] * 2 + CORPUS_HOST_HEADROOM_BYTES,
                      'guest_state':'full writable corpus copy and recovery-derived metadata'}
                     if corpus else None),
        'corpus':({'source':corpus['corpus_path'], 'reference':corpus['reference_path'],
                   'reference_sha256':corpus['reference_sha256'], 'core_profile':corpus['core_profile'],
                   'source_bytes':corpus['source_bytes'], 'fixture_files':len(fixture_inventory),
                   'source_inventory':corpus['source_inventory'], 'fixture_inventory':fixture_inventory,
                   'small_diagnostic':args.allow_small_corpus}
                  if corpus else None),
        'isolation':{'network':'none', 'display':'none', 'input_devices':'none',
                     'host_mounts':[str(staging)] + ([str(fixture)] if fixture else []),
                     'host_mounts_readonly':True,
                     'state':'owned guest disk' if not corpus else 'owned guest root plus private 64 GiB corpus disk'},
        'scope':('actual guest reboot and synthetic PTY recovery; not provider AI resume or power-loss acceptance'
                 if not corpus else
                 'completed corpus copied to guest ext4 and recovered across an actual guest reboot; '
                 'not provider/layout/real-time soak/in-VM full cold oracle/REC-08 p95/acceptance'),
        'REC08_acceptance':False,
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
    command = build_qemu_command(qemu, sdk, run_dir, manifest, qmp)
    check(hasattr(os, 'pidfd_open') and hasattr(signal, 'pidfd_send_signal'),
          'Linux pidfd support is required for exact owned QEMU cleanup')
    with (run_dir/'qemu.log').open('ab', buffering=0) as log:
        qemu_process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                        env=env, close_fds=True, start_new_session=True)
        pidfd = None
        try:
            # The unreaped Popen child cannot have its PID reused before this
            # open. All later cleanup signals use this exact kernel identity.
            pidfd = os.pidfd_open(qemu_process.pid)
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
            while time.monotonic() < deadline:
                if qemu_process.poll() is not None:
                    raise RuntimeError(f'QEMU exited early with {qemu_process.returncode}; inspect {run_dir}/qemu.log')
                text = serial.read_text(errors='replace') if serial.exists() else ''
                if failure in text:
                    raise RuntimeError(text.split(failure, 1)[1][-2000:])
                payload = completed_serial_payload(text, marker)
                if payload is not None:
                    result = json.loads(payload)
                    check(result.get('status') == 'recovery_verified', 'guest did not report recovery_verified')
                    if manifest.get('corpus'):
                        check(result.get('mode') == 'corpus' and result.get('REC08_acceptance') is False
                              and result.get('full_cold_oracle_in_vm') is False,
                              'corpus result scope or REC-08 limitation marker is missing')
                        check(len(result.get('second_boot_recovery', {}).get('sessions', [])) == 4,
                              'corpus result does not contain four second-boot sessions')
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
                    session_count = (len(result['second_boot_recovery']['sessions'])
                                     if result.get('mode') == 'corpus' else len(result['sessions']))
                    print(json.dumps({'status':manifest['status'], 'run_directory':str(run_dir),
                                      'guest_boot_ids':manifest['guest_boot_ids'],
                                      'sessions':session_count}, sort_keys=True), flush=True)
                    return 0
                if qmp.exists():
                    try: qmp_status = qmp_query(qmp, 'query-status')['result']
                    except (OSError, RuntimeError, ValueError, json.JSONDecodeError): pass
                time.sleep(.5)
            raise RuntimeError(f'guest result deadline ({args.timeout}s) expired; inspect {serial} and {run_dir}/qemu.log')
        finally:
            if qemu_process.poll() is None:
                try:
                    if pidfd is not None:
                        signal.pidfd_send_signal(pidfd, signal.SIGTERM)
                        try:
                            qemu_process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                            qemu_process.wait(timeout=5)
                except (OSError, ProcessLookupError):
                    pass
            if pidfd is not None:
                os.close(pidfd)
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
            result = run_vm_with_qmp(run_dir, manifest, args, qmp)
            if result == 0 and manifest.get('corpus'):
                fixture = Path(manifest['artifacts']['corpus_fixture'])
                paths = sorted(manifest['corpus']['fixture_inventory'])
                after = inventory_paths(fixture, paths)
                check(after == manifest['corpus']['fixture_inventory'],
                      'read-only staged corpus fixture changed during guest run')
                manifest['corpus']['fixture_unchanged_after_guest'] = True
                manifest['corpus']['fixture_inventory_after'] = after
                (run_dir/'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
            return result
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
    parser.add_argument('--corpus', type=Path, help='completed long-history corpus inner directory')
    parser.add_argument('--reference', type=Path, help='completed recover-corpus result.json for the same host binary')
    parser.add_argument('--allow-small-corpus', action='store_true',
                        help='run only a non-REC-08 diagnostic fixture with four verified sessions')
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
