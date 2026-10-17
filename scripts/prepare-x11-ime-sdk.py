#!/usr/bin/env python3
"""Prepare a signed, pinned Xvfb/Fcitx-Pinyin test SDK; never launch it.

Archives are SHA-256 pinned and verified against the installed Arch keyring,
then safely extracted under /tmp/pigade-x11-ime-sdk-20261017. This script does
not install packages, run package hooks, start Xvfb/Fcitx, or inject input.
Host /usr libraries are permitted only as explicitly recorded ldd dependencies;
this is not a portable sysroot and does not establish namespace isolation.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import shutil
import stat
import subprocess
import sys
import tarfile
import urllib.error
import urllib.request


SDK = Path('/tmp/pigade-x11-ime-sdk-20261017')
KEYRING_SOURCE = Path('/usr/share/pacman/keyrings/archlinux.gpg')
MIRROR = 'https://archive.archlinux.org/packages'
MAX_PACKAGE_BYTES = 60 * 1024 * 1024
MAX_SIGNATURE_BYTES = 1024 * 1024
MAX_TOTAL_BYTES = 90 * 1024 * 1024
MAX_EXTRACTED_BYTES = 256 * 1024 * 1024
MAX_PKGINFO_BYTES = 1024 * 1024
EXTRACTION_PROFILE = 'pinyin-punctuation-runtime-v2'


def pkg(name: str, version: str, arch: str, digest: str, signer: str) -> dict[str, str]:
    return {'name': name, 'version': version, 'arch': arch, 'sha256': digest,
            'signer': signer}


PACKAGES = (
    pkg('xorg-server-xvfb', '21.1.25-2', 'x86_64',
        '52c6d0394109b0bdfd0e4d00dd8d89d7e38bfe7576d96145179f216480eb7564',
        'ADC8A1FCC15E01D45310419E94657AB20F2A092B'),
    pkg('xorg-server-common', '21.1.25-2', 'x86_64',
        '851b8e7ad135d99ce263334f64512a10c69d6b19231a6de48953fe44227c0dcc',
        'ADC8A1FCC15E01D45310419E94657AB20F2A092B'),
    pkg('xorg-xauth', '1.1.5-1', 'x86_64',
        'df9e0d1f29ae0135a49b17157841a4cc7c97869f9f27fd7480fff64f9afb94b7',
        'ADC8A1FCC15E01D45310419E94657AB20F2A092B'),
    pkg('xorg-xkbcomp', '1.5.0-1', 'x86_64',
        '662f4ad50763b9d4192bb4f2bbd81ba3c50630a0ec2a83a13deb8770f785734d',
        'ADC8A1FCC15E01D45310419E94657AB20F2A092B'),
    pkg('xorg-setxkbmap', '1.3.5-1', 'x86_64',
        'dc1dce9a0aa737d39344d15232a62ab6b1963bb5763360bf166c11699c37e3f5',
        'ADC8A1FCC15E01D45310419E94657AB20F2A092B'),
    pkg('xkeyboard-config', '2.48-1', 'any',
        '7f7f8656dbd0b37e36d3a97f6e188fc5c6cbccc4c1e5d34ff35f1d7a79b1cc73',
        'ADC8A1FCC15E01D45310419E94657AB20F2A092B'),
    pkg('libxtst', '1.2.5-1', 'x86_64',
        '7e036fc4ae02ec16d09efed08b111e33d4753aa959d12148461d653659a9f529',
        'ADC8A1FCC15E01D45310419E94657AB20F2A092B'),
    pkg('fcitx5', '5.1.23-1', 'x86_64',
        '37d26735c03a923a69bacd2ec950ebb26f2f89dcde4318be7e36550e676df9a7',
        'B5971F2C5C10A9A08C60030F786C63F330D7CB92'),
    pkg('fcitx5-chinese-addons', '5.1.15-2', 'x86_64',
        '719a7cd8ea9829d16a57937d5565385af3fad2ea4d9e6b3a6281bf7eb67810ab',
        'C5D2A6E0ED2D11C66B9FA2A306313911057DD5A8'),
    pkg('libime', '1.1.17-1', 'x86_64',
        '82b8a0cad9cba17b8423b3f9b41fbdb4acb5f76211ff408e11b10ecb0a8dd2ea',
        'B5971F2C5C10A9A08C60030F786C63F330D7CB92'),
    pkg('boost-libs', '1.92.0-1', 'x86_64',
        '4b1392e578e46c1b23910d1c26956927d7e986d6afd5090be59045afb3c04f8d',
        'F00B96D15228013FFC9C9D0393B11DAA4C197E3D'),
    pkg('opencc', '1.2.0-1', 'x86_64',
        '1ebb3fdd742f41fd8d51e7de528c84c4db63746705dbe6cdf9870693774ca886',
        'B5971F2C5C10A9A08C60030F786C63F330D7CB92'),
    pkg('marisa', '0.3.1-1', 'x86_64',
        'ad6aa2b6cf3dd4f41dba48c8ca28c49830c8ba8ea4f9bfe45d4a9b9762d0497a',
        'B5971F2C5C10A9A08C60030F786C63F330D7CB92'),
)

# The Chinese-addons package metadata also depends on Qt/Fcitx-Qt for optional
# editor/UI modules. Extract Pinyin and its required punctuation dependency;
# optional Qt modules are outside this test and must not pull in Qt WebEngine.
SELECTED_FILES = {
    'fcitx5-chinese-addons': {
        'usr/lib/fcitx5/libpinyin.so',
        'usr/share/fcitx5/addon/pinyin.conf',
        'usr/share/fcitx5/inputmethod/pinyin.conf',
        'usr/share/fcitx5/pinyin/symbols',
        'usr/share/fcitx5/pinyin/chaizi.dict',
        'usr/lib/fcitx5/libpunctuation.so',
        'usr/share/fcitx5/addon/punctuation.conf',
        'usr/share/fcitx5/punctuation/punc.mb.zh_CN',
    },
}

RUNTIME_OBJECTS = (
    'usr/bin/Xvfb', 'usr/bin/xauth', 'usr/bin/xkbcomp', 'usr/bin/setxkbmap',
    'usr/bin/fcitx5', 'usr/bin/fcitx5-remote',
    'usr/lib/fcitx5/libxim.so', 'usr/lib/fcitx5/libxcb.so',
    'usr/lib/fcitx5/libdbus.so', 'usr/lib/fcitx5/libdbusfrontend.so',
    'usr/lib/fcitx5/libpinyin.so', 'usr/lib/fcitx5/libpunctuation.so',
    'usr/lib/libIMEPinyin.so.0',
    'usr/lib/libIMECore.so.0', 'usr/lib/libopencc.so',
    'usr/lib/libXtst.so',
)
REQUIRED_PAYLOADS = RUNTIME_OBJECTS + (
    'usr/include/X11/extensions/XTest.h',
    'usr/share/fcitx5/addon/pinyin.conf',
    'usr/share/fcitx5/addon/punctuation.conf',
    'usr/share/fcitx5/inputmethod/pinyin.conf',
    'usr/share/fcitx5/punctuation/punc.mb.zh_CN',
    'usr/lib/libime/zh_CN.lm',
    'usr/share/X11/xkb',
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def assert_private_file(path: Path, *, limit: int) -> None:
    require(not path.is_symlink(), f'Refusing symlink file: {path}')
    info = path.stat()
    require(stat.S_ISREG(info.st_mode), f'Not a regular file: {path}')
    require(info.st_uid == os.getuid(), f'File is not owned by this user: {path}')
    require(info.st_size <= limit, f'File exceeds size limit: {path}')


def create_private_tree(directory: Path) -> dict[str, Path]:
    absolute = Path(os.path.abspath(directory))
    for ancestor in (absolute, *absolute.parents):
        require(not ancestor.is_symlink(), f'Refusing symlink path component: {ancestor}')
    if absolute.exists():
        require(absolute.is_dir(), f'SDK root is not a directory: {absolute}')
        info = absolute.stat()
        require(info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == 0o700,
                f'SDK root must be user-owned and mode 0700: {absolute}')
    else:
        absolute.mkdir(mode=0o700, parents=True)
    paths: dict[str, Path] = {}
    for name in ('downloads', 'keyring', 'root', 'evidence'):
        path = absolute / name
        require(not path.is_symlink(), f'Refusing symlink directory: {path}')
        if path.exists():
            info = path.stat()
            require(path.is_dir() and info.st_uid == os.getuid() and
                    stat.S_IMODE(info.st_mode) == 0o700,
                    f'Directory must be user-owned and mode 0700: {path}')
        else:
            path.mkdir(mode=0o700)
        paths[name] = path
    return paths


def package_filename(package: dict[str, str]) -> str:
    return (f"{package['name']}-{package['version']}-"
            f"{package['arch']}.pkg.tar.zst")


def package_url(package: dict[str, str]) -> str:
    filename = package_filename(package)
    return f"{MIRROR}/{package['name'][0]}/{package['name']}/{filename}"


def obtain_file(url: str, target: Path, limit: int) -> None:
    require(not target.is_symlink(), f'Refusing symlink download target: {target}')
    if target.exists():
        assert_private_file(target, limit=limit)
        return
    partial = target.with_name(target.name + '.partial')
    require(not partial.is_symlink(), f'Refusing symlink partial: {partial}')
    if partial.exists():
        assert_private_file(partial, limit=limit)
        partial.unlink()
    request = urllib.request.Request(
        url, headers={'User-Agent': 'PigADE-X11-IME-SDK/0.0.1-dev'})
    fd = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    downloaded = 0
    try:
        with urllib.request.urlopen(request, timeout=45) as response, \
                os.fdopen(fd, 'wb') as output:
            fd = -1
            while chunk := response.read(min(1024 * 1024, limit - downloaded + 1)):
                downloaded += len(chunk)
                require(downloaded <= limit, f'Download exceeds size limit: {url}')
                output.write(chunk)
            output.flush()
            os.fsync(output.fileno())
    except (urllib.error.URLError, TimeoutError) as error:
        raise RuntimeError(f'Download failed: {url}: {error}') from error
    finally:
        if fd >= 0:
            os.close(fd)
    partial.replace(target)


def verify_status(output: str, expected_primary: str) -> dict[str, str]:
    signatures = [line.split() for line in output.splitlines()
                  if line.startswith('[GNUPG:] VALIDSIG ')]
    require(len(signatures) == 1, 'Expected one valid detached package signature')
    fields = signatures[0]
    signer = fields[-1].upper() if len(fields) >= 12 else fields[2].upper()
    require(signer == expected_primary,
            f'Unexpected package signer: {signer}; expected {expected_primary}')
    return {'primary_fingerprint': signer, 'status': output.strip()}


def verify_signature(signature: Path, archive: Path, keyring: Path,
                     gpg_home: Path, expected_primary: str) -> dict[str, str]:
    result = subprocess.run(
        ['gpgv', '--homedir', str(gpg_home), '--keyring', str(keyring),
         '--status-fd', '1', str(signature), str(archive)],
        text=True, capture_output=True, stdin=subprocess.DEVNULL,
        timeout=45, check=False)
    output = result.stdout + result.stderr
    require(result.returncode == 0, f'Package signature verification failed: {archive.name}')
    return verify_status(output, expected_primary)


def parse_package_info(archive: Path) -> dict[str, list[str]]:
    with tarfile.open(archive, mode='r:*') as bundle:
        try:
            member = bundle.getmember('.PKGINFO')
        except KeyError as error:
            raise RuntimeError(f'Missing .PKGINFO in {archive.name}') from error
        require(member.isfile(), f'.PKGINFO is not a regular file in {archive.name}')
        require(member.size <= MAX_PKGINFO_BYTES,
                f'.PKGINFO exceeds size limit in {archive.name}')
        stream = bundle.extractfile(member)
        require(stream is not None, f'Cannot read .PKGINFO in {archive.name}')
        payload = stream.read(MAX_PKGINFO_BYTES + 1)
    require(len(payload) <= MAX_PKGINFO_BYTES,
            f'.PKGINFO exceeds size limit in {archive.name}')
    result: dict[str, list[str]] = {}
    for line in payload.decode('utf-8').splitlines():
        if ' = ' in line:
            key, value = line.split(' = ', 1)
            result.setdefault(key, []).append(value)
    return result


def normalized_member_name(name: str) -> str:
    require(name and not name.startswith('/'), f'Unsafe archive path: {name!r}')
    parts = name.rstrip('/').split('/')
    require(all(part not in ('', '.', '..') for part in parts),
            f'Unsafe archive path: {name!r}')
    return '/'.join(parts)


def safe_symlink_target(name: str, target: str) -> str:
    require(target, f'Empty symlink target for {name}')
    link = PurePosixPath(target)
    if link.is_absolute():
        destination = posixpath.normpath(target.lstrip('/'))
        require(destination not in ('', '.') and not destination.startswith('../') and
                destination != '..', f'Absolute symlink escapes SDK: {name} -> {target}')
        return posixpath.relpath(destination, posixpath.dirname(name))
    destination = posixpath.normpath(posixpath.join(posixpath.dirname(name), target))
    require(destination not in ('', '.') and not destination.startswith('../') and
            destination != '..', f'Symlink escapes SDK: {name} -> {target}')
    return target


def validate_members(members: list[tarfile.TarInfo]) -> tuple[list[tarfile.TarInfo], int]:
    names: dict[str, tarfile.TarInfo] = {}
    total_bytes = 0
    for member in members:
        name = normalized_member_name(member.name)
        require(name not in names, f'Duplicate archive path: {name}')
        require(member.isdir() or member.isfile() or member.issym(),
                f'Unsupported archive entry type: {name}')
        if member.issym():
            member.linkname = safe_symlink_target(name, member.linkname)
        if member.isfile():
            require(member.size >= 0, f'Negative file size: {name}')
            total_bytes += member.size
            require(total_bytes <= MAX_EXTRACTED_BYTES,
                    'Package set exceeds uncompressed extraction size limit')
        names[name] = member
    for name, member in names.items():
        parts = PurePosixPath(name).parts
        for index in range(1, len(parts)):
            parent = '/'.join(parts[:index])
            parent_member = names.get(parent)
            require(parent_member is None or parent_member.isdir(),
                    f'Archive path is nested under a non-directory: {name}')
    return list(names.values()), total_bytes


def ensure_directory(root: Path, relative: str) -> Path:
    current = root
    for part in PurePosixPath(relative).parts:
        current = current / part
        require(not current.is_symlink(), f'Refusing symlink parent: {current}')
        if not current.exists():
            current.mkdir(mode=0o755)
        require(current.is_dir(), f'Extraction parent is not a directory: {current}')
    return current


def write_member(bundle: tarfile.TarFile, member: tarfile.TarInfo, root: Path) -> None:
    name = normalized_member_name(member.name)
    destination = root / name
    ensure_directory(root, str(PurePosixPath(name).parent) if '/' in name else '')
    if member.isdir():
        require(not destination.exists() and not destination.is_symlink(),
                f'Unexpected existing archive directory: {destination}')
        destination.mkdir(mode=0o755)
        return
    require(not destination.exists() and not destination.is_symlink(),
            f'Archive path collision: {destination}')
    if member.issym():
        os.symlink(member.linkname, destination)
        return
    stream = bundle.extractfile(member)
    require(stream is not None, f'Cannot read package file: {name}')
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, 'O_NOFOLLOW'):
        flags |= os.O_NOFOLLOW
    fd = os.open(destination, flags, member.mode & 0o755)
    try:
        with os.fdopen(fd, 'wb') as output:
            fd = -1
            remaining = member.size
            while remaining:
                chunk = stream.read(min(1024 * 1024, remaining))
                require(chunk, f'Truncated package member: {name}')
                output.write(chunk)
                remaining -= len(chunk)
            output.flush()
            os.fsync(output.fileno())
    finally:
        if fd >= 0:
            os.close(fd)


def extract_package(archive: Path, root: Path, package_name: str) -> dict[str, int]:
    excluded = {'.PKGINFO', '.BUILDINFO', '.MTREE', '.INSTALL'}
    selected = SELECTED_FILES.get(package_name)
    with tarfile.open(archive, mode='r:*') as bundle:
        members, extracted_size = validate_members(bundle.getmembers())
        files = [member for member in members
                 if PurePosixPath(member.name).name not in excluded and
                 (selected is None or normalized_member_name(member.name) in selected)]
        # Make parents first, then regular files and links. The package set uses
        # no hardlinks; any unsupported archive member fails before writing.
        dirs = [member for member in files if member.isdir()]
        payload = [member for member in files if member.isfile()]
        links = [member for member in files if member.issym()]
        for member in dirs:
            name = normalized_member_name(member.name)
            destination = root / name
            ensure_directory(root, str(PurePosixPath(name).parent) if '/' in name else '')
            if destination.exists():
                require(destination.is_dir() and not destination.is_symlink(),
                        f'Package directory collision: {destination}')
            else:
                destination.mkdir(mode=0o755)
        for member in payload + links:
            write_member(bundle, member, root)
    return {'archive_members': len(members), 'selected_members': len(files),
            'archive_uncompressed_bytes': extracted_size}


def atomic_json(path: Path, value: object) -> None:
    require(not path.is_symlink(), f'Refusing symlink output: {path}')
    temporary = path.with_name(path.name + '.partial')
    require(not temporary.is_symlink(), f'Refusing symlink output: {temporary}')
    if temporary.exists():
        assert_private_file(temporary, limit=8 * 1024 * 1024)
        temporary.unlink()
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w', encoding='utf-8') as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())
    temporary.replace(path)


def load_signed_packages(paths: dict[str, Path]) -> tuple[list[dict[str, object]], dict[str, Path]]:
    for executable in ('gpg', 'gpgv', 'ldd'):
        require(shutil.which(executable) is not None,
                f'Required local executable not found: {executable}')
    require(KEYRING_SOURCE.is_file() and not KEYRING_SOURCE.is_symlink(),
            f'Arch keyring is missing: {KEYRING_SOURCE}')
    keyring = paths['keyring'] / 'archlinux.gpg'
    private_home = paths['keyring'] / 'gnupg'
    require(not keyring.is_symlink() and not private_home.is_symlink(),
            'Refusing symlink in private keyring directory')
    if not private_home.exists():
        private_home.mkdir(mode=0o700)
    require(private_home.is_dir() and private_home.stat().st_uid == os.getuid() and
            stat.S_IMODE(private_home.stat().st_mode) == 0o700,
            'Private GnuPG home must be user-owned and mode 0700')
    if keyring.exists():
        assert_private_file(keyring, limit=16 * 1024 * 1024)
    subprocess.run(['gpg', '--homedir', str(private_home), '--batch', '--yes',
                    '--dearmor', '--output', str(keyring), str(KEYRING_SOURCE)],
                   stdin=subprocess.DEVNULL, check=True, timeout=30,
                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    keyring.chmod(0o600)

    result: list[dict[str, object]] = []
    total_archive_bytes = 0
    archives: dict[str, Path] = {}
    for package in PACKAGES:
        filename = package_filename(package)
        archive = paths['downloads'] / filename
        signature = archive.with_name(filename + '.sig')
        url = package_url(package)
        obtain_file(url, archive, MAX_PACKAGE_BYTES)
        obtain_file(url + '.sig', signature, MAX_SIGNATURE_BYTES)
        archive_hash = sha256(archive)
        require(archive_hash == package['sha256'],
                f'Pinned archive SHA-256 mismatch for {filename}: {archive_hash}')
        signer = verify_signature(signature, archive, keyring, private_home,
                                  package['signer'])
        metadata = parse_package_info(archive)
        require(metadata.get('pkgname') == [package['name']],
                f'Unexpected package name in {filename}')
        require(metadata.get('pkgver') == [package['version']],
                f'Unexpected package version in {filename}')
        require(metadata.get('arch') == [package['arch']],
                f'Unexpected package architecture in {filename}')
        total_archive_bytes += archive.stat().st_size + signature.stat().st_size
        require(total_archive_bytes <= MAX_TOTAL_BYTES,
                'Pinned archive set exceeds total download limit')
        archives[package['name']] = archive
        result.append({
            'name': package['name'], 'version': package['version'],
            'architecture': package['arch'], 'url': url,
            'signature_url': url + '.sig', 'archive_bytes': archive.stat().st_size,
            'archive_sha256': archive_hash,
            'signature_sha256': sha256(signature), 'expected_signer': package['signer'],
            'verified_signer': signer, 'licenses': metadata.get('license', []),
            'package_runtime_dependencies': metadata.get('depend', []),
            'extraction': 'selected-pinyin-and-required-punctuation-payloads'
            if package['name'] in SELECTED_FILES else 'full-package-payload',
        })
    return result, archives


def extract_package_set(archives: dict[str, Path], root: Path) -> list[dict[str, object]]:
    results = []
    total_extracted_bytes = 0
    for package in PACKAGES:
        extracted = extract_package(archives[package['name']], root, package['name'])
        total_extracted_bytes += extracted['archive_uncompressed_bytes']
        require(total_extracted_bytes <= MAX_EXTRACTED_BYTES,
                'Pinned package set exceeds total extracted size limit')
        results.append({
            'name': package['name'],
            'archive_members': extracted['archive_members'],
            'selected_members': extracted['selected_members'],
            'archive_uncompressed_bytes': extracted['archive_uncompressed_bytes'],
        })
    return results


def package_set_id() -> str:
    payload = (EXTRACTION_PROFILE + '\n' +
               ''.join(package['sha256'] for package in PACKAGES)).encode()
    return hashlib.sha256(payload).hexdigest()[:16]


def check_runtime(paths: dict[str, Path]) -> tuple[dict[str, object], list[str]]:
    root = paths['extract_root']
    lib_dirs = (root / 'usr/lib', root / 'usr/lib/fcitx5')
    environment = {'PATH': '/usr/bin', 'LC_ALL': 'C',
                   'LD_LIBRARY_PATH': ':'.join(map(str, lib_dirs))}
    checks: dict[str, object] = {}
    unresolved: list[str] = []
    for relative in REQUIRED_PAYLOADS:
        require((root / relative).exists(), f'Missing required SDK payload: {relative}')
    for relative in RUNTIME_OBJECTS:
        target = root / relative
        require(target.is_file(), f'Missing required SDK runtime payload: {relative}')
        result = subprocess.run(['ldd', '-r', str(target)], text=True, capture_output=True,
                                stdin=subprocess.DEVNULL, env=environment,
                                timeout=45, check=False)
        output = (result.stdout + result.stderr).strip()
        missing = [line.strip() for line in output.splitlines()
                   if 'not found' in line or 'undefined symbol:' in line]
        item = {'returncode': result.returncode, 'output': output,
                'unresolved': missing}
        checks[relative] = item
        unresolved.extend(f'{relative}: {line}' for line in missing)
        if result.returncode:
            unresolved.append(f'{relative}: ldd exited {result.returncode}')
        evidence_file = paths['evidence'] / (Path(relative).name + '.ldd.txt')
        evidence_file.write_text(output + '\n', encoding='utf-8')
        evidence_file.chmod(0o600)
    return checks, unresolved


def plan() -> dict[str, object]:
    return {
        'status': 'plan-only-no-downloads-no-writes', 'destination': str(SDK),
        'extraction_profile': EXTRACTION_PROFILE,
        'packages': [{'name': p['name'], 'version': p['version'],
                      'architecture': p['arch'], 'sha256': p['sha256'],
                      'primary_signer': p['signer'],
                      'url': package_url(p)} for p in PACKAGES],
        'limits': ['No package manager or installer hooks',
                   'No Xvfb/Fcitx/app execution or input synthesis',
                   'Pinyin plugin selected without optional Qt UI payloads'],
    }


def prepare() -> dict[str, object]:
    paths = create_private_tree(SDK)
    paths['extract_root'] = paths['root'] / f'x11-ime-{package_set_id()}'
    package_rows, archives = load_signed_packages(paths)
    marker = paths['extract_root'] / '.package-set.json'
    expected_hashes = [p['sha256'] for p in PACKAGES]
    if paths['extract_root'].exists():
        require(paths['extract_root'].is_dir() and not paths['extract_root'].is_symlink(),
                f'Unsafe extraction root: {paths["extract_root"]}')
        info = paths['extract_root'].stat()
        require(info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == 0o700,
                f'Extraction root must be user-owned and mode 0700: {paths["extract_root"]}')
        require(marker.is_file() and not marker.is_symlink(),
                f'Existing extraction root lacks package marker: {paths["extract_root"]}')
        assert_private_file(marker, limit=1024 * 1024)
        recorded = json.loads(marker.read_text())
        expected_marker = {'extraction_profile': EXTRACTION_PROFILE,
                           'package_sha256': expected_hashes}
        require(recorded == expected_marker,
                'Existing extraction root has a different package/profile set')
        manifest_path = paths['evidence'] / 'manifest.json'
        require(manifest_path.is_file() and not manifest_path.is_symlink(),
                'Existing extraction root lacks its preparation manifest')
        assert_private_file(manifest_path, limit=8 * 1024 * 1024)
        previous = json.loads(manifest_path.read_text())
        require(previous.get('extraction_profile') == EXTRACTION_PROFILE,
                'Prior manifest has a different extraction profile')
        previous_rows = previous.get('packages', [])
        require([row.get('archive_sha256') for row in previous_rows] == expected_hashes,
                'Prior manifest package hashes differ')
        previous_by_name = {row['name']: row for row in previous_rows}
        for row in package_rows:
            old = previous_by_name[row['name']]
            for key in ('archive_members', 'selected_members',
                        'archive_uncompressed_bytes'):
                if key in old:
                    row[key] = old[key]
    else:
        paths['extract_root'].mkdir(mode=0o700)
        extraction_rows = extract_package_set(archives, paths['extract_root'])
        rows_by_name = {row['name']: row for row in extraction_rows}
        for row in package_rows:
            row.update(rows_by_name[row['name']])
        atomic_json(marker, {'extraction_profile': EXTRACTION_PROFILE,
                             'package_sha256': expected_hashes})

    extracted_root = paths['extract_root'].resolve(strict=True)
    for relative in REQUIRED_PAYLOADS:
        target = paths['extract_root'] / relative
        require(target.exists(), f'Missing required SDK payload: {relative}')
        resolved = target.resolve(strict=True)
        require(resolved.is_relative_to(extracted_root),
                f'SDK payload resolves outside extraction root: {relative}')

    runtime_paths = {relative: str(paths['extract_root'] / relative)
                     for relative in REQUIRED_PAYLOADS}
    runtime_checks, unresolved = check_runtime(paths)
    manifest = {
        'schema': 1,
        'status': 'ready-for-isolated-review' if not unresolved
        else 'unresolved-runtime-dependencies',
        'extraction_profile': EXTRACTION_PROFILE,
        'preparer_sha256': sha256(Path(__file__).resolve()),
        'sdk_directory': str(SDK), 'extraction_root': str(paths['extract_root']),
        'keyring_source': str(KEYRING_SOURCE), 'keyring_source_sha256': sha256(KEYRING_SOURCE),
        'keyring_sha256': sha256(paths['keyring'] / 'archlinux.gpg'),
        'packages': package_rows,
        'runtime_paths': runtime_paths,
        'runtime_ldd_checks': runtime_checks,
        'unresolved_runtime_dependencies': unresolved,
        'omitted_declared_dependencies': {
            'fcitx5-chinese-addons': [
                'fcitx5-qt and qt6-webengine are declared by the full package but optional '
                'Qt editor/UI payloads are not extracted; libpinyin.so and its Pinyin '
                'descriptors/data plus its required punctuation addon/plugin/data are selected',
            ],
            'other host libraries': [
                'System /usr libraries can satisfy dependencies; exact ldd output is recorded. '
                'This is not a portable or ABI-independent sysroot.',
            ],
        },
        'limits': [
            'Signed pinned Arch payloads only; package installer hooks are not run',
            'No package installation, executable launch, Xvfb/Fcitx start, GUI or input occurred',
            'XTEST API/library is present but has not been queried against an X server',
            'No namespace/socket isolation has been established by SDK preparation',
            'A later isolated run must validate actual Fcitx plugin load and XIM Unicode commit',
            'Pinyin test payload is GPL-2.0-or-later and is test-only, not a product dependency',
        ],
    }
    atomic_json(paths['evidence'] / 'manifest.json', manifest)
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--plan', action='store_true',
                        help='print pinned package plan without creating files or networking')
    args = parser.parse_args()
    try:
        result = plan() if args.plan else prepare()
        print(json.dumps(result if args.plan else {
            'status': result['status'], 'manifest': str(SDK / 'evidence/manifest.json'),
            'extraction_root': result['extraction_root'],
            'unresolved_runtime_dependencies': result['unresolved_runtime_dependencies'],
        }, indent=2))
        return 0 if args.plan or result['status'] == 'ready-for-isolated-review' else 2
    except (RuntimeError, OSError, subprocess.SubprocessError, tarfile.TarError,
            UnicodeError, json.JSONDecodeError) as error:
        print(f'Preparation failed closed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
