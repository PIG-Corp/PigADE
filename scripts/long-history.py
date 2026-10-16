#!/usr/bin/env python3
"""Build and run the native long-history corpus example; retain every run."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import time
import uuid


def mount_for(path: Path) -> tuple[str, str]:
    resolved = path.resolve()
    best: tuple[int, str, str] | None = None
    for line in Path("/proc/self/mountinfo").read_text().splitlines():
        left, separator, right = line.partition(" - ")
        if not separator:
            continue
        left_fields = left.split()
        right_fields = right.split()
        if len(left_fields) < 5 or not right_fields:
            continue
        mountpoint = Path(left_fields[4].replace("\\040", " ").replace("\\011", "\t")).resolve()
        if resolved == mountpoint or mountpoint in resolved.parents:
            candidate = (len(str(mountpoint)), str(mountpoint), right_fields[0])
            if best is None or candidate[0] > best[0]:
                best = candidate
    if best is None:
        raise RuntimeError("could not identify corpus filesystem mount")
    return best[1], best[2]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def freeze_executable(source: Path, destination: Path) -> str:
    """Copy one opened binary inode into the retained run and hash that copy."""
    source_fd = os.open(source, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    destination_fd = None
    try:
        before = os.fstat(source_fd)
        destination_fd = os.open(
            destination,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL,
            stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR,
        )
        digest = hashlib.sha256()
        copied = 0
        while block := os.read(source_fd, 1024 * 1024):
            digest.update(block)
            copied += len(block)
            view = memoryview(block)
            while view:
                view = view[os.write(destination_fd, view):]
        os.fsync(destination_fd)
        after = os.fstat(source_fd)
        stable = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns)
        current = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns)
        if stable != current or copied != before.st_size:
            raise RuntimeError("binary changed while the private executable copy was made")
        frozen_hash = digest.hexdigest()
        if sha256(destination) != frozen_hash:
            raise RuntimeError("private executable copy hash mismatch")
        return frozen_hash
    finally:
        if destination_fd is not None:
            os.close(destination_fd)
        os.close(source_fd)


def save(path: Path, value: dict) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)
    descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", required=True, type=Path, help="durable filesystem parent for a private retained run")
    parser.add_argument("--binary", type=Path, help="use an already-built example binary and record its hash")
    parser.add_argument("--sessions", type=int, default=4)
    parser.add_argument("--output-bytes", type=int, default=1024**3)
    parser.add_argument("--resize-events", type=int, default=100_000)
    parser.add_argument("--logical-hours", type=float, default=24.0)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--verify", action="store_true", help="run the full-replay vs checkpoint-tail oracle after generation")
    args = parser.parse_args()

    if not args.directory.is_absolute():
        parser.error("--directory must be an absolute path")
    if args.sessions < 1 or args.sessions > 32 or args.output_bytes < 4096 or args.resize_events < 0:
        parser.error("sessions must be 1..32, output bytes at least 4096, and resize events nonnegative")
    args.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    parent = args.directory.resolve()
    mountpoint, filesystem = mount_for(parent)
    if filesystem in {"tmpfs", "ramfs"}:
        parser.error(f"{filesystem} cannot establish durable corpus evidence")
    estimate = int(args.sessions * args.output_bytes * 12 + 512 * 1024**2)
    free = shutil.disk_usage(parent).free
    if free < estimate:
        parser.error(f"insufficient free space: need estimated {estimate} bytes, have {free}")

    run = parent / f"long-history-run-{uuid.uuid4()}"
    run.mkdir(mode=0o700)
    builder_root = Path(__file__).resolve().parents[1]
    build_command = None
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", builder_root / "target")).resolve()
    if args.binary:
        binary = args.binary.resolve(strict=True)
    else:
        build_command = ["cargo", "build", "--release", "--locked", "-p", "pigade-session-host", "--example", "history_corpus"]
        build_env = os.environ.copy()
        build_env["CARGO_TARGET_DIR"] = str(target_dir)
        build = subprocess.run(build_command, cwd=builder_root, env=build_env, text=True, capture_output=True)
        (run / "build.stdout.log").write_text(build.stdout, encoding="utf-8")
        (run / "build.stderr.log").write_text(build.stderr, encoding="utf-8")
        if build.returncode:
            save(run / "runner.json", {
                "status": "build_failed", "exit_code": build.returncode,
                "build_command": build_command, "filesystem": filesystem,
                "mountpoint": mountpoint, "created_unix": time.time(),
                "note": "Failed artifacts and logs are retained; no corpus claim is made.",
            })
            print(run, file=sys.stderr)
            return build.returncode
        binary = target_dir / "release" / "examples" / "history_corpus"
    frozen_binary = run / "history_corpus.frozen"
    binary_hash = freeze_executable(binary, frozen_binary)
    command = [str(frozen_binary), "--directory", str(run), "--sessions", str(args.sessions),
               "--output-bytes", str(args.output_bytes), "--resize-events", str(args.resize_events),
               "--logical-hours", str(args.logical_hours), "--seed", str(args.seed)]
    if args.verify:
        command.append("--verify")
    runner = {
        "schema": 1, "status": "running", "created_unix": time.time(),
        "binary_source": str(binary), "frozen_binary": str(frozen_binary),
        "binary_sha256": binary_hash,
        "build_command": build_command,
        "command": command,
        "filesystem": filesystem, "mountpoint": mountpoint,
        "free_bytes_before_run": free, "estimated_bytes_required": estimate,
        "space_estimate_basis": "12x requested raw output plus 512 MiB; conservative preflight estimate, not a filesystem quota.",
        "verification_requested": args.verify,
        "limitations": [
            "Logical hours are metadata only; no 24-hour elapsed soak is performed.",
            "No VM reboot, UI/GPU, PTY, provider CLI, authentication, or resume claim is made.",
            "Full recovery verification is omitted unless --verify is supplied.",
            "Available space is checked; no filesystem quota is configured or claimed.",
            "The run directory is private and retained even if generation fails.",
        ],
    }
    save(run / "runner.json", runner)
    completed = subprocess.run(command, cwd=builder_root, text=True, capture_output=True)
    (run / "builder.stdout.log").write_text(completed.stdout, encoding="utf-8")
    (run / "builder.stderr.log").write_text(completed.stderr, encoding="utf-8")
    runner.update({
        "status": "completed" if completed.returncode == 0 else "builder_failed",
        "completed_unix": time.time(),
        "exit_code": completed.returncode,
        "builder_stdout": completed.stdout.strip(),
    })
    save(run / "runner.json", runner)
    print(run)
    if completed.returncode:
        print(completed.stderr, file=sys.stderr, end="")
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
