#!/usr/bin/env python3
"""Real-time, durable-filesystem soak of four release-host PTY sessions.

This is a synthetic headless host soak. It does not use a provider, network,
credentials, UI, or desktop input. State and append-only evidence are retained.
"""
import argparse
import datetime
import importlib.util
import json
import os
from pathlib import Path
import platform
import hashlib
import stat
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("stress_pty", ROOT / "scripts/stress-pty.py")
STRESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STRESS)
PROTOCOL = STRESS.PROTOCOL
DEFAULT_RATE = 4096
ROWS, COLS = 36, 120

# The producer splits UTF-8, CSI/OSC, and synchronized-output sequences across
# separate PTY writes. Every byte, including ACKs, is covered by its CRC.
PRODUCER = r'''
import errno,os,select,sys,termios,time,tty,zlib
from collections import deque
stream=int(sys.argv[1]); duration=int(sys.argv[2]); rate=int(sys.argv[3])
tty.setraw(0,termios.TCSANOW)
crc=count=0; pending=b''; stop=False; commands=deque()
def emit(data):
 global crc,count
 view=memoryview(data)
 while view:
  n=os.write(1,view); crc=zlib.crc32(view[:n],crc); count+=n; view=view[n:]
def receive_commands(timeout=0):
 global pending,stop
 ready,_,_=select.select([0],[],[],timeout)
 while ready:
  try: data=os.read(0,4096)
  except OSError as error:
   if error.errno==errno.EIO: stop=True; return
   raise
  if not data: stop=True; return
  pending+=data
  lines=pending.split(b'\n'); pending=lines.pop()
  commands.extend(lines)
  ready,_,_=select.select([0],[],[],0)
def read_commands():
 global stop
 receive_commands()
 while commands and not stop:
  line=commands.popleft()
  if line.startswith(b'PING:'):
   token=line[5:]; emit(b'\r\nACK:'+token+b'\r\n')
   while True:
    release=b'RELEASE:'+token
    if release in commands:
     commands.remove(release)
     break
    if b'FINISH' in commands:
     commands.remove(b'FINISH')
     stop=True
     break
    receive_commands(.05)
    if stop: break
  elif line==b'FINISH': stop=True
emit(('READY:%d\r\n'%stream).encode())
start=time.monotonic_ns(); deadline=start+duration*1000000000
patterns=[b'\x1b[?1049h\x1b[2J', 'Unicode: Жёлтый 🐖 漢字 e\u0301\r\n'.encode(),
          b'\x1b[?2026hSYNC-UPDATE\x1b[?2026l',
          b'\x1b]8;;https://example.invalid/soak\x1b\\link\x1b]8;;\x1b\\',
          b'\x1b[3;4Hprogress\x1b[K\x1b[?1049l']
prefix=('S%d:'%stream).encode(); base=(prefix+b'x'*min(3000,max(16,rate-64))+b'\r\n')
payload=(base*((rate//len(base))+1))[:rate]
events=[(payload[i:i+257]) for i in range(0,len(payload),257)]
def emit_split(data):
 cuts=(1,2,3,7,11)
 offset=0
 for width in cuts:
  if offset>=len(data): break
  emit(data[offset:offset+width]); offset+=width; time.sleep(.001)
 if offset<len(data): emit(data[offset:])
index=0; next_tick=start
while time.monotonic_ns()<deadline and not stop:
 read_commands()
 if stop: break
 now=time.monotonic_ns()
 if now<next_tick:
  time.sleep(min(.02,(next_tick-now)/1e9)); continue
 phase=(index//max(1,rate//257))%len(patterns)
 fragment=events[index%len(events)]
 if index%max(1,rate//257)==0: emit_split(patterns[phase])
 emit(fragment)
 index+=1
 next_tick=start+index*257*1000000000//rate
read_commands()
emit(b'\r\n\x1b[?2026l\x1b[?1049l')
finished=time.monotonic_ns()
emit(('\r\nPIGADE_ENDCRC:%d:%08x:%d:%d:%d:%d:%d\r\n'%
      (stream,crc,count,start,finished-start,0,count)).encode())
'''


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def write_json_line(file, value):
    file.write(json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n")
    file.flush()
    os.fsync(file.fileno())


def save_json(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, stat.S_IRUSR | stat.S_IWUSR)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(value, output, ensure_ascii=False, indent=2, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def resize_with_admission_retry(endpoint, command, stats, deadline_seconds=10):
    """Retry only an explicit queue-full resize rejection, within one deadline."""
    check(command.get("command") == "resize", "admission retry is restricted to resize")
    deadline = time.monotonic() + deadline_seconds
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError("resize admission retry deadline exceeded")
        stats["attempts"] += 1
        try:
            STRESS.request(endpoint, command, timeout=min(3, remaining))
            return
        except STRESS.HostRejection as error:
            if error.code != "queue_full":
                raise
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError("resize admission retry deadline exceeded") from error
            delay = min(.025, remaining)
            time.sleep(delay)
            stats["retries"] += 1
            stats["total_wait_seconds"] += delay
            stats["max_wait_seconds"] = max(stats["max_wait_seconds"], delay)


def freeze_executable(source, destination):
    source_fd = os.open(source, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    destination_fd = None
    try:
        before = os.fstat(source_fd)
        destination_fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                                 stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
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
        signature = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns, item.st_ctime_ns)
        check(signature(before) == signature(after) and copied == before.st_size,
              "release binary changed while the frozen copy was made")
        frozen_hash = digest.hexdigest()
        check(STRESS.digest(destination) == frozen_hash, "frozen binary hash mismatch")
        os.fchmod(destination_fd, stat.S_IRUSR | stat.S_IXUSR)
        os.fsync(destination_fd)
        return frozen_hash
    finally:
        if destination_fd is not None:
            os.close(destination_fd)
        os.close(source_fd)


def fs_type(path):
    return subprocess.check_output(["stat", "-f", "-c", "%T", str(path)], text=True).strip()


def checkpoint_state(state, generation):
    paths = list(state.glob(f"{generation}*.checkpoint"))
    if not paths:
        paths = list(state.glob(f"**/{generation}*.checkpoint"))
    if not paths:
        return {"count": 0, "latest_age_seconds": None, "latest_mtime_ns": None, "latest_sequence": None}
    newest = max(paths, key=lambda path: path.stat().st_mtime_ns)
    sequence = int(newest.name.rsplit(".", 2)[-2])
    return {"count": len(paths), "latest_age_seconds": max(0, time.time() - newest.stat().st_mtime),
            "latest_mtime_ns": newest.stat().st_mtime_ns, "latest_sequence": sequence}


def resource_sample(pid, ticks, page_size):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
    return {"host_rss_bytes": int(Path(f"/proc/{pid}/statm").read_text().split()[1]) * page_size,
            "host_cpu_seconds": (int(fields[11]) + int(fields[12])) / ticks}


def run(args):
    args.directory.mkdir(parents=True, exist_ok=True)
    args.directory = args.directory.resolve()
    filesystem = fs_type(args.directory)
    check(filesystem not in ("tmpfs", "ramfs"), f"durable filesystem required; found {filesystem}")
    check(args.binary.is_file() and os.access(args.binary, os.X_OK), f"release binary unavailable: {args.binary}")
    run_dir = args.output.resolve() if args.output else args.directory / f"soak-{datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')}-{uuid.uuid4().hex[:8]}"
    check(not run_dir.exists(), f"output directory already exists: {run_dir}")
    run_dir.mkdir(mode=0o700)
    state = run_dir / "state"
    state.mkdir(mode=0o700)
    frozen = state / "host-release"
    frozen_hash = freeze_executable(args.binary.resolve(strict=True), frozen)
    manifest_path = run_dir / "manifest.json"
    progress_path = run_dir / "progress.jsonl"
    samples_path = run_dir / "samples.jsonl"
    result = {"status": "running", "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "requested_seconds": args.seconds, "rate_bytes_per_second_per_session": args.rate,
              "sessions": [], "errors": [], "filesystem": filesystem, "run_directory": str(run_dir),
              "state_directory": str(state), "binary_path": str(args.binary.resolve()),
              "binary_sha256": frozen_hash,
              "binary_version": subprocess.check_output([str(frozen), "--version"], text=True).strip(),
              "source_sha256": {name: STRESS.digest(ROOT / name) for name in
                                ("crates/session-host/src/lib.rs", "scripts/soak-pty.py", "scripts/stress-pty.py")},
              "environment": {"os": platform.platform(), "kernel": platform.release(),
                              "logical_cpu_count": os.cpu_count(), "filesystem": filesystem},
              "scope": "Four synthetic shell/Python PTYs through a frozen release host; no UI, provider, credential or network activity.",
              "other_open_gates": ["4 sessions x 1 GiB plus 100000 resizes/session", "VM reboot", "UI/GPU and platform acceptance"]}
    save_json(manifest_path, result)
    with open(progress_path, "a", buffering=1) as progress, open(samples_path, "a", buffering=1) as samples:
        host = None
        owned = []
        specs = []
        interrupted = threading.Event()
        previous_handlers = {}
        def on_signal(signum, frame):
            interrupted.set()
        for sig in (signal.SIGTERM, signal.SIGINT):
            previous_handlers[sig] = signal.signal(sig, on_signal)
        log = open(state / "host.log", "ab", buffering=0)
        start = time.monotonic()
        ticks, page = os.sysconf("SC_CLK_TCK"), os.sysconf("SC_PAGE_SIZE")
        try:
            host = subprocess.Popen([str(frozen), "serve", str(state)], stdin=subprocess.DEVNULL,
                                    stdout=subprocess.DEVNULL, stderr=log, start_new_session=True)
            host_identity = STRESS.identity(host.pid)
            result["host_pid"], result["host_identity"] = host.pid, host_identity
            endpoint = state / "host.sock"
            ready_by = time.monotonic() + 30
            while True:
                check(host.poll() is None, "host exited during startup")
                try:
                    STRESS.request(endpoint, {"command": "list"})
                    break
                except (OSError, RuntimeError):
                    check(time.monotonic() < ready_by, "host startup deadline")
                    time.sleep(.05)
            for index in range(4):
                spec = {"session": str(uuid.uuid4()), "generation": str(uuid.uuid4()),
                        "operation": str(uuid.uuid4()), "executable": sys.executable,
                        "args": ["-I", "-u", "-c", PRODUCER, str(index), str(args.seconds + 30), str(args.rate)],
                        "cwd": str(state), "geometry": {"rows": ROWS, "cols": COLS}}
                view = STRESS.request(endpoint, {"command": "launch", "spec": spec})[0]
                owned.append(STRESS.identity(view["pid"]))
                specs.append(spec)
                result["sessions"].append({"stream": index, "session": spec["session"], "generation": spec["generation"],
                                           "pid": view["pid"], "journal": f"{spec['generation']}.journal"})
                ready_by = time.monotonic() + 20
                while f"READY:{index}" not in STRESS.text(STRESS.snapshot(endpoint, spec)):
                    check(time.monotonic() < ready_by, f"producer {index} READY timeout")
                    time.sleep(.05)
            # Persist owned identities/session IDs before the long measurement.
            # A running manifest must remain useful after harness interruption.
            save_json(manifest_path, result)
            actual_start = time.monotonic()
            result["measurement_started_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
            sample_every = args.sample_seconds
            next_sample, next_ack, next_resize = actual_start, actual_start + 5, actual_start + 3
            ack_counts = [0] * len(specs)
            resize_counts = [0] * len(specs)
            resize_admission_stats = [{"attempts": 0, "retries": 0,
                                       "total_wait_seconds": 0.0, "max_wait_seconds": 0.0}
                                      for _ in specs]
            checkpoint_summaries = [{"sample_count": 0, "max_checkpoint_age_seconds": None,
                                     "last_checkpoint_sequence": None, "max_sequence_lag": None}
                                    for _ in specs]
            ack_pause_totals = [0.0] * len(specs)
            ack_pause_max = [0.0] * len(specs)
            while time.monotonic() - actual_start < args.seconds and not interrupted.is_set():
                now = time.monotonic()
                check(host.poll() is None, "host exited during soak")
                if now >= next_ack:
                    for index, spec in enumerate(specs):
                        token = f"SOAK-{index}-{ack_counts[index]}"
                        STRESS.request(endpoint, {"command": "input", "session": spec["session"],
                                                  "generation": spec["generation"], "bytes": list(f"PING:{token}\n".encode())})
                        by = time.monotonic() + 5
                        while True:
                            view = STRESS.snapshot(endpoint, spec)
                            if f"ACK:{token}" in STRESS.text(view):
                                break
                            check(time.monotonic() < by, f"input ACK timeout: {token}")
                            time.sleep(.025)
                        ack_seen = time.monotonic()
                        STRESS.request(endpoint, {"command": "input", "session": spec["session"],
                                                  "generation": spec["generation"],
                                                  "bytes": list(f"RELEASE:{token}\n".encode())})
                        ack_release_seconds = time.monotonic() - ack_seen
                        ack_pause_totals[index] += ack_release_seconds
                        ack_pause_max[index] = max(ack_pause_max[index], ack_release_seconds)
                        write_json_line(progress, {"event": "ack_output_hold_release", "stream": index,
                                                   "token_index": ack_counts[index],
                                                   "observed_ack_to_release_dispatch_seconds": ack_release_seconds,
                                                   "note": "Producer pauses output until ACK is observed and RELEASE is dispatched; not an input latency sample."})
                        ack_counts[index] += 1
                    next_ack = now + args.ack_seconds
                if now >= next_resize:
                    for index, spec in enumerate(specs):
                        # Three bounded viewport sizes exercise live resize during output.
                        width = (112, 120, 128)[resize_counts[index] % 3]
                        height = (32, 36, 40)[resize_counts[index] % 3]
                        resize_with_admission_retry(endpoint, {"command": "resize", "session": spec["session"],
                                                              "generation": spec["generation"],
                                                              "geometry": {"rows": height, "cols": width}},
                                                    resize_admission_stats[index])
                        resize_counts[index] += 1
                    next_resize = now + args.resize_seconds
                if now >= next_sample:
                    elapsed = time.monotonic() - actual_start
                    host_sample = resource_sample(host.pid, ticks, page)
                    session_rows = []
                    for index, spec in enumerate(specs):
                        view = STRESS.snapshot(endpoint, spec)
                        check(view["storage_error"] is None, f"storage error in {spec['generation']}: {view['storage_error']}")
                        check(view["generation"] == spec["generation"], "session generation changed")
                        cp = checkpoint_state(state, spec["generation"])
                        summary = checkpoint_summaries[index]
                        summary["sample_count"] += 1
                        if cp["latest_age_seconds"] is not None:
                            summary["max_checkpoint_age_seconds"] = max(
                                summary["max_checkpoint_age_seconds"] or 0, cp["latest_age_seconds"])
                            summary["last_checkpoint_sequence"] = cp["latest_sequence"]
                            summary["max_sequence_lag"] = max(
                                summary["max_sequence_lag"] or 0,
                                max(0, view["sequence"] - cp["latest_sequence"]))
                        session_rows.append({"generation": spec["generation"], "sequence": view["sequence"],
                                             "checkpoint_sequence_lag": (None if cp["latest_sequence"] is None else
                                                                          max(0, view["sequence"] - cp["latest_sequence"])),
                                             "state": view["state"], "checkpoint": cp,
                                             "frame_history_lines": view["frame"].get("history_lines"),
                                             "input_ack_count": ack_counts[index], "resize_count": resize_counts[index]})
                    write_json_line(samples, {"elapsed_seconds": elapsed, "captured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                                              **host_sample, "sessions": session_rows})
                    write_json_line(progress, {"elapsed_seconds": elapsed, "target_seconds": args.seconds,
                                               "ack_counts": ack_counts, "resize_counts": resize_counts,
                                               "resize_admission_retry_counts": [item["retries"] for item in resize_admission_stats],
                                               "resize_admission_retry_wait_seconds": [item["total_wait_seconds"] for item in resize_admission_stats],
                                               "host_rss_bytes": host_sample["host_rss_bytes"]})
                    next_sample = now + sample_every
                time.sleep(.05)
            elapsed = time.monotonic() - actual_start
            result["actual_elapsed_seconds"] = elapsed
            result["real_time_elapsed"] = elapsed >= args.seconds
            result["elapsed_24h_completed"] = elapsed >= 86400
            result["rec08_full_acceptance"] = False
            result["interrupted"] = interrupted.is_set()
            for spec in specs:
                STRESS.request(endpoint, {"command": "input", "session": spec["session"],
                                          "generation": spec["generation"], "bytes": list(b"FINISH\n")})
            exit_by = time.monotonic() + 30
            while time.monotonic() < exit_by:
                if all(STRESS.snapshot(endpoint, spec)["state"] == "exited" for spec in specs):
                    break
                time.sleep(.1)
            else:
                raise RuntimeError("producer exit deadline")
            result["producers_exited_cleanly"] = True
            for record, spec in zip(result["sessions"], specs):
                proof = STRESS.verify_journal(state / record["journal"], spec)
                record.update(proof)
                record["input_ack_count"] = ack_counts[record["stream"]]
                record["resize_count"] = resize_counts[record["stream"]]
                check(proof["journal_framing_and_producer_crc_valid"], "journal/output CRC verification failed")
                if args.verify:
                    recovery = subprocess.run([str(frozen), "verify-recovery", str(state / record["journal"])],
                                              text=True, capture_output=True, timeout=3600, check=False)
                    check(recovery.returncode == 0,
                          f"verify-recovery failed for {record['generation']}: {recovery.stderr[-2000:]}")
                    recovery_proof = json.loads(recovery.stdout)
                    check(recovery_proof.get("recovery_path") == "checkpoint-tail" and
                          recovery_proof.get("complete_state_equal") is True and
                          recovery_proof.get("frame_equal") is True and
                          recovery_proof.get("cold_history_equal") is True,
                          f"incomplete full-model/cold-history recovery proof for {record['generation']}")
                    record["verify_recovery"] = recovery_proof
            result["verify_requested"] = args.verify
            result["verify_recovery_passed_all_sessions"] = args.verify
            result["journal_crc_verified_all_sessions"] = True
            result["input_acknowledged_all_sessions"] = all(ack_counts)
            result["ack_output_hold_summary"] = [{"total_observed_ack_to_release_dispatch_seconds": total,
                                                   "maximum_observed_ack_to_release_dispatch_seconds": maximum,
                                                   "note": "Each producer pauses output until token ACK is observed then harness dispatches RELEASE; this is not a latency/rate sample."}
                                                  for total, maximum in zip(ack_pause_totals, ack_pause_max)]
            result["resizes_per_session"] = resize_counts
            result["resize_admission_retry_summary"] = resize_admission_stats
            result["checkpoint_age_and_watermark_summary"] = checkpoint_summaries
            result["status"] = "completed" if result["real_time_elapsed"] and not interrupted.is_set() else "incomplete"
            result["real_time_soak_24h_integrity_pass"] = (
                result["status"] == "completed" and elapsed >= 86400 and
                result["journal_crc_verified_all_sessions"] and result["input_acknowledged_all_sessions"] and
                result["producers_exited_cleanly"])
            result["rec08_full_acceptance"] = False
        except Exception as error:
            result["status"] = "failed"
            result["errors"].append(f"{type(error).__name__}: {error}")
            result["actual_elapsed_seconds"] = time.monotonic() - start
            result["real_time_elapsed"] = result["actual_elapsed_seconds"] >= args.seconds
            result["elapsed_24h_completed"] = result["actual_elapsed_seconds"] >= 86400
            result["real_time_soak_24h_integrity_pass"] = False
            result["rec08_full_acceptance"] = False
        finally:
            # Signal only processes whose boot-id/PID/start-time still match this run.
            for identity in owned:
                try:
                    if STRESS.identity(identity[1]) == identity:
                        os.kill(identity[1], signal.SIGTERM)
                except (OSError, ValueError, FileNotFoundError):
                    pass
            if host is not None and host.poll() is None:
                try:
                    if STRESS.identity(host.pid) == host_identity:
                        host.terminate()
                        host.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    if STRESS.identity(host.pid) == host_identity:
                        host.kill()
                        host.wait(timeout=5)
                except (OSError, ValueError, FileNotFoundError):
                    pass
            log.close()
            result["finished_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
            result["state_retained"] = True
            result["host_log_tail"] = (state / "host.log").read_text(errors="replace")[-4000:]
            for sig, handler in previous_handlers.items():
                signal.signal(sig, handler)
            result.setdefault("verify_requested", args.verify)
            result.setdefault("verify_recovery_passed_all_sessions", False)
            if "resize_admission_stats" in locals():
                result["resize_admission_retry_summary"] = resize_admission_stats
            save_json(manifest_path, result)
    print(json.dumps({key: result.get(key) for key in
                      ("status", "actual_elapsed_seconds", "real_time_elapsed", "elapsed_24h_completed",
                       "journal_crc_verified_all_sessions", "real_time_soak_24h_integrity_pass",
                       "verify_recovery_passed_all_sessions", "run_directory", "errors")}), flush=True)
    return 1 if result["status"] in ("failed", "incomplete") else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True, help="durable filesystem parent; all state is retained")
    parser.add_argument("--output", type=Path, help="new result directory; defaults to a unique child of --directory")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/pigade-session-host")
    parser.add_argument("--seconds", type=int, default=86400, help="actual elapsed duration (default 24 hours)")
    parser.add_argument("--rate", type=int, default=DEFAULT_RATE, help="bounded output bytes/sec/session (default 4096)")
    parser.add_argument("--sample-seconds", type=int, default=60)
    parser.add_argument("--ack-seconds", type=int, default=5)
    parser.add_argument("--resize-seconds", type=int, default=3)
    parser.add_argument("--verify", action="store_true", help="after clean producer exit, compare full replay and checkpoint-tail model/cold history")
    args = parser.parse_args()
    if not 1 <= args.seconds <= 604800:
        parser.error("--seconds must be 1..604800 (maximum seven days)")
    if not 512 <= args.rate <= 65536:
        parser.error("--rate must be 512..65536 bytes/sec/session")
    if min(args.sample_seconds, args.ack_seconds, args.resize_seconds) < 1:
        parser.error("sampling intervals must be positive")
    if args.output is not None:
        args.directory = args.output.parent
    return run(args)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"PTY soak: {error}", file=sys.stderr)
        sys.exit(2)
