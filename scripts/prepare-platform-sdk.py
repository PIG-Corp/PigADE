#!/usr/bin/env python3
"""Fetch signed, pinned headless-test prerequisites; never launch a display.

Writes only /tmp/pigade-platform-sdk. Existing system libraries are inspected
read-only with ldd after package signature verification. This is not a complete
portable sysroot, and does not certify rendering or namespace isolation.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import urllib.request

SDK = Path('/tmp/pigade-platform-sdk')
KEYRING = Path('/usr/share/pacman/keyrings/archlinux.gpg')
SIGNER = '83BC8889351B5DEBBB68416EB8AC08600F108CDF'
PACKAGES = (
    ('w/weston/weston-15.0.1-3-x86_64.pkg.tar.zst',
     '92fe0bfe2cf1ae02105b0e6573f6695c6bbd4637c9a6b20299a6fea8c3d23b4d',
     'MIT'),
    ('v/vulkan-swrast/vulkan-swrast-1:26.2.1-1-x86_64.pkg.tar.zst',
     '08b9775e4743a64a94fd9591095cf4895d7ae17c3d43c2ba08fb9c8a1a91e752',
     'MIT AND BSD-3-Clause AND SGI-B-2.0'),
)
MAX_DOWNLOAD = 32 * 1024 * 1024  # Two archives and two small signatures; <128 MiB.


def run(args: list[str], *, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(args, text=True, capture_output=True, env=env,
                            stdin=subprocess.DEVNULL, timeout=60, check=False)
    if result.returncode:
        raise RuntimeError(f'{args[0]} failed: {result.stdout}{result.stderr}')
    return result.stdout + result.stderr


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def download(url: str, path: Path) -> None:
    if path.exists():
        if path.is_symlink() or path.stat().st_size > MAX_DOWNLOAD:
            raise RuntimeError(f'Unsafe existing download: {path}')
        return
    request = urllib.request.Request(url, headers={'User-Agent': 'PigADE-SDK/0.0.1-dev'})
    with urllib.request.urlopen(request, timeout=30) as response:
        payload = response.read(MAX_DOWNLOAD + 1)
    if len(payload) > MAX_DOWNLOAD:
        raise RuntimeError(f'Download exceeds bound: {url}')
    temporary = path.with_name(path.name + '.partial')
    temporary.write_bytes(payload)
    temporary.replace(path)


def main() -> None:
    if SDK.is_symlink() or (SDK.exists() and SDK.stat().st_uid != os.getuid()):
        raise RuntimeError('SDK root must be owned by this user and not a symlink')
    SDK.mkdir(mode=0o700, exist_ok=True)
    for name in ('downloads', 'keyring', 'root', 'evidence'):
        path = SDK / name
        if path.is_symlink():
            raise RuntimeError(f'Refusing symlink directory: {path}')
        path.mkdir(mode=0o700, exist_ok=True)
    key = SDK / 'keyring/archlinux.gpg'
    # Arch's .gpg file is ASCII-armored here; gpgv requires a binary keyring.
    run(['gpg', '--homedir', str(SDK / 'keyring'), '--batch', '--yes',
         '--dearmor', '--output', str(key), str(KEYRING)])
    packages = []
    for relative, expected_hash, license_id in PACKAGES:
        url = 'https://archive.archlinux.org/packages/' + relative
        archive = SDK / 'downloads' / relative.rsplit('/', 1)[1]
        signature = archive.with_name(archive.name + '.sig')
        download(url, archive)
        download(url + '.sig', signature)
        if sha256(archive) != expected_hash:
            raise RuntimeError(f'Archive hash mismatch: {archive.name}')
        verification = run(['gpgv', '--homedir', str(SDK / 'keyring'),
                            '--keyring', str(key), '--status-fd', '1',
                            str(signature), str(archive)])
        if f'[GNUPG:] VALIDSIG {SIGNER} ' not in verification:
            raise RuntimeError('Unexpected package signer')
        (SDK / 'evidence' / (archive.name + '.signature.txt')).write_text(verification)
        metadata = run(['bsdtar', '-xOf', str(archive), '.PKGINFO'])
        (SDK / 'evidence' / (archive.name + '.PKGINFO')).write_text(metadata)
        for entry in run(['bsdtar', '-tf', str(archive)]).splitlines():
            path = PurePosixPath(entry)
            if path.is_absolute() or '..' in path.parts:
                raise RuntimeError(f'Unsafe archive path: {entry}')
        # Only package payload; installer scripts are neither extracted nor run.
        run(['bsdtar', '-xf', str(archive), '-C', str(SDK / 'root'),
             '--no-same-owner', '--exclude', '.PKGINFO', '--exclude', '.BUILDINFO',
             '--exclude', '.MTREE', '--exclude', '.INSTALL'])
        packages.append({'url': url, 'bytes': archive.stat().st_size,
                         'sha256': expected_hash, 'signature_sha256': sha256(signature),
                         'signer': SIGNER, 'license': license_id})

    root = SDK / 'root'
    lib = root / 'usr/lib'
    environment = {'PATH': '/usr/bin', 'LC_ALL': 'C',
                   'LD_LIBRARY_PATH': f'{lib}:{lib}/weston'}
    objects = ['usr/bin/weston', 'usr/bin/weston-screenshooter',
               'usr/lib/libvulkan_lvp.so', 'usr/lib/libweston-15/headless-backend.so',
               'usr/lib/weston/kiosk-shell.so']
    dependency_evidence = {}
    for relative in objects:
        output = run(['ldd', str(root / relative)], env=environment)
        (SDK / 'evidence' / (Path(relative).name + '.ldd.txt')).write_text(output)
        if 'not found' in output:
            raise RuntimeError(f'Missing runtime library: {relative}\n{output}')
        dependency_evidence[relative] = output
    icd = json.loads((root / 'usr/share/vulkan/icd.d/lvp_icd.json').read_text())
    icd['ICD']['library_path'] = str(lib / 'libvulkan_lvp.so')
    icd_path = SDK / 'lavapipe-icd.json'
    icd_path.write_text(json.dumps(icd, indent=2) + '\n')
    environment.update({
        'VK_DRIVER_FILES': str(icd_path),
        'VK_ICD_FILENAMES': str(icd_path),
        'WESTON_MODULE_MAP': ';'.join([
            f'headless-backend.so={lib}/libweston-15/headless-backend.so',
            f'kiosk-shell.so={lib}/weston/kiosk-shell.so',
        ]),
    })
    manifest = {
        'schema': 1, 'status': 'prepared-not-executed', 'sdk': str(SDK),
        'packages': packages, 'trusted_keyring_sha256': sha256(KEYRING),
        'weston': str(root / 'usr/bin/weston'),
        'screenshooter': str(root / 'usr/bin/weston-screenshooter'),
        'lavapipe_icd': str(icd_path), 'private_environment': environment,
        'dependency_evidence': dependency_evidence,
        'limits': ['System library ABI dependency; not a portable standalone sysroot',
                   'No compositor, Vulkan initialization, namespace, app or input executed',
                   'Use only after verified namespace/socket isolation; no live display fallback'],
    }
    (SDK / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(f'Prepared {SDK}/manifest.json; no display or application launched.')


if __name__ == '__main__':
    main()
