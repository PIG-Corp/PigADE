#!/usr/bin/env python3
"""Prepare a signed, pinned Sway/Fcitx Wayland-IME test SDK; never launch it.

The SDK is a host-ABI test payload under /tmp only. It does not install packages,
run package hooks, start a compositor/Fcitx/application, create namespaces, or
send input. Runtime library resolution is recorded against the current host.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import PurePosixPath


SDK = Path('/tmp/pigade-wayland-ime-sdk-20261017')
KEYRING_SOURCE = Path('/usr/share/pacman/keyrings/archlinux.gpg')
X11_SDK = Path('/tmp/pigade-x11-ime-sdk-20261017')
X11_FCITX_ARCHIVE = X11_SDK / 'downloads/fcitx5-5.1.23-1-x86_64.pkg.tar.zst'
X11_MANIFEST = X11_SDK / 'evidence/manifest.json'
MIRROR = 'https://archive.archlinux.org/packages'
EXTRACTION_PROFILE = 'sway-1.12-wlroots-0.20-fcitx-wayland-v2-tools'
MAX_PACKAGE_BYTES = 12 * 1024 * 1024
MAX_SIGNATURE_BYTES = 1024 * 1024
MAX_TOTAL_BYTES = 20 * 1024 * 1024
MAX_EXTRACTED_BYTES = 96 * 1024 * 1024
MAX_EVIDENCE_BYTES = 8 * 1024 * 1024


def package(name: str, version: str, arch: str, digest: str,
            signer: str, license_id: str) -> dict[str, str]:
    return {'name': name, 'version': version, 'arch': arch, 'sha256': digest,
            'signer': signer, 'license': license_id}


PACKAGES = (
    package('sway', '1:1.12-4', 'x86_64',
            '5704cfc9eba9804116c665cb4450352bf5a240c1a8300b36aca3218116648443',
            'BE2DBCF2B1E3E588AC325AEAA06B49470F8E620A', 'MIT'),
    package('wlroots0.20', '0.20.2-1', 'x86_64',
            '8b1da3fbf29cc45908d8de771bd22a5813b298a9049462df5b8952f23c3ffb8f',
            '262A58EC6C51F7EA395B2E2DFDC3040B92ACA748', 'MIT'),
    package('libliftoff', '0.5.0-1', 'x86_64',
            '59cf08c21500673a14287b83b89b6db5389134cc24c97d9222c7cddefa639ccf',
            '95220BE99CE6FF778AE0DC670F65C7D881506130', 'MIT'),
    package('wayland-protocols', '1.49-1', 'any',
            '79efa08167c423c6bdbba0efeb05ccc0ef9172c930be96a5b93f8208bdab04a4',
            '83BC8889351B5DEBBB68416EB8AC08600F108CDF', 'MIT'),
    package('fcitx5', '5.1.23-1', 'x86_64',
            '37d26735c03a923a69bacd2ec950ebb26f2f89dcde4318be7e36550e676df9a7',
            'B5971F2C5C10A9A08C60030F786C63F330D7CB92',
            'LGPL-2.1-or-later AND Unicode-DFS-2016'),
    package('wayland-utils', '1.3.0-1', 'x86_64',
            'a79a3f1ffb59b0eeb1af11b88a44cb9e935623f0b98eee0f901378cd7f3ebd93',
            '9D74DF6F91B7BDABD5815CA84AC5588F941C2A25', 'MIT'),
    package('wtype', '0.4-2', 'x86_64',
            '36ce49003b9e741b85463574d3ab35316bd27d0ef950db0e1f8540d75d68409d',
            '04DC3FB1445FECA813C27EFAEA4F7B321A906AD9', 'MIT'),
    package('grim', '1.5.0-2', 'x86_64',
            '2cbddb88d4a3316c61e8984203bbdca81cbf089c323e5db09c4e1648c483c8a6',
            '2E36D8620221482FC45CB7F2A91764759326B440', 'MIT'),
)

SELECTED_FILES = {
    'wayland-protocols': {
        'usr/share/wayland-protocols/unstable/text-input/text-input-unstable-v3.xml',
        'usr/share/licenses/wayland-protocols/COPYING',
    },
    'fcitx5': {
        'usr/lib/fcitx5/libwayland.so',
        'usr/lib/fcitx5/libwaylandim.so',
        'usr/share/fcitx5/addon/wayland.conf',
        'usr/share/fcitx5/addon/waylandim.conf',
        'usr/share/licenses/fcitx5/Unicode-DFS-2016.txt',
    },
    'wayland-utils': {'usr/bin/wayland-info', 'usr/share/licenses/wayland-utils/COPYING'},
    'wtype': {'usr/bin/wtype', 'usr/share/licenses/wtype/LICENSE'},
    'grim': {'usr/bin/grim', 'usr/share/licenses/grim/LICENSE'},
}

REQUIRED_PAYLOADS = (
    'usr/bin/sway',
    'usr/lib/libwlroots-0.20.so',
    'usr/share/licenses/sway/LICENSE',
    'usr/share/licenses/wlroots0.20/LICENSE',
    'usr/share/wayland-protocols/unstable/text-input/text-input-unstable-v3.xml',
    'usr/share/licenses/wayland-protocols/COPYING',
    'usr/lib/fcitx5/libwayland.so',
    'usr/lib/fcitx5/libwaylandim.so',
    'usr/share/fcitx5/addon/wayland.conf',
    'usr/share/fcitx5/addon/waylandim.conf',
    'usr/share/licenses/fcitx5/Unicode-DFS-2016.txt',
    'usr/bin/wayland-info', 'usr/share/licenses/wayland-utils/COPYING',
    'usr/bin/wtype', 'usr/share/licenses/wtype/LICENSE',
    'usr/bin/grim', 'usr/share/licenses/grim/LICENSE',
)
RUNTIME_OBJECTS = (
    'usr/bin/sway', 'usr/lib/libwlroots-0.20.so',
    'usr/lib/fcitx5/libwayland.so', 'usr/lib/fcitx5/libwaylandim.so',
    'usr/bin/wayland-info', 'usr/bin/wtype', 'usr/bin/grim',
)


def load_archive_helpers():
    path = Path(__file__).with_name('prepare-x11-ime-sdk.py')
    if path.is_symlink() or not path.is_file():
        raise RuntimeError('Existing bounded Arch archive helper is unavailable')
    spec = importlib.util.spec_from_file_location('pigade_x11_archive_helpers', path)
    if spec is None or spec.loader is None:
        raise RuntimeError('Cannot load bounded Arch archive helpers')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def archive_filename(item: dict[str, str]) -> str:
    return f"{item['name']}-{item['version']}-{item['arch']}.pkg.tar.zst"


def archive_url(item: dict[str, str]) -> str:
    filename = archive_filename(item)
    return f"{MIRROR}/{item['name'][0]}/{item['name']}/{filename}"


def package_set_id() -> str:
    payload = EXTRACTION_PROFILE + '\n' + ''.join(p['sha256'] for p in PACKAGES)
    return hashlib.sha256(payload.encode()).hexdigest()[:16]


def prepare_directories(helpers, directory: Path) -> dict[str, Path]:
    paths = helpers.create_private_tree(directory)
    paths['extract_root'] = paths['root'] / f'wayland-ime-{package_set_id()}'
    return paths


def copy_fcitx_archive(helpers, archive_path: Path, target: Path) -> None:
    helpers.require(archive_path.is_file() and not archive_path.is_symlink(),
                    f'Existing signed Fcitx archive unavailable: {archive_path}')
    helpers.assert_private_file(archive_path, limit=MAX_PACKAGE_BYTES)
    expected = next(p['sha256'] for p in PACKAGES if p['name'] == 'fcitx5')
    helpers.require(helpers.sha256(archive_path) == expected,
                    'Existing Fcitx archive does not match its Wayland SDK pin')
    signature = Path(str(archive_path) + '.sig')
    helpers.assert_private_file(signature, limit=MAX_SIGNATURE_BYTES)
    target_archive = target / archive_filename(next(p for p in PACKAGES if p['name'] == 'fcitx5'))
    target_signature = Path(str(target_archive) + '.sig')
    for source, destination in ((archive_path, target_archive), (signature, target_signature)):
        helpers.require(not destination.is_symlink(),
                        f'Refusing symlink copied archive target: {destination}')
        if destination.exists():
            helpers.assert_private_file(destination, limit=MAX_PACKAGE_BYTES)
            helpers.require(helpers.sha256(source) == helpers.sha256(destination),
                            f'Existing copied Fcitx payload differs: {destination.name}')
        else:
            shutil.copyfile(source, destination)
            destination.chmod(0o600)


def get_signed_packages(helpers, paths: dict[str, Path]) -> tuple[list[dict[str, object]], dict[str, Path]]:
    for executable in ('gpg', 'gpgv', 'ldd'):
        helpers.require(shutil.which(executable) is not None,
                        f'Required local executable not found: {executable}')
    helpers.require(KEYRING_SOURCE.is_file() and not KEYRING_SOURCE.is_symlink(),
                    f'Arch keyring is missing: {KEYRING_SOURCE}')
    keyring = paths['keyring'] / 'archlinux.gpg'
    gpg_home = paths['keyring'] / 'gnupg'
    helpers.require(not keyring.is_symlink() and not gpg_home.is_symlink(),
                    'Refusing symlink in private keyring directory')
    if not gpg_home.exists():
        gpg_home.mkdir(mode=0o700)
    helpers.require(gpg_home.is_dir() and gpg_home.stat().st_uid == os.getuid() and
                    stat.S_IMODE(gpg_home.stat().st_mode) == 0o700,
                    'Private GnuPG home must be user-owned and mode 0700')
    subprocess.run(['gpg', '--homedir', str(gpg_home), '--batch', '--yes',
                    '--dearmor', '--output', str(keyring), str(KEYRING_SOURCE)],
                   stdin=subprocess.DEVNULL, check=True, timeout=30,
                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    keyring.chmod(0o600)

    fcitx = next(p for p in PACKAGES if p['name'] == 'fcitx5')
    copy_fcitx_archive(helpers, X11_FCITX_ARCHIVE, paths['downloads'])
    archives: dict[str, Path] = {}
    rows: list[dict[str, object]] = []
    total = 0
    for item in PACKAGES:
        filename = archive_filename(item)
        archive = paths['downloads'] / filename
        signature = Path(str(archive) + '.sig')
        url = archive_url(item)
        if item['name'] != 'fcitx5':
            helpers.obtain_file(url, archive, MAX_PACKAGE_BYTES)
            helpers.obtain_file(url + '.sig', signature, MAX_SIGNATURE_BYTES)
        helpers.assert_private_file(archive, limit=MAX_PACKAGE_BYTES)
        helpers.assert_private_file(signature, limit=MAX_SIGNATURE_BYTES)
        digest = helpers.sha256(archive)
        helpers.require(digest == item['sha256'], f'Pinned archive hash mismatch: {filename}')
        verification = helpers.verify_signature(signature, archive, keyring, gpg_home,
                                                item['signer'])
        metadata = helpers.parse_package_info(archive)
        for key, expected in (('pkgname', item['name']), ('pkgver', item['version']),
                              ('arch', item['arch'])):
            helpers.require(metadata.get(key) == [expected],
                            f'Unexpected {key} in {filename}')
        licenses = metadata.get('license', [])
        helpers.require(licenses and all(token in ' '.join(licenses)
                                         for token in item['license'].split(' AND ')),
                        f'Unexpected license metadata in {filename}: {licenses}')
        total += archive.stat().st_size + signature.stat().st_size
        helpers.require(total <= MAX_TOTAL_BYTES, 'Pinned archive set exceeds byte bound')
        archives[item['name']] = archive
        rows.append({
            'name': item['name'], 'version': item['version'], 'architecture': item['arch'],
            'url': url,
            'archive_source': str(X11_FCITX_ARCHIVE) if item['name'] == 'fcitx5'
            else 'downloaded from the official Arch archive URL above',
            'archive_bytes': archive.stat().st_size, 'archive_sha256': digest,
            'signature_sha256': helpers.sha256(signature), 'expected_primary_signer': item['signer'],
            'verified_signer': verification, 'licenses': licenses,
            'declared_dependencies': metadata.get('depend', []),
            'extraction': 'wayland-frontend-only' if item['name'] == 'fcitx5'
            else ('text-input-v3-xml-only' if item['name'] == 'wayland-protocols'
                  else 'full-package'),
        })
    helpers.require(fcitx['sha256'] == next(row['archive_sha256'] for row in rows
                                            if row['name'] == 'fcitx5'),
                    'Fcitx archive did not match its pinned row')
    return rows, archives


def archive_inventory(helpers, packages: list[tuple[str, Path]],
                     selected_files: dict[str, set[str]]) -> dict[str, object]:
    """Derive the exact selected file/link inventory from the pinned archives."""
    original = dict(helpers.SELECTED_FILES)
    helpers.SELECTED_FILES.update(selected_files)
    inventory: dict[str, dict[str, object]] = {}
    extracted: list[dict[str, object]] = []
    try:
        for name, archive in packages:
            selected = helpers.SELECTED_FILES.get(name)
            with tarfile.open(archive, mode='r:*') as bundle:
                members, total = helpers.validate_members(bundle.getmembers())
                helpers.require(total <= MAX_EXTRACTED_BYTES,
                                'Package exceeds extracted-byte bound')
                files = [member for member in members
                         if PurePosixPath(member.name).name not in
                         {'.PKGINFO', '.BUILDINFO', '.MTREE', '.INSTALL'} and
                         (selected is None or helpers.normalized_member_name(member.name) in selected)]
                count = 0
                for member in files:
                    relative = helpers.normalized_member_name(member.name)
                    if member.isfile():
                        stream = bundle.extractfile(member)
                        helpers.require(stream is not None, f'Cannot read archive member: {relative}')
                        digest = hashlib.sha256()
                        size = 0
                        with stream:
                            while chunk := stream.read(1024 * 1024):
                                size += len(chunk)
                                helpers.require(size <= MAX_EXTRACTED_BYTES,
                                                f'Archive member exceeds bound: {relative}')
                                digest.update(chunk)
                        row = {'type': 'file', 'bytes': size, 'sha256': digest.hexdigest()}
                    elif member.issym():
                        row = {'type': 'symlink', 'target': member.linkname}
                    elif member.isdir():
                        continue
                    else:
                        raise RuntimeError(f'Unsupported selected archive member: {relative}')
                    helpers.require(relative not in inventory,
                                    f'Package payload path collision: {relative}')
                    inventory[relative] = row
                    count += 1
            extracted.append({'name': name, 'archive_members': len(members),
                              'selected_members': len(files),
                              'archive_uncompressed_bytes': total,
                              'payload_files': count})
    finally:
        helpers.SELECTED_FILES.clear()
        helpers.SELECTED_FILES.update(original)
    return {'rows': extracted, 'files': inventory}


def extract_packages(helpers, archives: dict[str, Path], root: Path) -> list[dict[str, object]]:
    original = dict(helpers.SELECTED_FILES)
    helpers.SELECTED_FILES.update(SELECTED_FILES)
    extracted = []
    total = 0
    try:
        for item in PACKAGES:
            result = helpers.extract_package(archives[item['name']], root, item['name'])
            total += result['archive_uncompressed_bytes']
            helpers.require(total <= MAX_EXTRACTED_BYTES,
                            'Selected package set exceeds extracted-byte bound')
            extracted.append({'name': item['name'], **result})
    finally:
        helpers.SELECTED_FILES.clear()
        helpers.SELECTED_FILES.update(original)
    return extracted


def verify_payload_inventory(root: Path, expected: dict[str, dict[str, object]]) -> None:
    """Fail closed if a reused or fresh extraction differs from signed bytes."""
    actual_paths: set[str] = set()
    for base, directories, files in os.walk(root, followlinks=False):
        base_path = Path(base)
        for name in list(directories):
            path = base_path / name
            if path.is_symlink():
                actual_paths.add(path.relative_to(root).as_posix())
                directories.remove(name)
        for name in files:
            path = base_path / name
            relative = path.relative_to(root).as_posix()
            if relative == '.package-set.json':
                continue
            actual_paths.add(relative)
    if actual_paths != set(expected):
        missing = sorted(set(expected) - actual_paths)[:3]
        unexpected = sorted(actual_paths - set(expected))[:3]
        raise RuntimeError(f'Extracted SDK file inventory differs; missing={missing}, unexpected={unexpected}')
    for relative, row in expected.items():
        path = root / relative
        if row['type'] == 'symlink':
            if not path.is_symlink() or os.readlink(path) != row['target']:
                raise RuntimeError(f'Extracted SDK symlink differs from signed archive: {relative}')
            resolved = path.resolve(strict=True)
            if not resolved.is_relative_to(root.resolve(strict=True)):
                raise RuntimeError(f'Extracted SDK symlink escapes root: {relative}')
        else:
            if path.is_symlink() or not path.is_file():
                raise RuntimeError(f'Extracted SDK file type differs from signed archive: {relative}')
            if path.stat().st_size != row['bytes'] or helpers_hash(path) != row['sha256']:
                raise RuntimeError(f'Extracted SDK bytes differ from signed archive: {relative}')


def helpers_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def normalize_ldd_output(output: str) -> str:
    return re.sub(r'0x[0-9a-fA-F]+', '0xADDR', output.strip())


def verified_x11_fcitx_root(sdk_dir: Path = X11_SDK,
                            manifest_path: Path = X11_MANIFEST) -> tuple[Path, dict[str, object]]:
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise RuntimeError(f'Existing verified Fcitx core profile is missing: {manifest_path}')
    if manifest_path.stat().st_uid != os.getuid():
        raise RuntimeError('Existing Fcitx manifest is not owned by this user')
    metadata = json.loads(manifest_path.read_text())
    if metadata.get('extraction_profile') != 'pinyin-punctuation-runtime-v2':
        raise RuntimeError('Existing Fcitx runtime profile identity changed')
    fcitx = next((row for row in metadata.get('packages', [])
                  if row.get('name') == 'fcitx5'), None)
    if not fcitx or fcitx.get('archive_sha256') != next(
            item['sha256'] for item in PACKAGES if item['name'] == 'fcitx5'):
        raise RuntimeError('Existing X11 profile does not contain the pinned Fcitx 5.1.23 package')
    root = Path(metadata.get('extraction_root', ''))
    if (root.is_symlink() or not root.is_dir() or root.stat().st_uid != os.getuid() or
            stat.S_IMODE(root.stat().st_mode) != 0o700):
        raise RuntimeError(f'Existing Fcitx runtime root is unavailable: {root}')
    if not root.resolve(strict=True).is_relative_to((sdk_dir / 'root').resolve(strict=True)):
        raise RuntimeError('Existing X11 Fcitx extraction escapes its SDK root')
    x11_preparer = load_archive_helpers()
    package_preparer_path = Path(__file__).with_name('prepare-x11-ime-sdk.py')
    spec = importlib.util.spec_from_file_location('pigade_x11_sdk_preparer', package_preparer_path)
    if spec is None or spec.loader is None:
        raise RuntimeError('Cannot load exact X11 SDK package pins')
    x11_pins = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(x11_pins)
    expected_packages = {item['name']: item for item in x11_pins.PACKAGES}
    rows = metadata.get('packages', [])
    if {row.get('name') for row in rows} != set(expected_packages):
        raise RuntimeError('Existing X11 profile package set differs from its pin list')
    keyring = sdk_dir / 'keyring/archlinux.gpg'
    keyhome = sdk_dir / 'keyring/gnupg'
    x11_preparer.assert_private_file(keyring, limit=16 * 1024 * 1024)
    archives: list[tuple[str, Path]] = []
    for row in rows:
        pin = expected_packages[row['name']]
        if row.get('archive_sha256') != pin['sha256']:
            raise RuntimeError(f"Existing X11 package pin differs: {row['name']}")
        archive = sdk_dir / 'downloads' / x11_pins.package_filename(pin)
        signature = Path(str(archive) + '.sig')
        x11_preparer.assert_private_file(archive, limit=x11_pins.MAX_PACKAGE_BYTES)
        x11_preparer.assert_private_file(signature, limit=x11_pins.MAX_SIGNATURE_BYTES)
        if x11_preparer.sha256(archive) != pin['sha256']:
            raise RuntimeError(f"Existing X11 archive hash differs: {row['name']}")
        if x11_preparer.sha256(signature) != row.get('signature_sha256'):
            raise RuntimeError(f"Existing X11 signature hash differs: {row['name']}")
        verification = x11_preparer.verify_signature(signature, archive, keyring, keyhome,
                                                     pin['signer'])
        if verification['primary_fingerprint'] != row.get('verified_signer', {}).get('primary_fingerprint'):
            raise RuntimeError(f"Existing X11 signer differs: {row['name']}")
        archives.append((row['name'], archive))
    inventory = archive_inventory(x11_preparer, archives, x11_pins.SELECTED_FILES)
    verify_payload_inventory(root, inventory['files'])
    for relative in x11_pins.REQUIRED_PAYLOADS:
        target = root / relative
        if not target.exists() or not target.resolve(strict=True).is_relative_to(root.resolve(strict=True)):
            raise RuntimeError(f'Existing Fcitx profile lacks verified runtime: {relative}')
    metadata['_verified_payload_inventory'] = inventory
    return root, metadata


def resolve_runtime(paths: dict[str, Path], x11_root: Path) -> tuple[dict[str, object], list[str]]:
    root = paths['extract_root']
    for relative in REQUIRED_PAYLOADS:
        target = root / relative
        if not target.exists():
            raise RuntimeError(f'Missing required Wayland IME SDK payload: {relative}')
        if not target.resolve(strict=True).is_relative_to(root.resolve(strict=True)):
            raise RuntimeError(f'Wayland SDK payload escapes extraction root: {relative}')
    environment = {
        'PATH': '/usr/bin', 'LC_ALL': 'C',
        'LD_LIBRARY_PATH': ':'.join((
            str(root / 'usr/lib'), str(root / 'usr/lib/fcitx5'),
            str(x11_root / 'usr/lib'), str(x11_root / 'usr/lib/fcitx5'))),
    }
    checks: dict[str, object] = {}
    unresolved: list[str] = []
    for relative in RUNTIME_OBJECTS:
        target = root / relative
        result = subprocess.run(['ldd', '-r', str(target)], text=True, capture_output=True,
                                stdin=subprocess.DEVNULL, env=environment,
                                timeout=45, check=False)
        # ldd address-space load addresses vary on every process invocation;
        # retain dependency resolution while normalizing those non-evidence values.
        output = normalize_ldd_output(result.stdout + result.stderr)
        missing = [line.strip() for line in output.splitlines()
                   if 'not found' in line or 'undefined symbol:' in line]
        checks[relative] = {'returncode': result.returncode, 'output': output,
                            'unresolved': missing}
        if result.returncode:
            unresolved.append(f'{relative}: ldd exited {result.returncode}')
        unresolved.extend(f'{relative}: {row}' for row in missing)
    return {'environment': environment, 'checks': checks}, unresolved


def plan() -> dict[str, object]:
    return {
        'status': 'plan-only-no-downloads-no-writes', 'sdk': str(SDK),
        'extraction_profile': EXTRACTION_PROFILE,
        'packages': [{'name': p['name'], 'version': p['version'], 'arch': p['arch'],
                      'sha256': p['sha256'], 'primary_signer': p['signer'],
                      'url': archive_url(p)} for p in PACKAGES],
        'limits': ['No package manager, install hooks, compositor, Fcitx, app, or input',
                   'Archive payloads selected from signatures and fixed member profile',
                   'Host /usr is runtime dependency scope, not portable sysroot'],
    }


def prepare() -> dict[str, object]:
    helpers = load_archive_helpers()
    paths = prepare_directories(helpers, SDK)
    rows, archives = get_signed_packages(helpers, paths)
    x11_root, x11_metadata = verified_x11_fcitx_root(X11_SDK, X11_MANIFEST)
    marker = paths['extract_root'] / '.package-set.json'
    expected_marker = {'extraction_profile': EXTRACTION_PROFILE,
                       'package_sha256': [p['sha256'] for p in PACKAGES]}
    inventory = archive_inventory(helpers,
                                  [(item['name'], archives[item['name']]) for item in PACKAGES],
                                  SELECTED_FILES)
    if paths['extract_root'].exists():
        helpers.require(paths['extract_root'].is_dir() and
                        not paths['extract_root'].is_symlink(),
                        'Unsafe existing extraction root')
        info = paths['extract_root'].stat()
        helpers.require(info.st_uid == os.getuid() and
                        stat.S_IMODE(info.st_mode) == 0o700,
                        'Existing extraction root must be private and user-owned')
        helpers.require(marker.is_file() and not marker.is_symlink(),
                        'Existing extraction root lacks package/profile marker')
        helpers.assert_private_file(marker, limit=1024 * 1024)
        helpers.require(json.loads(marker.read_text()) == expected_marker,
                        'Existing extraction root belongs to another package/profile set')
    else:
        paths['extract_root'].mkdir(mode=0o700)
        extraction_rows = extract_packages(helpers, archives, paths['extract_root'])
        helpers.atomic_json(marker, expected_marker)
        comparable = [{key: row[key] for key in
                       ('name', 'archive_members', 'selected_members', 'archive_uncompressed_bytes')}
                      for row in extraction_rows]
        expected_comparable = [{key: row[key] for key in
                                ('name', 'archive_members', 'selected_members', 'archive_uncompressed_bytes')}
                               for row in inventory['rows']]
        if comparable != expected_comparable:
            raise RuntimeError('Fresh extraction provenance differs from signed archive inventory')

    verify_payload_inventory(paths['extract_root'], inventory['files'])

    runtime, unresolved = resolve_runtime(paths, x11_root)
    manifest = {
        'schema': 1, 'status': 'prepared-not-executed' if not unresolved
        else 'unresolved-runtime-dependencies',
        'extraction_profile': EXTRACTION_PROFILE,
        'preparer_sha256': helpers.sha256(Path(__file__).resolve()),
        'shared_archive_helper_sha256': helpers.sha256(
            Path(__file__).with_name('prepare-x11-ime-sdk.py').resolve()),
        'sdk_directory': str(SDK), 'extraction_root': str(paths['extract_root']),
        'keyring_source': str(KEYRING_SOURCE),
        'keyring_source_sha256': helpers.sha256(KEYRING_SOURCE),
        'keyring_sha256': helpers.sha256(paths['keyring'] / 'archlinux.gpg'),
        'fcitx_core_runtime_base': {
            'manifest': str(X11_MANIFEST),
            'manifest_sha256': helpers.sha256(X11_MANIFEST),
            'extraction_profile': x11_metadata['extraction_profile'],
            'extraction_root': str(x11_root),
            'package_sha256': next(row['archive_sha256']
                                   for row in x11_metadata['packages']
                                   if row['name'] == 'fcitx5'),
        },
        'packages': rows, 'extraction': inventory['rows'],
        'payload_inventory': inventory['files'],
        'fcitx_core_payload_inventory': x11_metadata['_verified_payload_inventory']['files'],
        'required_payloads': list(REQUIRED_PAYLOADS),
        'runtime_ldd': runtime, 'unresolved_runtime_dependencies': unresolved,
        'protocol_evidence': {
            'pinned_gpui': 'zwp_text_input_v3 client in GPUI source pin 322cd509dcccaee1f6bc44725d270598ae8171ad',
            'sway_source': 'Sway 1.12 creates wlr_text_input_manager_v3 and wlr_input_method_manager_v2',
            'fcitx_waylandim': 'Fcitx 5.1.23 Wayland frontend declares dependency wayland:5.1.23',
            'status': 'source/package evidence only; no protocol globals queried',
        },
        'limits': [
            'Package signatures verified before extraction; installer hooks are not executed',
            'No system installation, compositor/Fcitx/application launch, namespace, or input occurred',
            'Runtime ldd resolves against this host /usr and is not a portable sysroot proof',
            'No text-input-v3/input-method-v2 handshake, Pinyin preedit/commit, focus, or isolation acceptance yet',
            'Wayland popup/candidate positioning is explicitly outside this SDK claim',
        ],
    }
    helpers.atomic_json(paths['evidence'] / 'manifest.json', manifest)
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--plan', action='store_true',
                        help='print pins without creating files or networking')
    args = parser.parse_args()
    try:
        result = plan() if args.plan else prepare()
        if args.plan:
            print(json.dumps(result, indent=2))
            return 0
        print(json.dumps({
            'status': result['status'],
            'manifest': str(SDK / 'evidence/manifest.json'),
            'extraction_root': result['extraction_root'],
            'unresolved_runtime_dependencies': result['unresolved_runtime_dependencies'],
        }, indent=2))
        return 0 if result['status'] == 'prepared-not-executed' else 2
    except (RuntimeError, OSError, subprocess.SubprocessError, tarfile.TarError,
            UnicodeError, json.JSONDecodeError) as error:
        print(f'Preparation failed closed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
