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
MAX_SOURCE_FILES = 3
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
    fail(manifest.get("profile") == "pigade-x11-ime-focus-v3", "unexpected patch profile")
    fail(manifest.get("previous_files_sha256") == {
        "crates/gpui_linux/src/linux/x11/xim_handler.rs":
            "7b91a6d4109622aea22ace00f452afbd7da704ac195e00cb68ee264059368f2a",
        "crates/gpui_linux/src/linux/x11/client.rs":
            "1086c3f44d6567360c74703d2101c7e3c24c89371c6fade921332f11e0b95b70",
        "crates/gpui/src/window.rs":
            "224c1f15e78b3baa128479dbabfa1512e67d2794d3ef4cf6ca9eaf64cbd2bd74",
    }, "profile migration must recognize the exact v2 postimages")
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
    xim_handler = next(row for row in manifest["files"]
                       if row["path"].endswith("xim_handler.rs"))
    handler_text = source.joinpath(*PurePosixPath(xim_handler["path"]).parts).read_text(
        encoding="utf-8")
    fail("InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NOTHING" in handler_text,
         "input context must select a complete preedit/status style pair")
    fail(".push(AttributeName::InputStyle, InputStyle::PREEDIT_CALLBACKS)" not in handler_text,
         "preedit callbacks must not be selected without a status style")
    fail("xim::read(bytes)" in handler_text and
         "xim::InputStyleList" in handler_text and
         "AttributeName::QueryInputStyle" in handler_text,
         "input contexts must be gated by the typed QueryInputStyle response")
    fail("QueryInputStyle was not returned" in handler_text and
         "PigADE: X11 IME unavailable: server does not advertise callback preedit" in handler_text,
         "unsupported or missing callback style must be reported and fail closed")
    xim_manifest_path = PROFILE.parent / "xim-preedit" / "manifest.json"
    xim_manifest = json.loads(xim_manifest_path.read_text(encoding="utf-8"))
    fail(xim_manifest.get("profile") == "pigade-x11-preedit-reply-v1" and
         xim_manifest.get("upstream_commit") ==
         "16f35a2c881b815a2b6cdfd6687988e84f8447d8",
         "pinned xim-rs preedit reply profile is missing")
    reply_patch = (xim_manifest_path.parent / xim_manifest["patches"][0]["path"]).read_text(
        encoding="utf-8")
    fail("Request::PreeditStartReply" in reply_patch and
         "return_value: -1" in reply_patch and
         "failed_preedit_start_sends_no_reply" in reply_patch,
         "xim-rs preedit-start reply patch or its failure-path test is missing")
    client_entry = next(row for row in manifest["files"]
                        if row["path"].endswith("client.rs"))
    client_text = source.joinpath(*PurePosixPath(client_entry["path"]).parts).read_text(
        encoding="utf-8")
    style_pair = "xim::InputStyle::PREEDIT_CALLBACKS | xim::InputStyle::STATUS_NOTHING"
    fail(client_text.count(style_pair) == 2,
         "both live XIC style updates must keep the complete preedit/status style pair")
    fail("xim::InputStyle::PREEDIT_CALLBACKS," not in client_text,
         "client XIC updates must not select preedit callbacks without a status style")
    xdnd_setter = client_text.find("state.xcb_connection.change_property32(")
    fail(xdnd_setter >= 0, "XdndAware property setter is missing")
    xdnd_block = client_text[xdnd_setter:xdnd_setter + 600]
    fail("state.atoms.XdndAware" in xdnd_block and
         "xproto::AtomEnum::ATOM" in xdnd_block and
         "state.atoms.XA_ATOM" not in xdnd_block,
         "XdndAware property type must use the predefined X11 ATOM")
    selection_start = client_text.find("Event::SelectionNotify(event)")
    selection_end = client_text.find("Event::ConfigureNotify(event)", selection_start)
    fail(selection_start >= 0 and selection_end > selection_start,
         "X11 selection-notify handler is missing")
    selection_block = client_text[selection_start:selection_end]
    fail("16 * 1024" in selection_block and "reply.bytes_after != 0" in selection_block,
         "X11 URI-list reads must be bounded to 64 KiB and reject truncation")
    fail(".filter(|path| !path.is_empty() && !path.starts_with('#'))" in selection_block and
         ".collect::<Option<SmallVec<[_; 2]>>>()" in selection_block and
         "Url::parse(path).ok().and_then(|url| url.to_file_path().ok())" in selection_block and
         ".filter_map(" not in selection_block and
         "let drop_success = !paths.is_empty();" in selection_block and
         "state.xdnd_state.drop_success = drop_success;" in selection_block,
         "external URI lists must be all-or-nothing and mark only nonempty decoded paths successful")
    xdnd_start = client_text.find("pub struct Xdnd {")
    xdnd_end = client_text.find("\n}", xdnd_start)
    fail(xdnd_start >= 0 and xdnd_end > xdnd_start and
         "drop_success: bool" in client_text[xdnd_start:xdnd_end],
         "XDND drop validity must be tracked for the active transfer")
    enter_start = client_text.find("event.type_ == state.atoms.XdndEnter")
    enter_end = client_text.find("event.type_ == state.atoms.XdndLeave", enter_start)
    fail(enter_start >= 0 and enter_end > enter_start and
         "state.xdnd_state.drop_success = false;" in client_text[enter_start:enter_end],
         "a new XDND transfer must not inherit an earlier result")
    drop_start = client_text.find("event.type_ == state.atoms.XdndDrop")
    drop_end = client_text.find("Event::SelectionNotify(event)", drop_start)
    fail(drop_start >= 0 and drop_end > drop_start and
         "state.xdnd_state.drop_success" in client_text[drop_start:drop_end],
         "XdndDrop must report the validity of its received URI list")
    finished_start = client_text.find("fn xdnd_send_finished(")
    finished_end = client_text.find("fn xdnd_send_status(", finished_start)
    fail(finished_start >= 0 and finished_end > finished_start,
         "XdndFinished sender is missing")
    finished_block = client_text[finished_start:finished_end]
    fail("success: bool" in finished_block and
         "if success { 1 } else { 0 }" in finished_block and
         "if success { atoms.XdndActionCopy } else { 0 }" in finished_block,
         "invalid URI lists must produce failed XdndFinished with no action")
    window_entry = next(row for row in manifest["files"]
                        if row["path"].endswith("/window.rs"))
    window_text = source.joinpath(*PurePosixPath(window_entry["path"]).parts).read_text(
        encoding="utf-8")
    modality_start = window_text.find("// Track input modality for focus-visible styling and hover suppression.")
    modality_end = window_text.find("if self.last_input_modality != old_modality", modality_start)
    fail(modality_start >= 0 and modality_end > modality_start,
         "window input-modality classifier is missing")
    modality_block = window_text[modality_start:modality_end]
    fail("PlatformInput::FileDrop(" in modality_block and
         "FileDropEvent::Entered { .. }" in modality_block and
         "FileDropEvent::Pending { .. }" in modality_block and
         "FileDropEvent::Submit { .. }" in modality_block and
         modality_block.count("InputModality::Mouse") == 2 and
         "FileDropEvent::Exited" not in modality_block and
         "FileDropEvent::Ended" not in modality_block,
         "active external file-drop events must enter mouse modality without changing exit/end modality")
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
