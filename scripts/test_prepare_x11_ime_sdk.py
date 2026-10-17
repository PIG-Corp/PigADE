#!/usr/bin/env python3
"""Focused fail-closed tests for the isolated X11 IME SDK preparer."""
from __future__ import annotations

import importlib.util
import io
from pathlib import Path
import tempfile
import tarfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name('prepare-x11-ime-sdk.py')
SPEC = importlib.util.spec_from_file_location('prepare_x11_ime_sdk', SCRIPT)
assert SPEC is not None and SPEC.loader is not None
sdk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sdk)


def archive_with(members: list[tuple[tarfile.TarInfo, bytes | None]]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w') as bundle:
        for member, payload in members:
            if payload is None:
                bundle.addfile(member)
            else:
                member.size = len(payload)
                bundle.addfile(member, io.BytesIO(payload))
    return buffer.getvalue()


class PrepareX11ImeSdkTests(unittest.TestCase):
    def test_package_pins_are_exact_and_bounded(self) -> None:
        self.assertEqual(len(sdk.PACKAGES), 13)
        self.assertEqual(len({row['name'] for row in sdk.PACKAGES}), len(sdk.PACKAGES))
        for row in sdk.PACKAGES:
            self.assertEqual(len(row['sha256']), 64)
            self.assertTrue(all(char in '0123456789abcdef' for char in row['sha256']))
            self.assertEqual(len(row['signer']), 40)
            self.assertIn('.pkg.tar.zst', sdk.package_filename(row))

    def test_plan_is_read_only_and_discloses_pins(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / 'must-not-exist'
            original = sdk.SDK
            sdk.SDK = destination
            try:
                plan = sdk.plan()
            finally:
                sdk.SDK = original
            self.assertEqual(plan['status'], 'plan-only-no-downloads-no-writes')
            self.assertEqual(len(plan['packages']), 13)
            self.assertEqual(plan['extraction_profile'], sdk.EXTRACTION_PROFILE)
            self.assertFalse(destination.exists())

    def test_extraction_profile_participates_in_cache_identity(self) -> None:
        original = sdk.EXTRACTION_PROFILE
        try:
            first = sdk.package_set_id()
            sdk.EXTRACTION_PROFILE = original + '-changed'
            second = sdk.package_set_id()
        finally:
            sdk.EXTRACTION_PROFILE = original
        self.assertNotEqual(first, second)

    def test_pinyin_required_punctuation_addon_is_in_selected_and_required_closure(self) -> None:
        paths = {
            'usr/lib/fcitx5/libpinyin.so': b'pinyin plugin',
            'usr/share/fcitx5/addon/pinyin.conf': (
                b'[Addon/Dependencies]\n0=core:5.1.22\n1=punctuation:5.1.15\n'),
            'usr/share/fcitx5/inputmethod/pinyin.conf': b'[InputMethod]\nAddon=pinyin\n',
            'usr/share/fcitx5/pinyin/symbols': b'punctuation map',
            'usr/share/fcitx5/pinyin/chaizi.dict': b'dictionary',
            'usr/lib/fcitx5/libpunctuation.so': b'punctuation plugin',
            'usr/share/fcitx5/addon/punctuation.conf': b'[Addon]\nName=Punctuation\n',
            'usr/share/fcitx5/punctuation/punc.mb.zh_CN': '. 。\n, ，\n'.encode(),
        }
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            archive = base / 'addons.tar'
            archive.write_bytes(archive_with([
                (tarfile.TarInfo(path), payload) for path, payload in paths.items()
            ]))
            root = base / 'root'
            root.mkdir()
            result = sdk.extract_package(archive, root, 'fcitx5-chinese-addons')
            self.assertEqual(result['selected_members'], len(paths))
            self.assertEqual({path for path in paths if (root / path).is_file()}, set(paths))

        pinyin_conf = paths['usr/share/fcitx5/addon/pinyin.conf'].decode()
        self.assertIn('1=punctuation:5.1.15', pinyin_conf)
        for path in (
            'usr/lib/fcitx5/libpunctuation.so',
            'usr/share/fcitx5/addon/punctuation.conf',
            'usr/share/fcitx5/punctuation/punc.mb.zh_CN',
        ):
            self.assertIn(path, sdk.SELECTED_FILES['fcitx5-chinese-addons'])
            self.assertIn(path, sdk.REQUIRED_PAYLOADS)
        self.assertIn('usr/lib/fcitx5/libpunctuation.so', sdk.RUNTIME_OBJECTS)

    def test_signature_parser_requires_one_expected_validsig(self) -> None:
        signer = 'A' * 40
        valid = ('[GNUPG:] VALIDSIG ' + signer +
                 ' 2026-10-17 1792006169 0 4 0 1 10 00 ' + signer)
        self.assertEqual(sdk.verify_status(valid, signer)['primary_fingerprint'], signer)
        for output in ('[GNUPG:] GOODSIG 1234', valid + '\n' + valid):
            with self.subTest(output=output[:32]), self.assertRaises(RuntimeError):
                sdk.verify_status(output, signer)
        with self.assertRaisesRegex(RuntimeError, 'Unexpected package signer'):
            sdk.verify_status(valid, 'B' * 40)

    def test_gpgv_nonzero_exit_is_never_accepted(self) -> None:
        failed = mock.Mock(returncode=1, stdout='[GNUPG:] VALIDSIG ' + 'A' * 40,
                           stderr='bad signature')
        with mock.patch.object(sdk.subprocess, 'run', return_value=failed):
            with self.assertRaisesRegex(RuntimeError, 'verification failed'):
                sdk.verify_signature(Path('signature'), Path('archive'),
                                     Path('keyring'), Path('gnupg'), 'A' * 40)

    def test_archive_path_traversal_and_escaping_symlink_fail(self) -> None:
        traversal = tarfile.TarInfo('../../outside')
        traversal.size = 1
        with self.assertRaisesRegex(RuntimeError, 'Unsafe archive path'):
            sdk.validate_members([traversal])
        with self.assertRaisesRegex(RuntimeError, 'symlink escapes'):
            sdk.safe_symlink_target('usr/share/X11/xkb', '/../../etc/passwd')
        with self.assertRaisesRegex(RuntimeError, 'Symlink escapes'):
            sdk.safe_symlink_target('usr/share/X11/xkb', '../../../../outside')

    def test_extract_rejects_path_traversal_without_writing_outside(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / 'root'
            root.mkdir()
            archive = base / 'bad.tar'
            entry = tarfile.TarInfo('../outside')
            archive.write_bytes(archive_with([(entry, b'x')]))
            with self.assertRaisesRegex(RuntimeError, 'Unsafe archive path'):
                sdk.extract_package(archive, root, 'test')
            self.assertFalse((base / 'outside').exists())
            self.assertEqual(list(root.iterdir()), [])

    def test_internal_absolute_package_symlink_is_rebased_inside_sdk(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / 'root'
            root.mkdir()
            dirs = []
            for name in ('usr', 'usr/share', 'usr/share/X11', 'usr/share/xkbdata'):
                entry = tarfile.TarInfo(name)
                entry.type = tarfile.DIRTYPE
                entry.mode = 0o755
                dirs.append((entry, None))
            link = tarfile.TarInfo('usr/share/X11/xkb')
            link.type = tarfile.SYMTYPE
            link.linkname = '/usr/share/xkbdata'
            archive = base / 'good.tar'
            archive.write_bytes(archive_with(dirs + [(link, None)]))
            sdk.extract_package(archive, root, 'test')
            self.assertEqual((root / 'usr/share/X11/xkb').readlink(), Path('../xkbdata'))
            self.assertEqual((root / 'usr/share/X11/xkb').resolve(),
                             (root / 'usr/share/xkbdata').resolve())

    def test_unsupported_archive_entries_fail_before_extraction(self) -> None:
        fifo = tarfile.TarInfo('usr/bin/unexpected')
        fifo.type = tarfile.FIFOTYPE
        with self.assertRaisesRegex(RuntimeError, 'Unsupported archive entry type'):
            sdk.validate_members([fifo])

    def test_extracted_size_limit_is_checked_before_writing(self) -> None:
        oversized = tarfile.TarInfo('usr/share/oversized')
        oversized.size = sdk.MAX_EXTRACTED_BYTES + 1
        with self.assertRaisesRegex(RuntimeError, 'uncompressed extraction size limit'):
            sdk.validate_members([oversized])


if __name__ == '__main__':
    unittest.main()
