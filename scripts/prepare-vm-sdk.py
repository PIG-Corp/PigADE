#!/usr/bin/env python3
"""Prepare a private, signed Arch QEMU SDK for headless VM tests.

This downloads only pinned Arch packages and their detached signatures, checks
SHA-256 and Arch package signatures, extracts payloads into an owned 0700 tree,
and records the host-library closure. It never installs packages or starts a
VM, display, listener, or network service. Guest images are intentionally not
included.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request


DEFAULT_DIRECTORY = Path('/tmp/pigade-vm-sdk')
KEYRING_SOURCE = Path('/usr/share/pacman/keyrings/archlinux.gpg')
MIRROR = 'https://geo.mirror.pkgbuild.com'
MAX_PACKAGE_BYTES = 50 * 1024 * 1024
MAX_SIGNATURE_BYTES = 1024 * 1024
MAX_TOTAL_BYTES = 90 * 1024 * 1024
SIGNER_DAVID_RUNGE = '991F6E3F0765CF6295888586139B09DA5BF0D338'
SIGNER_FREDERIK_SCHWAN = '05C7775A9E8B977407FE08E69D4C5AA15426DA0A'
SIGNER_LEVENTE_POLYAK = 'E240B57E2C4630BA768E2F26FC1B547C8D8172C8'
SIGNER_JAKUB_KLINKOVSKY = '109415E692007609CA7EBFE4001CF4810BE8D911'
SIGNER_CHRISTIAN_HEUSEL = 'F00B96D15228013FFC9C9D0393B11DAA4C197E3D'
SIGNER_FELIX_YAN = 'B5971F2C5C10A9A08C60030F786C63F330D7CB92'
SIGNER_GEORGE_RAWLINSON = '034D823DA2055BEE6A6BF0BB25EA6900D9EA5EBC'


PACKAGES = (
    {
        'name': 'qemu-system-x86', 'version': '11.1.2-1', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': '4207178fd53c7cb134d2d11639b873fdd3f8cc5d06aec67816d842a549752a1e',
        'signer': SIGNER_DAVID_RUNGE,
    },
    {
        'name': 'qemu-common', 'version': '11.1.2-1', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': 'bd78128c06b0686b360fee1d46af3bd4f7e19a346323191aed1fd6c78a296183',
        'signer': SIGNER_DAVID_RUNGE,
    },
    {
        'name': 'qemu-system-x86-firmware', 'version': '11.1.2-1', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': '556b9a55f94da79b64972340396f7d631ddc3b0feed53d3a5773cb0aa5652ccf',
        'signer': SIGNER_DAVID_RUNGE,
    },
    {
        'name': 'libslirp', 'version': '4.9.5-1', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': 'd9584c21e1556ae02650ccbd769e838d7aec286b0b092e6b43aac10e3bade240',
        'signer': SIGNER_FREDERIK_SCHWAN,
    },
    {
        'name': 'capstone', 'version': '5.0.9-1', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': '76fc16318206289010c1be7977c94d3ee0b48408d5fe3e558c05615a703aa1f5',
        'signer': SIGNER_LEVENTE_POLYAK,
    },
    {
        'name': 'rdma-core', 'version': '65.0-1', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': 'a7a0648650811fd753ded1003acd83871bc31a4eb998bc4d04cf050ed02a238b',
        'signer': SIGNER_JAKUB_KLINKOVSKY,
    },
    {
        'name': 'vde2', 'version': '2.3.3-10', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': 'deb9c99fc2334c9410ccf7ac8af68ffb1846df9eb6c02f523c15403052ef1f37',
        'signer': SIGNER_DAVID_RUNGE,
    },
    {
        'name': 'libxdp', 'version': '1.6.3-2', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': '4146de33b9b12531064de40cb671537407c2267e4304159f407be92f2c8758a4',
        'signer': SIGNER_CHRISTIAN_HEUSEL,
    },
    {
        'name': 'ndctl', 'version': '85-1', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': '8473584f486f93fecefd62240ef1e83194e562c9ff899c1491036edae9e41443',
        'signer': SIGNER_FELIX_YAN,
    },
    {
        'name': 'dtc', 'version': '1:1.8.1-1', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': 'c377d325b42c4250102615e779cdeb67816a176cd0c55c018f7c81920057eb43',
        'signer': SIGNER_GEORGE_RAWLINSON,
    },
    {
        'name': 'libaio', 'version': '0.3.113-5', 'repo': 'core',
        'arch': 'x86_64',
        'sha256': 'e746fdedefa9987ad06e918af78976917aba780be8ac51d22dcb9ded324a84ab',
        'signer': SIGNER_DAVID_RUNGE,
    },
    {
        'name': 'seabios', 'version': '1.17.0-2', 'repo': 'extra',
        'arch': 'any',
        'sha256': 'edf543abcff088f94779fa1ec81096867fe5bff06b835da8e45c9c5b0e4d5269',
        'signer': SIGNER_DAVID_RUNGE,
    },
    {
        'name': 'mtools', 'version': '1:4.0.49-1', 'repo': 'extra',
        'arch': 'x86_64',
        'sha256': 'd5098ea3b7d1795905aa30e83cdaac9b53e081c9c08d7b02fdc2f21a7a36f449',
        'signer': SIGNER_CHRISTIAN_HEUSEL,
    },
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def run(args: list[str], *, env: dict[str, str] | None = None,
        timeout: int = 90) -> str:
    result = subprocess.run(args, text=True, capture_output=True,
                            stdin=subprocess.DEVNULL, env=env, timeout=timeout,
                            check=False)
    if result.returncode:
        raise RuntimeError(
            f'{Path(args[0]).name} failed ({result.returncode}): '
            f'{result.stdout}{result.stderr}')
    return result.stdout + result.stderr


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def assert_regular_private_file(path: Path, *, max_size: int) -> None:
    require(not path.is_symlink(), f'Refusing symlink: {path}')
    metadata = path.stat()
    require(stat.S_ISREG(metadata.st_mode), f'Not a regular file: {path}')
    require(metadata.st_size <= max_size, f'File exceeds size bound: {path}')


def create_private_tree(directory: Path) -> dict[str, Path]:
    absolute = Path(os.path.abspath(directory))
    for ancestor in (absolute, *absolute.parents):
        require(not ancestor.is_symlink(), f'Refusing symlink path component: {ancestor}')
    if absolute.exists():
        require(absolute.is_dir(), f'SDK path is not a directory: {absolute}')
        info = absolute.stat()
        require(info.st_uid == os.getuid(), f'SDK path is not owned by this user: {absolute}')
        require(stat.S_IMODE(info.st_mode) == 0o700,
                f'SDK path must have mode 0700: {absolute}')
    else:
        absolute.mkdir(mode=0o700, parents=True)
    paths = {}
    for name in ('downloads', 'keyring', 'root', 'evidence'):
        path = absolute / name
        require(not path.is_symlink(), f'Refusing symlink directory: {path}')
        if path.exists():
            require(path.is_dir(), f'Not a directory: {path}')
            info = path.stat()
            require(info.st_uid == os.getuid(), f'Directory is not user-owned: {path}')
            require(stat.S_IMODE(info.st_mode) == 0o700,
                    f'Directory must have mode 0700: {path}')
        else:
            path.mkdir(mode=0o700)
        paths[name] = path
    return paths


def copy_bounded(source: Path, target: Path, limit: int) -> None:
    assert_regular_private_file(source, max_size=limit)
    partial = target.with_name(target.name + '.partial')
    require(not partial.is_symlink(), f'Refusing symlink partial: {partial}')
    if partial.exists():
        info = partial.stat()
        require(info.st_uid == os.getuid(), f'Partial file is not user-owned: {partial}')
        partial.unlink()
    copied = 0
    fd = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with source.open('rb') as reader, os.fdopen(fd, 'wb') as writer:
            fd = -1
            while chunk := reader.read(1024 * 1024):
                copied += len(chunk)
                require(copied <= limit, f'Copy exceeds size bound: {source}')
                writer.write(chunk)
            writer.flush()
            os.fsync(writer.fileno())
    finally:
        if fd >= 0:
            os.close(fd)
    partial.replace(target)


def download_bounded(url: str, target: Path, limit: int) -> None:
    partial = target.with_name(target.name + '.partial')
    require(not partial.is_symlink(), f'Refusing symlink partial: {partial}')
    if partial.exists():
        info = partial.stat()
        require(info.st_uid == os.getuid(), f'Partial file is not user-owned: {partial}')
        partial.unlink()
    request = urllib.request.Request(
        url, headers={'User-Agent': 'PigADE-VM-SDK/0.0.1-dev'})
    downloaded = 0
    fd = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with urllib.request.urlopen(request, timeout=35) as response, \
                os.fdopen(fd, 'wb') as output:
            fd = -1
            while chunk := response.read(min(1024 * 1024, limit - downloaded + 1)):
                downloaded += len(chunk)
                require(downloaded <= limit, f'Download exceeds size bound: {url}')
                output.write(chunk)
            output.flush()
            os.fsync(output.fileno())
    except (urllib.error.URLError, TimeoutError) as error:
        raise RuntimeError(f'Download failed: {url}: {error}') from error
    finally:
        if fd >= 0:
            os.close(fd)
    partial.replace(target)


def obtain_file(url: str, target: Path, expected_hash: str, limit: int,
                cache_sources: tuple[Path, ...]) -> None:
    require(not target.is_symlink(), f'Refusing symlink cache file: {target}')
    if target.exists():
        assert_regular_private_file(target, max_size=limit)
        actual = sha256(target)
        require(actual == expected_hash,
                f'Cached hash mismatch for {target.name}: {actual}')
        return
    for source in cache_sources:
        if source.exists():
            assert_regular_private_file(source, max_size=limit)
            actual = sha256(source)
            require(actual == expected_hash,
                    f'Cached hash mismatch for {source.name}: {actual}')
            copy_bounded(source, target, limit)
            return
    download_bounded(url, target, limit)
    actual = sha256(target)
    require(actual == expected_hash,
            f'Download hash mismatch for {target.name}: {actual}')


def parse_package_info(archive: Path) -> dict[str, list[str]]:
    with tarfile.open(archive, mode='r:*') as bundle:
        try:
            member = bundle.getmember('.PKGINFO')
        except KeyError as error:
            raise RuntimeError(f'Missing .PKGINFO in {archive.name}') from error
        stream = bundle.extractfile(member)
        require(stream is not None, f'Cannot read .PKGINFO in {archive.name}')
        payload = stream.read(1024 * 1024 + 1)
    require(len(payload) <= 1024 * 1024,
            f'.PKGINFO exceeds size bound in {archive.name}')
    result: dict[str, list[str]] = {}
    for line in payload.decode('utf-8').splitlines():
        if ' = ' not in line:
            continue
        key, value = line.split(' = ', 1)
        result.setdefault(key, []).append(value)
    return result


def safe_member(member: tarfile.TarInfo) -> bool:
    name = member.name.rstrip('/')
    path = PurePosixPath(name)
    require(name and not path.is_absolute() and
            all(part not in ('', '.', '..') for part in name.split('/')),
            f'Unsafe archive path: {member.name}')
    require(member.isdir() or member.isfile() or member.issym() or member.islnk(),
            f'Unsupported archive entry type: {member.name}')
    if member.issym() or member.islnk():
        link = PurePosixPath(member.linkname)
        require(member.linkname and not link.is_absolute(),
                f'Unsafe archive link: {member.name} -> {member.linkname}')
        if member.issym():
            base = list(path.parent.parts)
        else:
            base = []
        for part in link.parts:
            if part in ('', '.'):
                continue
            if part == '..':
                require(bool(base),
                        f'Archive link escapes extraction root: {member.name} -> {member.linkname}')
                base.pop()
            else:
                base.append(part)
    return True


def extract_package(archive: Path, root: Path) -> None:
    excluded = {'.PKGINFO', '.BUILDINFO', '.MTREE', '.INSTALL'}
    with tarfile.open(archive, mode='r:*') as bundle:
        members = bundle.getmembers()
        for member in members:
            safe_member(member)
        payload = [member for member in members
                   if PurePosixPath(member.name).name not in excluded]
        bundle.extractall(root, members=payload, filter='data')


def verify_signature(signature: Path, archive: Path, keyring: Path,
                     private_gnupg: Path, expected_primary: str) -> dict[str, str]:
    output = run(['gpgv', '--homedir', str(private_gnupg), '--keyring',
                  str(keyring), '--status-fd', '1', str(signature), str(archive)])
    valid = [line.split() for line in output.splitlines()
             if line.startswith('[GNUPG:] VALIDSIG ')]
    require(len(valid) == 1,
            f'Expected one valid Arch signature for {archive.name}')
    fields = valid[0]
    signing_fingerprint = fields[2].upper()
    primary_fingerprint = fields[-1].upper() if len(fields) >= 12 else signing_fingerprint
    require(primary_fingerprint == expected_primary,
            f'Unexpected signer for {archive.name}: {primary_fingerprint}')
    return {
        'signing_fingerprint': signing_fingerprint,
        'primary_fingerprint': primary_fingerprint,
        'gpgv_evidence': output.strip(),
    }


def prepare(args: argparse.Namespace) -> int:
    paths = create_private_tree(args.directory)
    for executable in ('gpg', 'gpgv', 'ldd'):
        require(shutil.which(executable) is not None,
                f'Required local executable not found: {executable}')
    require(KEYRING_SOURCE.is_file() and not KEYRING_SOURCE.is_symlink(),
            f'Arch keyring not found: {KEYRING_SOURCE}')

    keyring = paths['keyring'] / 'archlinux.gpg'
    private_gnupg = paths['keyring'] / 'gnupg'
    require(not private_gnupg.is_symlink(), f'Refusing symlink: {private_gnupg}')
    if private_gnupg.exists():
        require(private_gnupg.is_dir() and private_gnupg.stat().st_uid == os.getuid(),
                f'Unsafe private GnuPG directory: {private_gnupg}')
    else:
        private_gnupg.mkdir(mode=0o700)
    require(not keyring.is_symlink(), f'Refusing symlink: {keyring}')
    run(['gpg', '--homedir', str(private_gnupg), '--batch', '--yes',
         '--dearmor', '--output', str(keyring), str(KEYRING_SOURCE)])
    keyring.chmod(0o600)

    downloaded: list[dict[str, object]] = []
    for package in PACKAGES:
        filename = (f"{package['name']}-{package['version']}-"
                    f"{package['arch']}.pkg.tar.zst")
        archive = paths['downloads'] / filename
        signature = archive.with_name(archive.name + '.sig')
        url = f"{MIRROR}/{package['repo']}/os/x86_64/{filename}"
        legacy_cache = Path('/tmp/pigade-qemu-research-20261017') / filename
        cache_sources = ((legacy_cache,) if package['name'] in {
            'qemu-system-x86', 'qemu-common', 'qemu-system-x86-firmware',
            'libslirp'} else ())
        obtain_file(url, archive, package['sha256'], MAX_PACKAGE_BYTES, cache_sources)
        require(not signature.is_symlink(), f'Refusing symlink signature: {signature}')
        if not signature.exists() and cache_sources:
            cached_signature = legacy_cache.with_name(legacy_cache.name + '.sig')
            if cached_signature.exists():
                copy_bounded(cached_signature, signature, MAX_SIGNATURE_BYTES)
        if not signature.exists():
            download_bounded(url + '.sig', signature, MAX_SIGNATURE_BYTES)
        assert_regular_private_file(archive, max_size=MAX_PACKAGE_BYTES)
        assert_regular_private_file(signature, max_size=MAX_SIGNATURE_BYTES)
        require(sha256(archive) == package['sha256'],
                f'Archive hash mismatch: {filename}')
        signer = verify_signature(signature, archive, keyring,
                                  private_gnupg, package['signer'])
        metadata = parse_package_info(archive)
        require(metadata.get('pkgname') == [package['name']],
                f'Unexpected package name in {filename}: {metadata.get("pkgname")}')
        require(metadata.get('pkgver') == [package['version']],
                f'Unexpected package version in {filename}: {metadata.get("pkgver")}')
        downloaded.append({
            'name': package['name'], 'version': package['version'],
            'architecture': package['arch'], 'repository': package['repo'],
            'url': url, 'signature_url': url + '.sig', 'bytes': archive.stat().st_size,
            'sha256': package['sha256'], 'signature_sha256': sha256(signature),
            'signer': signer, 'licenses': metadata.get('license', []),
            'runtime_dependencies': metadata.get('depend', []),
            'archive': str(archive),
        })

    total_bytes = sum(int(item['bytes']) for item in downloaded)
    total_bytes += sum((paths['downloads'] / (Path(str(item['archive'])).name + '.sig')).stat().st_size
                       for item in downloaded)
    require(total_bytes < MAX_TOTAL_BYTES,
            f'Package archive set exceeds total download limit: {total_bytes} bytes')

    package_set = hashlib.sha256(
        ''.join(str(item['sha256']) for item in downloaded).encode()).hexdigest()[:16]
    extraction_root = paths['root'] / f'qemu-{package_set}'
    require(not extraction_root.is_symlink(), f'Refusing symlink extraction root: {extraction_root}')
    marker = extraction_root / '.package-set.json'
    if extraction_root.exists():
        require(extraction_root.is_dir() and extraction_root.stat().st_uid == os.getuid(),
                f'Unsafe extraction root: {extraction_root}')
        require(marker.is_file() and not marker.is_symlink(),
                f'Existing extraction root lacks a trusted package marker: {extraction_root}')
        recorded = json.loads(marker.read_text())
        require(recorded == [item['sha256'] for item in downloaded],
                f'Existing extraction root has another package set: {extraction_root}')
    else:
        extraction_root.mkdir(mode=0o700)
        for item in downloaded:
            extract_package(Path(str(item['archive'])), extraction_root)
        marker.write_text(json.dumps([item['sha256'] for item in downloaded]) + '\n')
        marker.chmod(0o600)

    qemu = extraction_root / 'usr/bin/qemu-system-x86_64'
    mcopy = extraction_root / 'usr/bin/mcopy'
    bios = extraction_root / 'usr/share/qemu/bios-256k.bin'
    qemu_data = extraction_root / 'usr/share/qemu'
    for required_path in (qemu, mcopy, bios):
        require(required_path.is_file(), f'Missing expected SDK payload: {required_path}')

    runtime_env = {
        'PATH': '/usr/bin', 'LC_ALL': 'C',
        'LD_LIBRARY_PATH': f'{extraction_root}/usr/lib:{extraction_root}/usr/lib/qemu',
    }
    elf_targets = [qemu, mcopy]
    module_dir = extraction_root / 'usr/lib/qemu'
    if module_dir.is_dir():
        for candidate in sorted(module_dir.rglob('*.so*')):
            if candidate.is_file() and candidate.read_bytes()[:4] == b'\x7fELF':
                elf_targets.append(candidate)

    ldd_checks = {}
    unresolved: list[str] = []
    for target in elf_targets:
        result = subprocess.run(['ldd', str(target)], text=True, capture_output=True,
                                stdin=subprocess.DEVNULL, env=runtime_env,
                                timeout=30, check=False)
        evidence = result.stdout + result.stderr
        target_unresolved = [line.strip() for line in evidence.splitlines()
                             if 'not found' in line]
        ldd_checks[str(target)] = {
            'returncode': result.returncode,
            'output': evidence.strip(),
            'unresolved': target_unresolved,
        }
        unresolved.extend(f'{target}: {line}' for line in target_unresolved)
        if result.returncode:
            unresolved.append(f'{target}: ldd returned {result.returncode}')

    commands = {}
    if not unresolved:
        commands['qemu_version'] = run([str(qemu), '--version'], env=runtime_env).splitlines()[0]
        commands['mcopy_version'] = run([str(mcopy), '-V'], env=runtime_env).splitlines()[0]

    manifest = {
        'schema': 1,
        'status': 'ready' if not unresolved else 'unresolved-runtime-dependencies',
        'prepared_at_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'sdk_directory': str(args.directory.absolute()),
        'extraction_root': str(extraction_root),
        'keyring_source': str(KEYRING_SOURCE),
        'keyring_sha256': sha256(keyring),
        'packages': downloaded,
        'qemu_system_x86_64': str(qemu),
        'qemu_data_dir': str(qemu_data),
        'seabios_bios_256k': str(bios),
        'mcopy': str(mcopy),
        'qemu_img': None,
        'uefi': 'not provisioned; this SDK is for explicit SeaBIOS boot only',
        'total_downloaded_bytes': total_bytes,
        'ldd_checks': ldd_checks,
        'unresolved': unresolved,
        'commands': commands,
        'limits': [
            'No guest image is downloaded or included',
            'No system package is installed and package installer hooks are not run',
            'Host ABI libraries may satisfy dependencies; inspect ldd_checks before use',
            'This command does not start QEMU or create a VM/network service/display',
            'Launch QEMU with -display none and private user networking only',
        ],
    }
    manifest_path = paths['evidence'] / 'manifest.json'
    require(not manifest_path.is_symlink(), f'Refusing symlink manifest: {manifest_path}')
    manifest_path.write_text(json.dumps(manifest, indent=2) + '\n')
    manifest_path.chmod(0o600)
    print(json.dumps({
        'status': manifest['status'], 'manifest': str(manifest_path),
        'qemu': str(qemu), 'mcopy': str(mcopy),
        'package_count': len(downloaded), 'total_downloaded_bytes': total_bytes,
        'unresolved': unresolved,
    }, indent=2))
    return 0 if not unresolved else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, default=DEFAULT_DIRECTORY,
                        help=f'private owned SDK cache (default: {DEFAULT_DIRECTORY})')
    parser.add_argument('--prepare', action='store_true',
                        help='download/verify/extract pinned Arch packages and check ldd closure')
    args = parser.parse_args()
    if not args.prepare:
        parser.print_help()
        return 0
    try:
        return prepare(args)
    except Exception as error:
        print(f'prepare-vm-sdk: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
