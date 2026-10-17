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
MAX_RESIZE_ADMISSION_DIAGNOSTIC_RECORDS = 1024
RESIZE_ADMISSION_DIAGNOSTIC_INTERVAL = 1.0
RESIZE_DIAGNOSTIC_SNAPSHOT_TIMEOUT = 0.020
ACK_TIMEOUT_SECONDS = 5
MAX_ACK_SIDECAR_RECORDS = 16_384
ACK_SIDECAR_DRAIN_LIMIT = 512
ACK_SIDECAR_PACKET = struct.Struct("!BBQQQ")
ACK_SIDECAR_EVENTS = {1: "ping_received", 2: "ack_write_completed", 3: "release_observed"}
ACK_SIDECAR_SOCKET_PREFIX = "pigade-soak-ack-"

# The producer splits UTF-8, CSI/OSC, and synchronized-output sequences across
# separate PTY writes. Every byte, including ACKs, is covered by its CRC.
PRODUCER = r'''
import errno,os,select,socket,struct,sys,termios,time,tty,zlib
from collections import deque
stream=int(sys.argv[1]); duration=int(sys.argv[2]); rate=int(sys.argv[3])
sidecar=None; sidecar_address=None; sidecar_sequence=0
if len(sys.argv)>4:
 sidecar=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM); sidecar.setblocking(False)
 sidecar_address='\0'+sys.argv[4][1:] if sys.argv[4].startswith('@') else sys.argv[4]
tty.setraw(0,termios.TCSANOW)
crc=count=0; pending=b''; stop=False; commands=deque()
def milestone(kind,token):
 global sidecar_sequence
 if sidecar is None: return
 try:
  parts=token.split(b'-')
  if len(parts)!=3 or parts[0]!=b'SOAK' or int(parts[1])!=stream: return
  packet=struct.pack('!BBQQQ',kind,stream,int(parts[2]),time.monotonic_ns(),sidecar_sequence)
 except (ValueError,OverflowError):
  return
 sidecar_sequence+=1
 try:
  sidecar.sendto(packet,sidecar_address)
 except OSError:
  pass
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
   token=line[5:]; milestone(1,token); emit(b'\r\nACK:'+token+b'\r\n'); milestone(2,token)
   while True:
    release=b'RELEASE:'+token
    if release in commands:
     commands.remove(release); milestone(3,token)
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


class AckSidechannel:
    """Bounded producer milestones; datagrams contain IDs and clocks only."""

    def __init__(self, path, max_records=MAX_ACK_SIDECAR_RECORDS):
        self.path = path
        self.max_records = max_records
        self.address = "@" + ACK_SIDECAR_SOCKET_PREFIX + uuid.uuid4().hex
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        self.socket.bind("\0" + self.address[1:])
        self.socket.setblocking(False)
        self.file = open(path, "a", buffering=65536)
        self.records = 0
        self.omitted = 0
        self.counts = {}
        self.last_by_stream = {}
        self.last_token_events = {}
        self.sequence_gaps = 0

    def drain(self, limit=ACK_SIDECAR_DRAIN_LIMIT):
        for _ in range(limit):
            try:
                packet = self.socket.recv(ACK_SIDECAR_PACKET.size + 1)
            except BlockingIOError:
                break
            if len(packet) != ACK_SIDECAR_PACKET.size:
                self.omitted += 1
                continue
            kind, stream, token_index, timestamp, sequence = ACK_SIDECAR_PACKET.unpack(packet)
            name = ACK_SIDECAR_EVENTS.get(kind)
            if name is None or stream > 3:
                self.omitted += 1
                continue
            key = str(stream)
            previous = self.last_by_stream.get(key)
            if previous is not None and sequence != previous["event_sequence"] + 1:
                self.sequence_gaps += max(1, sequence - previous["event_sequence"] - 1)
            event = {"event": name, "stream": stream, "token_index": token_index,
                     "producer_monotonic_ns": timestamp, "event_sequence": sequence}
            self.counts[f"{key}:{name}"] = self.counts.get(f"{key}:{name}", 0) + 1
            self.last_by_stream[key] = event
            previous_token = self.last_token_events.get(key)
            if previous_token is None or previous_token["token_index"] != token_index:
                self.last_token_events[key] = {"token_index": token_index, "events": {}}
            self.last_token_events[key]["events"][name] = event
            if self.records < self.max_records:
                write_diagnostic_line(self.file, event)
                self.records += 1
            else:
                self.omitted += 1

    def current_token(self, stream, token_index):
        self.drain()
        latest = self.last_token_events.get(str(stream))
        if latest is None or latest["token_index"] != token_index:
            return {}
        return latest["events"]

    def summary(self):
        self.drain()
        return {"records_written": self.records, "records_omitted": self.omitted,
                "sequence_gaps": self.sequence_gaps, "counts": self.counts,
                "last_by_stream": self.last_by_stream}

    def close(self):
        self.drain()
        self.file.flush()
        os.fsync(self.file.fileno())
        self.file.close()
        self.socket.close()


def process_wait_diagnostics(host_identity, child_identities, max_threads=32):
    """Read bounded /proc wait-channel metadata only after identity revalidation."""
    result = {"host": None, "children": [], "diagnostic_errors": []}
    def read_proc(identity, include_threads=False):
        boot, pid, start_ticks = identity
        if Path("/proc/sys/kernel/random/boot_id").read_text().strip() != boot:
            raise RuntimeError("boot_id_mismatch")
        before = STRESS.identity(pid)
        if before != identity:
            raise RuntimeError("process_identity_mismatch")
        def one(tid):
            proc = Path(f"/proc/{pid}/task/{tid}" if tid != pid else f"/proc/{pid}")
            stat_fields = (proc / "stat").read_text().rsplit(") ", 1)[1].split()
            return {"tid": tid, "state": stat_fields[0], "start_ticks": int(stat_fields[19]),
                    "comm": (proc / "comm").read_text().strip()[:64],
                    "wchan": (proc / "wchan").read_text().strip()[:128]}
        proc = Path(f"/proc/{pid}")
        base = one(pid)
        threads = []
        if include_threads:
            with os.scandir(proc / "task") as entries:
                selected_tids = []
                for entry in entries:
                    if len(selected_tids) >= max_threads:
                        break
                    if entry.name.isdecimal():
                        selected_tids.append(int(entry.name))
            for tid in selected_tids:
                try:
                    threads.append(one(tid))
                except (OSError, ValueError, IndexError):
                    continue
        after = STRESS.identity(pid)
        if after != identity:
            raise RuntimeError("process_identity_changed_during_read")
        if include_threads:
            for thread in threads:
                try:
                    current = one(thread["tid"])
                    if current["start_ticks"] != thread["start_ticks"]:
                        raise RuntimeError("thread_identity_changed_during_read")
                except (OSError, ValueError, IndexError) as error:
                    raise RuntimeError("thread_disappeared_during_read") from error
        return {"pid": pid, **base, "threads": threads,
                "thread_count_captured": len(threads), "thread_limit": max_threads}

    try:
        result["host"] = read_proc(host_identity, include_threads=True)
    except (OSError, RuntimeError, ValueError, IndexError) as error:
        result["diagnostic_errors"].append({"target": "host", "error": type(error).__name__})
    for stream, identity in enumerate(child_identities):
        if identity is None:
            continue
        try:
            result["children"].append({"stream": stream, **read_proc(identity)})
        except (OSError, RuntimeError, ValueError, IndexError) as error:
            result["diagnostic_errors"].append({"target": f"child:{stream}",
                                                "error": type(error).__name__})
    return result


def ack_timeout_record(*, elapsed, stream, token_index, views, view_durations_ns,
                       view_completed_monotonic_ns,
                       ping_started_ns, ping_completed_ns, ping_status, ping_views_count, sidechannel,
                       process_diagnostics):
    sessions = []
    captured_at_ns = time.monotonic_ns()
    for index, view in enumerate(views):
        sessions.append({"stream": index, "state": None if view is None else view.get("state"),
                         "sequence": None if view is None else view.get("sequence"),
                         "storage_error": None if view is None else view.get("storage_error"),
                         "target_token_visible": (None if view is None or index != stream else
                                                   f"ACK:SOAK-{stream}-{token_index}" in STRESS.text(view)),
                         "snapshot_duration_ns": view_durations_ns[index],
                         "snapshot_completed_monotonic_ns": view_completed_monotonic_ns[index],
                         "snapshot_age_at_diagnostic_ns": (None if view_completed_monotonic_ns[index] is None else
                                                            max(0, captured_at_ns - view_completed_monotonic_ns[index]))})
    return {"schema": 1, "elapsed_seconds": elapsed, "stream": stream,
            "token_index": token_index, "timeout_seconds": ACK_TIMEOUT_SECONDS,
            "ping": {"started_monotonic_ns": ping_started_ns,
                     "completed_monotonic_ns": ping_completed_ns,
                     "request_duration_ns": ping_completed_ns - ping_started_ns,
                     "status": ping_status, "returned_views": ping_views_count},
            "last_sessions": sessions,
            "producer_milestones": sidechannel.current_token(stream, token_index),
            "producer_sidechannel": sidechannel.summary(),
            "process_wait_diagnostics": process_diagnostics}


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def signal_owned_identity(identity, sig):
    """Signal only the process pinned by pidfd and revalidated against identity."""
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        return False
    _boot, pid, _started = identity
    pidfd = None
    try:
        pidfd = os.pidfd_open(pid, 0)
        if STRESS.identity(pid) != identity:
            return False
        signal.pidfd_send_signal(pidfd, sig)
        return True
    except (OSError, ValueError, IndexError, TypeError):
        return False
    finally:
        if pidfd is not None:
            os.close(pidfd)


def write_json_line(file, value):
    file.write(json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n")
    file.flush()
    os.fsync(file.fileno())


def write_diagnostic_line(file, value):
    """Append a bounded diagnostic row; final manifest fsyncs the diagnostic stream."""
    file.write(json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n")
    file.flush()


class ResizeAdmissionDiagnostics:
    """Rate-limited, bounded queue-full metadata; never writes frame contents."""

    METRIC_FIELDS = ("events", "output_bytes", "journal_fsync_ns", "model_apply_ns",
                     "archive_commit_ns", "checkpoint_ns", "frame_publish_ns", "checkpoints")

    def __init__(self, file, started_at, host_pid, ticks, page_size):
        self.file = file
        self.started_at = started_at
        self.host_pid = host_pid
        self.ticks = ticks
        self.page_size = page_size
        self.last_by_generation = {}
        self.records_written = 0
        self.omitted_records = 0
        self.last_diagnostic = None

    def observe(self, endpoint, stream, command, retry_count, remaining_seconds):
        now = time.monotonic()
        generation = command.get("generation")
        previous = self.last_by_generation.get(generation)
        if previous is not None and now - previous < RESIZE_ADMISSION_DIAGNOSTIC_INTERVAL:
            return False
        if generation not in self.last_by_generation and len(self.last_by_generation) >= 4:
            self.omitted_records += 1
            return False
        self.last_by_generation[generation] = now

        record = {
            "event": "resize_queue_full",
            "elapsed_monotonic_seconds": max(0.0, now - self.started_at),
            "stream": stream,
            "session": command.get("session"),
            "generation": generation,
            "target_geometry": command.get("geometry"),
            "retry_count": retry_count,
        }
        try:
            resources = resource_sample(self.host_pid, self.ticks, self.page_size)
            record["host_rss_bytes"] = resources.get("host_rss_bytes")
            record["host_cpu_seconds"] = resources.get("host_cpu_seconds")
        except (OSError, ValueError, IndexError):
            record["host_rss_bytes"] = None
            record["host_cpu_seconds"] = None

        snapshot_command = {"command": "snapshot", "session": command.get("session"),
                            "generation": generation}
        timeout = min(RESIZE_DIAGNOSTIC_SNAPSHOT_TIMEOUT, max(0.0, remaining_seconds))
        if timeout <= 0:
            record["snapshot"] = {"available": False, "reason": "retry_deadline_elapsed"}
        else:
            try:
                views = STRESS.request(endpoint, snapshot_command, timeout=timeout)
                if len(views) != 1:
                    record["snapshot"] = {"available": False, "reason": "missing_view"}
                else:
                    view = views[0]
                    metrics = view.get("metrics") or {}
                    record["snapshot"] = {
                        "available": True,
                        "generation": view.get("generation"),
                        "state": view.get("state"),
                        "sequence": view.get("sequence"),
                        "storage_error": str(view.get("storage_error"))[:512]
                        if view.get("storage_error") is not None else None,
                        "metrics": {key: metrics[key] for key in self.METRIC_FIELDS
                                    if isinstance(metrics.get(key), (int, float))},
                    }
            except Exception as error:  # Snapshot is diagnostic only; retry semantics stay unchanged.
                record["snapshot"] = {"available": False, "reason": type(error).__name__}

        self.last_diagnostic = record
        if self.records_written >= MAX_RESIZE_ADMISSION_DIAGNOSTIC_RECORDS:
            self.omitted_records += 1
            return False
        try:
            write_diagnostic_line(self.file, record)
        except OSError:
            self.omitted_records += 1
            return False
        self.records_written += 1
        return True

    def summary(self):
        return {"records_written": self.records_written,
                "omitted_records": self.omitted_records,
                "record_limit": MAX_RESIZE_ADMISSION_DIAGNOSTIC_RECORDS,
                "sample_interval_seconds": RESIZE_ADMISSION_DIAGNOSTIC_INTERVAL,
                "snapshot_timeout_seconds": RESIZE_DIAGNOSTIC_SNAPSHOT_TIMEOUT,
                "last_diagnostic": self.last_diagnostic}


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


def resize_with_admission_retry(endpoint, command, stats, deadline_seconds=10, on_queue_full=None):
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
            rejection_at = time.monotonic()
            remaining = deadline - time.monotonic()
            retry_count = stats["retries"] + 1
            if on_queue_full is not None:
                on_queue_full(command, retry_count, remaining)
            if remaining <= 0:
                raise RuntimeError("resize admission retry deadline exceeded") from error
            delay = min(.025, remaining)
            diagnostic_elapsed = max(0.0, time.monotonic() - rejection_at)
            time.sleep(max(0.0, delay - diagnostic_elapsed))
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
    resize_diagnostics_path = run_dir / "resize-admission-diagnostics.jsonl"
    ack_sidechannel_path = run_dir / "ack-producer-sidechannel.jsonl"
    ack_timeout_path = run_dir / "ack-timeout.json"
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
    with open(progress_path, "a", buffering=1) as progress, \
            open(samples_path, "a", buffering=1) as samples, \
            open(resize_diagnostics_path, "a", buffering=1) as resize_diagnostic_file:
        host = None
        owned = []
        specs = []
        child_identities = []
        ack_sidechannel = AckSidechannel(ack_sidechannel_path)
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
                        "args": ["-I", "-u", "-c", PRODUCER, str(index), str(args.seconds + 30),
                                 str(args.rate), ack_sidechannel.address],
                        "cwd": str(state), "geometry": {"rows": ROWS, "cols": COLS}}
                view = STRESS.request(endpoint, {"command": "launch", "spec": spec})[0]
                owned.append(STRESS.identity(view["pid"]))
                child_identities.append(owned[-1])
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
            resize_diagnostics = ResizeAdmissionDiagnostics(
                resize_diagnostic_file, actual_start, host.pid, ticks, page)
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
            last_views = [None] * len(specs)
            last_view_durations_ns = [None] * len(specs)
            last_view_completed_ns = [None] * len(specs)
            while time.monotonic() - actual_start < args.seconds and not interrupted.is_set():
                now = time.monotonic()
                check(host.poll() is None, "host exited during soak")
                if now >= next_ack:
                    for index, spec in enumerate(specs):
                        token = f"SOAK-{index}-{ack_counts[index]}"
                        ping_started_ns = time.monotonic_ns()
                        ping_status = "writer_queue_admitted"
                        ping_views_count = None
                        try:
                            ping_views = STRESS.request(endpoint, {"command": "input", "session": spec["session"],
                                                                   "generation": spec["generation"],
                                                                   "bytes": list(f"PING:{token}\n".encode())})
                            ping_views_count = len(ping_views)
                        except Exception as error:
                            ping_status = "input_request_error:" + type(error).__name__
                            raise
                        ping_completed_ns = time.monotonic_ns()
                        by = time.monotonic() + ACK_TIMEOUT_SECONDS
                        while True:
                            ack_sidechannel.drain()
                            snapshot_started_ns = time.monotonic_ns()
                            view = STRESS.snapshot(endpoint, spec)
                            last_view_completed_ns[index] = time.monotonic_ns()
                            last_view_durations_ns[index] = last_view_completed_ns[index] - snapshot_started_ns
                            last_views[index] = view
                            if f"ACK:{token}" in STRESS.text(view):
                                break
                            if time.monotonic() >= by:
                                record = ack_timeout_record(
                                    elapsed=time.monotonic() - actual_start, stream=index,
                                    token_index=ack_counts[index], views=last_views,
                                    view_durations_ns=last_view_durations_ns,
                                    view_completed_monotonic_ns=last_view_completed_ns,
                                    ping_started_ns=ping_started_ns,
                                    ping_completed_ns=ping_completed_ns, ping_status=ping_status,
                                    ping_views_count=ping_views_count,
                                    sidechannel=ack_sidechannel,
                                    process_diagnostics=process_wait_diagnostics(
                                        host_identity, child_identities))
                                save_json(ack_timeout_path, record)
                                result["ack_timeout_diagnostic"] = {
                                    "path": str(ack_timeout_path), "stream": index,
                                    "token_index": ack_counts[index],
                                    "timeout_seconds": ACK_TIMEOUT_SECONDS}
                                raise RuntimeError(f"input ACK timeout: {token}")
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
                                                    resize_admission_stats[index],
                                                    on_queue_full=lambda command, retry_count, remaining,
                                                    stream=index: resize_diagnostics.observe(
                                                        endpoint, stream, command, retry_count, remaining))
                        resize_counts[index] += 1
                    next_resize = now + args.resize_seconds
                if now >= next_sample:
                    elapsed = time.monotonic() - actual_start
                    host_sample = resource_sample(host.pid, ticks, page)
                    session_rows = []
                    for index, spec in enumerate(specs):
                        ack_sidechannel.drain()
                        snapshot_started_ns = time.monotonic_ns()
                        view = STRESS.snapshot(endpoint, spec)
                        last_view_completed_ns[index] = time.monotonic_ns()
                        last_view_durations_ns[index] = last_view_completed_ns[index] - snapshot_started_ns
                        last_views[index] = view
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
            # Pin and revalidate process identities before signaling; never fall
            # back to a racy PID-only kill if pidfd is unavailable.
            for identity in owned:
                signal_owned_identity(identity, signal.SIGTERM)
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
            if "resize_diagnostics" in locals():
                diagnostic_flush_error = None
                try:
                    resize_diagnostic_file.flush()
                    os.fsync(resize_diagnostic_file.fileno())
                except OSError as error:
                    diagnostic_flush_error = type(error).__name__
                diagnostic_summary = resize_diagnostics.summary()
                if diagnostic_flush_error is not None:
                    diagnostic_summary["flush_error"] = diagnostic_flush_error
                result["resize_admission_diagnostic_summary"] = diagnostic_summary
            if "ack_sidechannel" in locals():
                try:
                    result["ack_producer_sidechannel"] = ack_sidechannel.summary()
                    result["ack_producer_sidechannel_path"] = str(ack_sidechannel_path)
                    ack_sidechannel.close()
                except OSError as error:
                    result["ack_producer_sidechannel_flush_error"] = type(error).__name__
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
