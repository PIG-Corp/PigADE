#!/usr/bin/env python3
"""Measure four historical host frames across owned SIGKILL/reopen cycles.

This measures the real session host, not UI presentation. Use a completed corpus
from long-history.py. Source files are hashed outside the timed startup window.
"""
import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


STRESS = module("corpus_stress", "stress-pty.py")
CORPUS = module("corpus_runner", "long-history.py")


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def frame_digest(view):
    return hashlib.sha256(json.dumps(view["frame"], sort_keys=True,
                                     ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()


def host_request(endpoint, expected_pid, command):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(3)
        connection.connect(str(endpoint))
        peer_pid, peer_uid, _ = struct.unpack("3i", connection.getsockopt(
            socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))
        check(peer_pid == expected_pid and peer_uid == os.getuid(),
              "IPC endpoint belongs to a different host; request refused")
        payload = json.dumps({"protocol": STRESS.PROTOCOL, "command": command},
                             separators=(",", ":")).encode()
        check(len(payload) <= STRESS.MAX_IPC, "request exceeds wire limit")
        connection.sendall(struct.pack("<I", len(payload)) + payload)

        def read_exact(size):
            output = bytearray()
            while len(output) < size:
                block = connection.recv(size - len(output))
                check(block, "host closed IPC during response")
                output.extend(block)
            return output

        size = struct.unpack("<I", read_exact(4))[0]
        check(0 < size <= STRESS.MAX_IPC, "host response exceeds wire limit")
        reply = json.loads(read_exact(size))
        check(reply["protocol"] == STRESS.PROTOCOL and reply["error"] is None,
              f"host response error: {reply.get('error')}")
        return reply


def historical_frames(endpoint, expected_pid):
    return host_request(endpoint, expected_pid, {"command": "list"})["views"]


def canonical_tail(endpoint, pid, view):
    count = view["history"]["records"]
    if count == 0:
        return {"snapshot_records": 0, "recovery_prefix_records": 0, "records": []}
    query = {"session": view["session"], "generation": view["generation"],
             "expected_sequence": view["sequence"], "ordinal": 0,
             "offset": 0, "max_bytes": 1}

    def chunk():
        reply = host_request(endpoint, pid, {"command": "history_read", "query": query})
        check(not reply["views"], "history read unexpectedly cloned terminal frames")
        record = reply["history_chunk"]
        check(record["session_id"] == view["session"] and
              record["generation"] == view["generation"] and
              record["history_sequence"] == view["sequence"] and
              record["snapshot_records"] == count and
              record["ordinal"] == query["ordinal"] and record["offset"] == query["offset"],
              "canonical history chunk identity/watermark mismatch")
        check(record["source_event_sequence"] <= view["sequence"], "history event is ahead of model")
        check(len(record["bytes"]) <= query["max_bytes"], "history chunk exceeds requested limit")
        return record

    first = chunk()
    prefix = first["recovery_prefix_records"]
    check(isinstance(prefix, int) and 0 <= prefix <= count, "recovery cold prefix unavailable")
    check(count - prefix <= 1024, "reference tail exceeds 1024-record diagnostic limit")
    records = []
    total_bytes = 0
    for ordinal in range(prefix, count):
        query.update(ordinal=ordinal, offset=0, max_bytes=65536)
        data = bytearray()
        while True:
            record = chunk()
            check(record["recovery_prefix_records"] == prefix, "recovery prefix changed")
            check(record["record_bytes"] + total_bytes <= 2 * 1024**2,
                  "reference canonical tail exceeds 2 MiB diagnostic limit")
            data.extend(record["bytes"])
            next_offset = record["next_offset"]
            if next_offset is None:
                check(len(data) == record["record_bytes"], "canonical record ended at wrong length")
                break
            check(next_offset == len(data) and record["bytes"], "noncontiguous history chunk")
            query["offset"] = next_offset
        total_bytes += len(data)
        records.append({"ordinal": ordinal, "source_event_sequence": record["source_event_sequence"],
                        "record_bytes": len(data), "bytes_hex": data.hex()})
    return {"snapshot_records": count, "recovery_prefix_records": prefix,
            "profile": first["profile"], "records": records}


def source_inventory(directory):
    result = {}
    for path in sorted(directory.rglob("*")):
        # Lease files and derived private caches are runtime, never source.
        if any(part.startswith(".history-replay-") for part in path.relative_to(directory).parts):
            continue
        if path.is_file() and (path.suffix in (".journal", ".checkpoint", ".archive")
                               or path.name == "rows.index" or path.name.endswith(".timeline.jsonl")):
            check(not path.is_symlink(), "source inventory refuses symlinks")
            result[str(path.relative_to(directory))] = {
                "bytes": path.stat().st_size, "sha256": CORPUS.sha256(path)}
    return result


def percentiles(values):
    ordered = sorted(values)
    return {name: ordered[max(0, math.ceil(len(ordered) * p) - 1)]
            for name, p in (("p50", .5), ("p95", .95), ("p99", .99))}


def run(args):
    directory = args.corpus.resolve(strict=True)
    manifest = json.loads((directory / "manifest.json").read_text())
    expected = {row["generation"]: row for row in manifest["results"]}
    check(len(expected) == 4 and manifest["sessions"] == 4, "exactly four corpus sessions required")
    for row in expected.values():
        proof = row.get("verification") or {}
        check(all(proof.get(key) is True for key in
                  ("complete_state_equal", "frame_equal", "cold_history_equal",
                   "live_state_equal", "live_frame_equal", "live_cold_history_equal")),
              "completed full/fast/live corpus verification required")
    args.output = args.output.resolve()
    check(not args.output.exists(), "result directory must be new")
    args.output.mkdir(mode=0o700, parents=True)
    frozen = args.output / "host.frozen"
    binary_hash = CORPUS.freeze_executable(args.binary.resolve(strict=True), frozen)
    result = {"schema": 1, "status": "running", "corpus": str(directory),
              "binary_sha256": binary_hash, "iterations_requested": args.iterations,
              "created_unix": time.time(), "samples": [], "errors": [],
              "scope": "Host-only first four complete historical frames; SIGKILL/reopen of owned host, no PTY spawn or UI.",
              "cache_scope": "First sample has uncontrolled OS page cache; subsequent samples are warm. No cold-cache claim.",
              "percentile_method": "nearest rank; all samples retained",
              "corpus_output_bytes_per_session": manifest["requested_output_bytes_per_session"],
              "corpus_resizes_per_session": manifest["resize_events_per_session"],
              "core_profile": manifest["core_profile"], "REC08_acceptance": False}
    output = args.output / "result.json"
    CORPUS.save(output, result)
    before = source_inventory(directory)
    check(sum(name.endswith(".journal") for name in before) == 4,
          "corpus root must contain exactly four source journals")
    result["source_inventory"] = before
    # Unix socket names have a small kernel limit independent of durable path
    # length. A private, exclusively locked short runtime holds only endpoints.
    runtime = tempfile.TemporaryDirectory(prefix="p-rc-")
    endpoint = Path(runtime.name) / "host.sock"
    result["private_runtime_directory"] = runtime.name
    baseline = None
    host = None
    identity = None
    with open(args.output / "host.log", "ab", buffering=0) as log:
        try:
            for iteration in range(args.iterations):
                started = time.monotonic()
                host = subprocess.Popen([str(frozen), "serve", str(directory),
                                         "--runtime", runtime.name],
                                        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                        stderr=log, start_new_session=True)
                identity = STRESS.identity(host.pid)
                deadline = started + args.deadline_seconds
                while True:
                    check(host.poll() is None, "owned host exited during startup; another owner may hold the corpus")
                    try:
                        views = historical_frames(endpoint, host.pid)
                        break
                    except (OSError, RuntimeError):
                        check(time.monotonic() < deadline, "four-frame startup deadline exceeded")
                        time.sleep(.01)
                elapsed_ms = (time.monotonic() - started) * 1000
                check(STRESS.identity(host.pid) == identity, "owned host identity changed")
                # The IPC server binds only after Host::open completes. The
                # response's process identity must also be our exact frozen exe.
                actual_exe = Path(f"/proc/{host.pid}/exe").stat()
                frozen_exe = frozen.stat()
                check((actual_exe.st_dev, actual_exe.st_ino) == (frozen_exe.st_dev, frozen_exe.st_ino),
                      "owned host executable changed")
                check(len(views) == 4, "startup did not expose exactly four historical frames")
                rows = {}
                for view in views:
                    generation = view["generation"]
                    check(generation in expected and generation not in rows, "unexpected/duplicate generation")
                    source = expected[generation]
                    check(view["sequence"] == source["final_sequence"], "model watermark changed")
                    check(view["session"] == source["session_id"], "session identity changed")
                    check(view["pid"] is None and view["state"] == "history", "historical frame reported live")
                    check(view["storage_error"] is None, "recovery exposed storage warnings")
                    check(view["recovery_path"] == "checkpoint-tail", "full raw replay used at startup")
                    check(view["history"]["records"] == source["cold_history_records"], "cold record count changed")
                    check(view["recovery_journal_bytes_read"] < source["journal_bytes"], "entire journal read")
                    rows[generation] = {"session": view["session"], "sequence": view["sequence"],
                                        "frame_sha256": frame_digest(view), "history": view["history"],
                                        "journal_bytes_read": view["recovery_journal_bytes_read"],
                                        "canonical_tail": canonical_tail(endpoint, host.pid, view)}
                if baseline is None:
                    baseline = rows
                check(rows == baseline, "complete frames/history changed across host kill/reopen")
                rss = int(Path(f"/proc/{host.pid}/statm").read_text().split()[1]) * os.sysconf("SC_PAGE_SIZE")
                result["samples"].append({"iteration": iteration, "four_frames_ms": elapsed_ms,
                                          "post_start_host_RSS_bytes": rss, "host_identity": identity,
                                          "generations": rows})
                # This is intentionally a crash, not a graceful shutdown.
                check(STRESS.identity(host.pid) == identity, "refusing SIGKILL after identity change")
                os.kill(host.pid, signal.SIGKILL)
                check(host.wait(timeout=10) == -signal.SIGKILL, "owned host did not die from SIGKILL")
                host = None
                CORPUS.save(output, result)
                print(f"host recovery {iteration + 1}/{args.iterations}: {elapsed_ms:.2f} ms", flush=True)
            after = source_inventory(directory)
            check(before == after, "source journal/checkpoint/archive bytes changed during host cycles")
            result["source_inventory_unchanged"] = True
            result["four_frames_ms"] = percentiles([sample["four_frames_ms"] for sample in result["samples"]])
            result["host_only_p95_within_2s"] = result["four_frames_ms"]["p95"] <= 2000
            result["status"] = "completed"
        except Exception as error:
            result["status"] = "failed"
            result["errors"].append(f"{type(error).__name__}: {error}")
        finally:
            if host is not None and host.poll() is None:
                try:
                    if STRESS.identity(host.pid) == identity:
                        host.kill()
                        host.wait(timeout=10)
                except OSError:
                    # Exiting between poll and identity lookup is an ordinary
                    # crash race; never widen cleanup to a reused PID.
                    pass
            result["completed_unix"] = time.time()
            CORPUS.save(output, result)
            runtime.cleanup()
    print(json.dumps({key: result.get(key) for key in ("status", "four_frames_ms", "errors")}))
    return 0 if result["status"] == "completed" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, type=Path, help="completed shared corpus directory containing manifest.json")
    parser.add_argument("--output", required=True, type=Path, help="new private retained result directory")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/pigade-session-host")
    parser.add_argument("--iterations", type=int, default=30)
    parser.add_argument("--deadline-seconds", type=float, default=30)
    args = parser.parse_args()
    if not 1 <= args.iterations <= 1000 or not 1 <= args.deadline_seconds <= 300:
        parser.error("iterations must be 1..1000; deadline must be 1..300 seconds")
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
