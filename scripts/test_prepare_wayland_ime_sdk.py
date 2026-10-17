#!/usr/bin/env python3
"""Focused safety and reproducibility tests for the Wayland IME SDK preparer."""
from __future__ import annotations

import importlib.util
import io
import hashlib
import os
from pathlib import Path
import tarfile
import tempfile
import unittest


SCRIPT = Path(__file__).with_name('prepare-wayland-ime-sdk.py')
SPEC = importlib.util.spec_from_file_location('prepare_wayland_ime_sdk', SCRIPT)
assert SPEC is not None and SPEC.loader is not None
sdk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sdk)


def archive_with(files: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w') as archive:
        for name, data in files.items():
            member = tarfile.TarInfo(name)
            member.mode = 0o644
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    return buffer.getvalue()


class PrepareWaylandImeSdkTests(unittest.TestCase):
    def test_pins_are_exact_and_profile_identity_changes_with_payload(self) -> None:
        self.assertEqual([p['name'] for p in sdk.PACKAGES],
                         ['sway', 'wlroots0.20', 'libliftoff',
                          'wayland-protocols', 'fcitx5', 'wayland-utils', 'wtype', 'grim'])
        self.assertEqual(len({p['name'] for p in sdk.PACKAGES}), len(sdk.PACKAGES))
        for item in sdk.PACKAGES:
            self.assertEqual(len(item['sha256']), 64)
            self.assertTrue(all(c in '0123456789abcdef' for c in item['sha256']))
            self.assertEqual(len(item['signer']), 40)
            self.assertTrue(sdk.archive_filename(item).endswith('.pkg.tar.zst'))
        original = sdk.EXTRACTION_PROFILE
        first = sdk.package_set_id()
        try:
            sdk.EXTRACTION_PROFILE = original + '-changed'
            second = sdk.package_set_id()
        finally:
            sdk.EXTRACTION_PROFILE = original
        self.assertNotEqual(first, second)

    def test_plan_is_non_mutating_and_discloses_exact_payload(self) -> None:
        original = sdk.SDK
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / 'untouched'
            sdk.SDK = destination
            try:
                result = sdk.plan()
            finally:
                sdk.SDK = original
        self.assertEqual(result['status'], 'plan-only-no-downloads-no-writes')
        self.assertEqual([row['name'] for row in result['packages']],
                         [row['name'] for row in sdk.PACKAGES])
        self.assertFalse(destination.exists())

    def test_extraction_keeps_only_pinned_fcitx_wayland_frontend_files(self) -> None:
        helpers = sdk.load_archive_helpers()
        payloads = {
            'usr/lib/fcitx5/libwayland.so': b'wayland module',
            'usr/lib/fcitx5/libwaylandim.so': b'wayland frontend',
            'usr/share/fcitx5/addon/wayland.conf': b'[Addon]\nVersion=5.1.23\n',
            'usr/share/fcitx5/addon/waylandim.conf': b'[Addon]\nVersion=5.1.23\n',
            'usr/share/licenses/fcitx5/Unicode-DFS-2016.txt': b'license text',
            'usr/lib/fcitx5/libxim.so': b'unselected X11 module',
        }
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            archive = base / 'fcitx.tar'
            archive.write_bytes(archive_with(payloads))
            root = base / 'root'
            root.mkdir()
            original = dict(helpers.SELECTED_FILES)
            helpers.SELECTED_FILES.update(sdk.SELECTED_FILES)
            try:
                extracted = helpers.extract_package(archive, root, 'fcitx5')
            finally:
                helpers.SELECTED_FILES.clear()
                helpers.SELECTED_FILES.update(original)
            self.assertEqual(extracted['selected_members'], 5)
            for path in sdk.SELECTED_FILES['fcitx5']:
                self.assertTrue((root / path).is_file(), path)
            self.assertFalse((root / 'usr/lib/fcitx5/libxim.so').exists())

    def test_unsafe_archive_path_fails_before_any_extraction(self) -> None:
        helpers = sdk.load_archive_helpers()
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / 'root'
            root.mkdir()
            archive = base / 'traversal.tar'
            archive.write_bytes(archive_with({'../../outside': b'blocked'}))
            with self.assertRaisesRegex(RuntimeError, 'Unsafe archive path'):
                helpers.extract_package(archive, root, 'wayland-protocols')
            self.assertFalse((base / 'outside').exists())
            self.assertEqual(list(root.iterdir()), [])

    def test_archive_symlink_cannot_escape_private_root(self) -> None:
        helpers = sdk.load_archive_helpers()
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / 'root'
            root.mkdir()
            buffer = io.BytesIO()
            link = tarfile.TarInfo('usr/share/link')
            link.type = tarfile.SYMTYPE
            link.linkname = '../../../../etc/passwd'
            with tarfile.open(fileobj=buffer, mode='w') as archive:
                archive.addfile(link)
            path = base / 'escape.tar'
            path.write_bytes(buffer.getvalue())
            with self.assertRaisesRegex(RuntimeError, 'Symlink escapes'):
                helpers.extract_package(path, root, 'wayland-protocols')
            self.assertEqual(list(root.iterdir()), [])

    def test_signature_primary_must_match_pin(self) -> None:
        helpers = sdk.load_archive_helpers()
        signer = 'A' * 40
        valid = ('[GNUPG:] VALIDSIG ' + signer +
                 ' 2026-10-17 1792006169 0 4 0 1 10 00 ' + signer)
        self.assertEqual(helpers.verify_status(valid, signer)['primary_fingerprint'], signer)
        with self.assertRaisesRegex(RuntimeError, 'Unexpected package signer'):
            helpers.verify_status(valid, 'B' * 40)
        with self.assertRaisesRegex(RuntimeError, 'Expected one valid'):
            helpers.verify_status('[GNUPG:] GOODSIG ' + signer, signer)

    def test_archive_inventory_verifies_exact_files_links_and_rejects_tampering(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'root'
            root.mkdir()
            payload = b'pinned bytes\n'
            (root / 'payload').write_bytes(payload)
            os.symlink('payload', root / 'alias')
            expected = {
                'payload': {'type': 'file', 'bytes': len(payload),
                            'sha256': hashlib.sha256(payload).hexdigest()},
                'alias': {'type': 'symlink', 'target': 'payload'},
            }
            sdk.verify_payload_inventory(root, expected)
            (root / 'payload').write_bytes(b'tampered')
            with self.assertRaisesRegex(RuntimeError, 'bytes differ'):
                sdk.verify_payload_inventory(root, expected)

    def test_inventory_rejects_unexpected_missing_and_escaping_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / 'root'
            root.mkdir()
            expected = {'required': {'type': 'file', 'bytes': 1,
                                    'sha256': hashlib.sha256(b'x').hexdigest()}}
            (root / 'required').write_bytes(b'x')
            (root / 'extra').write_bytes(b'y')
            with self.assertRaisesRegex(RuntimeError, 'inventory differs'):
                sdk.verify_payload_inventory(root, expected)
            (root / 'extra').unlink()
            (root / 'required').unlink()
            with self.assertRaisesRegex(RuntimeError, 'inventory differs'):
                sdk.verify_payload_inventory(root, expected)
            os.symlink('/etc/passwd', root / 'required')
            expected_link = {'required': {'type': 'symlink', 'target': '/etc/passwd'}}
            with self.assertRaisesRegex(RuntimeError, 'escapes root'):
                sdk.verify_payload_inventory(root, expected_link)

    def test_runtime_evidence_normalizes_aslr_addresses(self) -> None:
        first = 'libexample.so => /usr/lib/libexample.so (0x7f0000123000)\n'
        second = 'libexample.so => /usr/lib/libexample.so (0x7ff0123abc00)\n'
        self.assertEqual(sdk.normalize_ldd_output(first), sdk.normalize_ldd_output(second))
        self.assertIn('/usr/lib/libexample.so', sdk.normalize_ldd_output(first))


if __name__ == '__main__':
    unittest.main()
