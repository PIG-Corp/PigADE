#!/usr/bin/env python3
"""Apply a hash-pinned downstream patch profile to one private GPUI checkout."""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile

MAX_MANIFEST_BYTES = 64 * 1024
MAX_PATCH_BYTES = 256 * 1024
MAX_FILES = 32
MAX_SOURCE_FILE_BYTES = 1024 * 1024
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_ARCHIVE_MEMBERS = 1_000_000
MAX_ARCHIVE_EXPANDED_BYTES = 4 * 1024 * 1024 * 1024
HEX_SHA256 = re.compile(r"^[0-9a-f]{64}$")
GIT_COMMIT = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")


class PatchError(RuntimeError):
    """A fail-closed manifest, preimage, or patch application error."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise PatchError(message)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def safe_relative(value: object, label: str) -> PurePosixPath:
    require(isinstance(value, str) and bool(value), f"{label} must be a nonempty relative path")
    require("\\" not in value and "\x00" not in value, f"{label} has an invalid path separator")
    path = PurePosixPath(value)
    require(not path.is_absolute() and str(path) == value and
            all(part not in ("", ".", "..") for part in path.parts),
            f"{label} must be canonical and cannot traverse")
    return path


def resolve_inside(root: Path, relative: PurePosixPath, label: str,
                   *, must_exist: bool = True) -> Path:
    candidate = root.joinpath(*relative.parts)
    current = root
    for part in relative.parts:
        current = current / part
        if current.exists() or current.is_symlink():
            require(not current.is_symlink(), f"{label} traverses a symlink: {relative}")
    if must_exist:
        resolved = candidate.resolve(strict=True)
    else:
        resolved = candidate.resolve(strict=False)
    require(resolved.is_relative_to(root), f"{label} escapes its root: {relative}")
    return resolved


def owned_directory(path: Path, label: str) -> Path:
    require(not path.is_symlink(), f"{label} cannot be a symlink")
    resolved = path.resolve(strict=True)
    metadata = resolved.stat()
    require(stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == os.getuid(),
            f"{label} must be a user-owned directory")
    require(metadata.st_mode & 0o022 == 0, f"{label} must not be group/world writable")
    return resolved


def read_bounded(path: Path, limit: int, label: str) -> bytes:
    require(not path.is_symlink(), f"{label} cannot be a symlink")
    metadata = path.stat()
    require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.getuid(),
            f"{label} must be a user-owned regular file")
    require(metadata.st_size <= limit, f"{label} exceeds {limit} bytes")
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    require(len(data) <= limit, f"{label} exceeds {limit} bytes")
    return data


def parse_manifest(manifest_path: Path) -> tuple[dict, bytes, Path]:
    require(not manifest_path.is_symlink(), "manifest cannot be a symlink")
    manifest_path = manifest_path.resolve(strict=True)
    data = read_bounded(manifest_path, MAX_MANIFEST_BYTES, "manifest")
    try:
        manifest = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise PatchError(f"invalid manifest JSON: {error}") from error
    require(isinstance(manifest, dict) and manifest.get("schema") == 1,
            "manifest schema must be 1")
    commit = manifest.get("upstream_commit")
    require(isinstance(commit, str) and GIT_COMMIT.fullmatch(commit),
            "manifest upstream_commit must be a full lowercase Git hash")
    profile = manifest.get("profile")
    require(isinstance(profile, str) and 0 < len(profile) <= 128,
            "manifest profile must be a short nonempty string")
    patches = manifest.get("patches")
    files = manifest.get("files")
    require(isinstance(patches, list) and patches, "manifest patches must be a nonempty list")
    require(isinstance(files, list) and 0 < len(files) <= MAX_FILES,
            f"manifest files must contain 1..{MAX_FILES} entries")
    return manifest, data, manifest_path


def archive_sha256(path: Path) -> str:
    require(not path.is_symlink(), "upstream archive cannot be a symlink")
    metadata = path.stat()
    require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.getuid(),
            "upstream archive must be a user-owned regular file")
    require(metadata.st_size <= MAX_ARCHIVE_BYTES,
            f"upstream archive exceeds {MAX_ARCHIVE_BYTES} bytes")
    digest = hashlib.sha256()
    total = 0
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            total += len(chunk)
            require(total <= MAX_ARCHIVE_BYTES, "upstream archive grew beyond size limit")
            digest.update(chunk)
    return digest.hexdigest()


def verify_archive_preimages(archive: Path, commit: str, entries: list[dict],
                             expected_root: str | None = None) -> dict[str, bytes]:
    expected_root = expected_root or f"zed-{commit}"
    require(expected_root in (f"zed-{commit}", f"xim-rs-{commit}"),
            "unsupported upstream archive root")
    expected = {entry["relative"]: entry for entry in entries}
    found = {}
    members_seen = 0
    expanded = 0
    root_seen = False
    try:
        with tarfile.open(archive, mode="r:gz") as bundle:
            for member in bundle:
                members_seen += 1
                require(members_seen <= MAX_ARCHIVE_MEMBERS,
                        "upstream archive has too many members")
                name = member.name
                require("\\" not in name and "\x00" not in name,
                        "upstream archive contains an invalid member path")
                path = PurePosixPath(name)
                require(not path.is_absolute() and
                        all(part not in ("", ".", "..") for part in path.parts),
                        "upstream archive contains an unsafe member path")
                require(path.parts and path.parts[0] == expected_root,
                        f"upstream archive root must be {expected_root}")
                if name == expected_root:
                    require(member.isdir() and not root_seen,
                            "upstream archive root is missing, duplicated, or not a directory")
                    root_seen = True
                    continue
                expanded += member.size
                require(expanded <= MAX_ARCHIVE_EXPANDED_BYTES,
                        "upstream archive expanded size exceeds limit")
                relative = PurePosixPath(*path.parts[1:]).as_posix()
                if relative not in expected:
                    continue
                require(relative not in found, f"duplicate upstream archive target: {relative}")
                require(member.isfile() and member.size <= MAX_SOURCE_FILE_BYTES,
                        f"archive target is not a bounded regular file: {relative}")
                source = bundle.extractfile(member)
                require(source is not None, f"cannot read archive target: {relative}")
                with source:
                    data = source.read(MAX_SOURCE_FILE_BYTES + 1)
                require(len(data) == member.size and len(data) <= MAX_SOURCE_FILE_BYTES,
                        f"archive target size is invalid: {relative}")
                found[relative] = data
    except (OSError, tarfile.TarError, EOFError) as error:
        raise PatchError(f"cannot inspect upstream tar archive: {type(error).__name__}") from error
    require(root_seen, f"upstream archive root {expected_root} is absent")
    require(found.keys() == expected.keys(), "upstream archive is missing one or more manifest targets")
    for relative, entry in expected.items():
        require(sha256_bytes(found[relative]) == entry["before_sha256"],
                f"manifest before_sha256 does not match upstream archive: {relative}")

    return found


def parse_patch_paths(data: bytes, label: str) -> set[str]:
    try:
        lines = data.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise PatchError(f"{label} is not UTF-8 text") from error
    paths: set[str] = set()
    old_path = None
    saw_hunk = False

    def strip_prefix(value: str, context: str) -> str:
        require("/" in value, f"{label} {context} is not a prefixed file path")
        prefix, relative = value.split("/", 1)
        require(prefix in ("a", "b", "i", "w"),
                f"{label} {context} has an unsupported path prefix")
        safe_relative(relative, f"{label} {context}")
        return relative

    for line in lines:
        if line.startswith(("new file mode ", "deleted file mode ", "rename from ",
                            "rename to ", "copy from ", "copy to ")):
            raise PatchError(f"{label} contains a create/delete/rename/copy operation")
        if line.startswith("diff --git "):
            match = re.fullmatch(r"diff --git ([^ ]+) ([^ ]+)", line)
            require(match is not None, f"{label} has an unsupported git diff header")
            old_name, new_name = match.groups()
            require(strip_prefix(old_name, "git old path") ==
                    strip_prefix(new_name, "git new path"), f"{label} attempts a rename")
        elif line.startswith("--- "):
            require(old_path is None, f"{label} has an unmatched old-file header")
            old_path = strip_prefix(line[4:].split("\t", 1)[0], "old-file path")
        elif line.startswith("+++ "):
            require(old_path is not None, f"{label} has a new-file header without an old-file header")
            new_path = line[4:].split("\t", 1)[0]
            new_path = strip_prefix(new_path, "new-file path")
            require(old_path == new_path, f"{label} attempts a create/delete/rename")
            paths.add(old_path)
            old_path = None
        elif line.startswith("@@ "):
            saw_hunk = True
    require(old_path is None and saw_hunk and paths, f"{label} contains no complete unified diff")
    return paths


def run_patch(patch_bin: str, source: Path, patch_path: Path, *, dry_run: bool) -> None:
    command = [patch_bin, "--batch", "--forward", "--fuzz=0"]
    if dry_run:
        command.append("--dry-run")
    command.extend(["-p1", "--directory", str(source), "--input", str(patch_path)])
    try:
        result = subprocess.run(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True, timeout=30, check=False)
    except (OSError, subprocess.SubprocessError) as error:
        raise PatchError(f"patch {'preflight' if dry_run else 'apply'} could not run: {type(error).__name__}") from error
    require(result.returncode == 0,
            f"patch {'preflight' if dry_run else 'apply'} failed (exit {result.returncode}): "
            f"{result.stdout[-1200:]}")


def file_states(source: Path, entries: list[dict], previous: dict | None = None) -> str:
    before = after = prior = 0
    previous = previous or {}
    for entry in entries:
        path = resolve_inside(source, entry["path"], "source file")
        data = read_bounded(path, MAX_SOURCE_FILE_BYTES, "source file")
        digest = sha256_bytes(data)
        require(digest in (entry["before_sha256"], entry["after_sha256"], previous.get(entry["relative"])),
                f"unexpected source hash: {entry['relative']}")
        before += digest == entry["before_sha256"]
        after += digest == entry["after_sha256"]
        prior += digest == previous.get(entry["relative"])
    if prior == len(entries):
        return "previous"
    if before == len(entries):
        return "before"
    if after == len(entries):
        return "after"
    raise PatchError("source files are in a mixed before/after state; refusing partial application")


def apply(source_arg: Path, manifest_arg: Path, archive_arg: Path | None = None) -> dict:
    require(not source_arg.is_symlink(), "source directory cannot be a symlink")
    source = owned_directory(source_arg, "source directory")
    parent = owned_directory(source.parent, "source parent")
    manifest, _raw_manifest, manifest_path = parse_manifest(manifest_arg)
    patch_bin = shutil.which("patch")
    require(patch_bin is not None, "patch must be installed")
    git_bin = shutil.which("git") if archive_arg is None else None
    if archive_arg is None:
        require(git_bin is not None, "git must be installed in git source mode")

    lock_path = parent / ("." + source.name + ".gpui-patch.lock")
    flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    lock_fd = os.open(lock_path, flags, 0o600)
    try:
        lock_stat = os.fstat(lock_fd)
        require(stat.S_ISREG(lock_stat.st_mode) and lock_stat.st_uid == os.getuid(),
                "patch lock must be a user-owned regular file")
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        if archive_arg is None:
            top = subprocess.run([git_bin, "-C", str(source), "rev-parse", "--show-toplevel"],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 text=True, timeout=5, check=False)
            require(top.returncode == 0 and Path(top.stdout.strip()).resolve() == source,
                    "source must be the exact Git checkout root; refusing an ancestor repository")
            commit = subprocess.run([git_bin, "-C", str(source), "rev-parse", "--verify", "HEAD"],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, text=True, timeout=5, check=False)
            require(commit.returncode == 0 and commit.stdout.strip() == manifest["upstream_commit"],
                    "source HEAD does not match manifest upstream_commit")

        entries = []
        by_path = {}
        for row in manifest["files"]:
            require(isinstance(row, dict), "file entries must be objects")
            relative = safe_relative(row.get("path"), "source file path")
            before = row.get("before_sha256")
            after = row.get("after_sha256")
            require(isinstance(before, str) and HEX_SHA256.fullmatch(before) and
                    isinstance(after, str) and HEX_SHA256.fullmatch(after) and before != after,
                    f"invalid before/after hashes for {relative}")
            key = relative.as_posix()
            require(key not in by_path, f"duplicate source file path: {key}")
            resolve_inside(source, relative, "source file")
            entry = {"relative": key, "path": relative,
                     "before_sha256": before, "after_sha256": after}
            entries.append(entry)
            by_path[key] = entry

        preimages = None
        previous = manifest.get("previous_files_sha256", {})
        require(isinstance(previous, dict) and (not previous or previous.keys() == by_path.keys()),
                "previous profile must contain exactly the manifest source set")
        require(all(isinstance(value, str) and HEX_SHA256.fullmatch(value) for value in previous.values()),
                "invalid previous profile hash")
        if archive_arg is not None:
            archive_root = manifest.get("archive_root", f"zed-{manifest['upstream_commit']}")
            require(archive_root in (f"zed-{manifest['upstream_commit']}", f"xim-rs-{manifest['upstream_commit']}"),
                    "unsupported upstream archive root")
            require(source.name == archive_root,
                    "archive-mode source directory must match the pinned archive root")
            archive_hash = manifest.get("upstream_archive_sha256")
            require(isinstance(archive_hash, str) and HEX_SHA256.fullmatch(archive_hash),
                    "manifest upstream_archive_sha256 is required in archive mode")
            archive = archive_arg
            require(archive_sha256(archive) == archive_hash,
                    "upstream archive SHA-256 differs from manifest")
            preimages = verify_archive_preimages(archive, manifest["upstream_commit"], entries, archive_root)

        patch_records = []
        covered = set()
        patch_total = 0
        manifest_root = manifest_path.parent.resolve(strict=True)
        for row in manifest["patches"]:
            require(isinstance(row, dict), "patch entries must be objects")
            relative = safe_relative(row.get("path"), "patch path")
            expected = row.get("sha256")
            require(isinstance(expected, str) and HEX_SHA256.fullmatch(expected),
                    f"invalid patch hash for {relative}")
            patch_path = resolve_inside(manifest_root, relative, "patch file")
            data = read_bounded(patch_path, MAX_PATCH_BYTES, "patch file")
            patch_total += len(data)
            require(patch_total <= MAX_PATCH_BYTES, "combined patch profile exceeds size limit")
            require(sha256_bytes(data) == expected, f"patch hash mismatch: {relative}")
            targets = parse_patch_paths(data, str(relative))
            require(targets <= by_path.keys(), f"patch contains an unknown target: {relative}")
            covered.update(targets)
            patch_records.append((patch_path, data))
        require(covered == by_path.keys(), "manifest file set differs from patch targets")

        state = file_states(source, entries, previous)
        if state == "after":
            return {"status": "already-applied", "profile": manifest["profile"],
                    "upstream_commit": manifest["upstream_commit"], "files": len(entries),
                    "patches": len(patch_records), "source_kind": "archive" if archive_arg else "git"}

        with tempfile.NamedTemporaryFile(prefix="gpui-downstream-", suffix=".patch", delete=False) as combined:
            combined_path = Path(combined.name)
            for _patch_path, data in patch_records:
                combined.write(data)
                if data and not data.endswith(b"\n"):
                    combined.write(b"\n")
        try:
            for patch_path, data in patch_records:
                targets = parse_patch_paths(data, patch_path.name)
                require(targets <= by_path.keys(), "patch target changed after validation")
            if state == "previous":
                require(preimages is not None, "previous profile migration requires verified upstream archive")
                # Validate the complete successor in isolation before replacing any
                # exact predecessor bytes. Unknown/local modifications stay intact.
                with tempfile.TemporaryDirectory(prefix="gpui-profile-upgrade-") as temp:
                    staged = Path(temp)
                    for entry in entries:
                        target = staged / entry["path"]
                        target.parent.mkdir(parents=True, exist_ok=True)
                        target.write_bytes(preimages[entry["relative"]])
                    run_patch(patch_bin, staged, combined_path, dry_run=True)
                    run_patch(patch_bin, staged, combined_path, dry_run=False)
                    require(file_states(staged, entries) == "after", "successor profile postimages do not match")
                    require(file_states(source, entries, previous) == "previous", "previous profile changed during validation")
                    for entry in entries:
                        target = resolve_inside(source, entry["path"], "source file")
                        with tempfile.NamedTemporaryFile(dir=target.parent, prefix=".profile-", delete=False) as stream:
                            replacement = Path(stream.name)
                            try:
                                stream.write((staged / entry["path"]).read_bytes())
                                stream.flush()
                                os.fchmod(stream.fileno(), target.stat().st_mode & 0o777)
                                os.fsync(stream.fileno())
                                os.replace(replacement, target)
                            finally:
                                replacement.unlink(missing_ok=True)
                require(file_states(source, entries) == "after", "migrated profile does not match postimages")
                return {"status": "migrated", "profile": manifest["profile"],
                        "upstream_commit": manifest["upstream_commit"], "files": len(entries),
                        "patches": len(patch_records), "source_kind": "archive"}
            run_patch(patch_bin, source, combined_path, dry_run=True)
            require(file_states(source, entries) == "before",
                    "source preimages changed after dry-run; refusing application")
            if archive_arg is None:
                commit_again = subprocess.run([git_bin, "-C", str(source), "rev-parse", "--verify", "HEAD"],
                                              stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                              stderr=subprocess.PIPE, text=True, timeout=5, check=False)
                require(commit_again.returncode == 0 and
                        commit_again.stdout.strip() == manifest["upstream_commit"],
                        "source HEAD changed after dry-run; refusing application")
            run_patch(patch_bin, source, combined_path, dry_run=False)
        finally:
            combined_path.unlink(missing_ok=True)

        require(file_states(source, entries) == "after",
                "patch command completed but postimage hashes do not match; source retained for diagnosis")
        return {"status": "applied", "profile": manifest["profile"],
                "upstream_commit": manifest["upstream_commit"], "files": len(entries),
                "patches": len(patch_records), "source_kind": "archive" if archive_arg else "git"}
    finally:
        os.close(lock_fd)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_dir", type=Path)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--archive", type=Path,
                        help="verified upstream tar.gz archive for a non-Git extracted source tree")
    args = parser.parse_args(argv)
    try:
        print(json.dumps(apply(args.source_dir, args.manifest, args.archive), sort_keys=True))
        return 0
    except (OSError, PatchError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"apply-gpui-patches: {type(error).__name__}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
