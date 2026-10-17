#!/usr/bin/env python3
"""Verify the tracked GPUI X11 IME focus patch and run its pure state tests."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "crates/app/patches/gpui-x11-ime"
MANIFEST = PROFILE / "manifest.json"
MAX_ARCHIVE_FILE = 1024 * 1024
MAX_PATCH = 256 * 1024
MAX_SOURCE_FILES = 2
STATE_BEGIN = "// BEGIN PIGADE XIM FOCUS STATE"
STATE_END = "// END PIGADE XIM FOCUS STATE"
TEST_BEGIN = "// BEGIN PIGADE XIM FOCUS TESTS"
TEST_END = "// END PIGADE XIM FOCUS TESTS"
DEFAULT_ARCHIVE = Path(os.environ.get(
    'PIGADE_GPUI_CACHE',
    str(Path(os.environ.get('XDG_CACHE_HOME', str(Path.home() / '.cache'))) / 'pigade/gpui'),
)) / '322cd509dcccaee1f6bc44725d270598ae8171ad.tar.gz'


class CheckError(RuntimeError):
    pass


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def fail(condition: bool, message: str) -> None:
    if not condition:
        raise CheckError(message)


def extract_sources(archive: Path, manifest: dict, destination: Path) -> None:
    expected_archive_hash = manifest["upstream_archive_sha256"]
    digest = hashlib.sha256()
    with archive.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    fail(digest.hexdigest() == expected_archive_hash, "pinned upstream archive SHA-256 mismatch")
    commit = manifest["upstream_commit"]
    prefix = f"zed-{commit}/"
    with tarfile.open(archive, "r:gz") as tar:
        for entry in manifest["files"]:
            relative = PurePosixPath(entry["path"])
            fail(not relative.is_absolute() and ".." not in relative.parts,
                 "manifest source path escapes upstream root")
            member_name = prefix + relative.as_posix()
            member = tar.getmember(member_name)
            fail(member.isfile() and member.size <= MAX_ARCHIVE_FILE,
                 f"unexpected upstream member type/size: {relative}")
            stream = tar.extractfile(member)
            fail(stream is not None, f"cannot read upstream member: {relative}")
            data = stream.read(MAX_ARCHIVE_FILE + 1)
            fail(len(data) == member.size and len(data) <= MAX_ARCHIVE_FILE,
                 f"upstream member exceeded bound: {relative}")
            fail(sha256(data) == entry["before_sha256"],
                 f"pinned source preimage mismatch: {relative}")
            target = destination.joinpath(*relative.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)


def apply_profile(archive: Path, temp: Path) -> tuple[dict, Path]:
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    fail(manifest.get("schema") == 1, "unsupported manifest schema")
    fail(manifest.get("profile") == "pigade-x11-ime-focus-v1", "unexpected patch profile")
    fail(len(manifest.get("files", [])) == MAX_SOURCE_FILES, "unexpected source file set")
    fail(len(manifest.get("patches", [])) == 1, "expected one focus patch")
    source = temp / "upstream"
    source.mkdir()
    extract_sources(archive, manifest, source)

    patch_entry = manifest["patches"][0]
    patch_path = PROFILE / patch_entry["path"]
    patch = patch_path.read_bytes()
    fail(len(patch) <= MAX_PATCH, "patch exceeds profile bound")
    fail(sha256(patch) == patch_entry["sha256"], "patch SHA-256 mismatch")
    patch_bin = shutil.which("patch")
    fail(patch_bin is not None, "GNU patch unavailable")
    result = subprocess.run(
        [patch_bin, "--batch", "--forward", "--fuzz=0", "-p1", "--directory", str(source)],
        input=patch,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=30,
        check=False,
    )
    fail(result.returncode == 0, f"patch failed: {result.stdout[-1000:].decode(errors='replace')}")
    for entry in manifest["files"]:
        data = source.joinpath(*PurePosixPath(entry["path"]).parts).read_bytes()
        fail(len(data) <= MAX_ARCHIVE_FILE, "patched source exceeds source bound")
        fail(sha256(data) == entry["after_sha256"], f"patched source hash mismatch: {entry['path']}")
    return manifest, source


def extract_test_harness(source: Path, temp: Path) -> Path:
    entry = next(row for row in json.loads(MANIFEST.read_text())["files"]
                 if row["path"].endswith("xim_handler.rs"))
    text = source.joinpath(*PurePosixPath(entry["path"]).parts).read_text(encoding="utf-8")
    ranges = []
    for start, end in ((STATE_BEGIN, STATE_END), (TEST_BEGIN, TEST_END)):
        fail(text.count(start) == 1 and text.count(end) == 1,
             f"embedded test marker missing or duplicated: {start}")
        a = text.index(start)
        b = text.index(end, a) + len(end)
        ranges.append(text[a:b])
    harness = temp / "focus_state_tests.rs"
    harness.write_text(
        "#![allow(dead_code)]\nmod xproto { pub type Window = u32; }\n"
        + "\n".join(ranges) + "\n",
        encoding="utf-8",
    )
    return harness


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, default=DEFAULT_ARCHIVE)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="pigade-gpui-focus-test-") as temp_dir:
        temp = Path(temp_dir)
        _manifest, source = apply_profile(args.archive, temp)
        harness = extract_test_harness(source, temp)
        rustc = shutil.which("rustc")
        fail(rustc is not None, "rustc unavailable")
        binary = temp / "focus-state-tests"
        subprocess.run([rustc, "--edition=2024", "--test", str(harness), "-o", str(binary)],
                       check=True, timeout=30)
        subprocess.run([str(binary)], check=True, timeout=30)
    print("GPUI X11 IME patch preimages/postimages and embedded focus-state tests passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (CheckError, OSError, subprocess.SubprocessError, json.JSONDecodeError,
            tarfile.TarError) as error:
        raise SystemExit(f"test_gpui_xim_focus: {type(error).__name__}: {error}")
