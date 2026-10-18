#!/usr/bin/env python3
"""Four real PTYs, durable journal integrity and host-only resource observations.

Reproduce: python3 scripts/stress-pty.py --directory ~/.local/state/pigade/benchmarks \
  --output benchmarks/results/native-pty-stress.json --preserve-state
No provider tasks/authentication. This is not an input-to-present/UI benchmark.
The generated run directory is mode 0700; failed runs retain state automatically.
"""
import argparse
import datetime
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid
import zlib

ROOT = Path(__file__).resolve().parents[1]
PROTOCOL = 2
RATE = 1024 * 1024
CHUNK = 16 * 1024
MAX_IPC = 8 * 1024 * 1024
PRODUCER = r'''
import fcntl,os,select,sys,termios,time,tty,zlib
stream,seconds,rate=sys.argv[1],int(sys.argv[2]),int(sys.argv[3])
normal=len(sys.argv)>4 and sys.argv[4]=='normal'
tty.setraw(0,termios.TCSANOW)
crc=count=0
pending=b''
def emit(data):
 global crc,count
 view=memoryview(data)
 while view:
  n=os.write(1,view)
  crc=zlib.crc32(view[:n],crc);count+=n;view=view[n:]
def commands():
 global pending
 while select.select([0],[],[],0)[0]:
  try:data=os.read(0,4096)
  except BlockingIOError:break
  if not data:return []
  pending+=data
 lines=pending.split(b'\n');pending=lines.pop()
 return lines
emit(('READY:'+stream+'\r\n').encode())
while True:
 if b'GO' in commands():break
 time.sleep(.001)
if normal:
 emit(('STARTED:'+stream+'\r\n').encode())
 while True:
  if b'STREAM' in commands():break
  time.sleep(.001)
start=time.monotonic_ns()
if normal:emit(b'\x1b[2J\x1b[1;1H')
else:emit(b'\x1b[?1049h\x1b[2J\x1b[2;36r\x1b[1;1H'+('STARTED:'+stream).encode()+b'\x1b[2;1H')
line=('S'+stream+':'+('0123456789'*11)+'\r\n').encode()
body=(line*((16384//len(line))+1))[:16384]
chunks=(rate*seconds)//16384
max_late_ns=0
for index in range(chunks):
 deadline=start+index*16384*1000000000//rate
 remaining=deadline-time.monotonic_ns()
 if remaining>0:time.sleep(remaining/1000000000)
 max_late_ns=max(max_late_ns,time.monotonic_ns()-deadline)
 for command in commands():
  if command.startswith(b'PING:'):
   emit(b'\x1b7\x1b[1;1H\x1b[2KACK:'+command[5:]+b'\x1b8')
 emit(body)
finished=time.monotonic_ns()
if not normal:emit(b'\x1b[?1049l')
trailer=('\r\nPIGADE_ENDCRC:%s:%08x:%d:%d:%d:%d:%d\r\n'%(stream,crc,count,start,finished-start,max_late_ns,chunks*16384)).encode()
view=memoryview(trailer)
while view:
 n=os.write(1,view);view=view[n:]
'''


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    hasher = hashlib.sha256()
    with open(path, 'rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            hasher.update(chunk)
    return hasher.hexdigest()


def identity(pid):
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(') ', 1)[1].split()
    return (Path('/proc/sys/kernel/random/boot_id').read_text().strip(), pid, int(fields[19]))


def identity_if_present(pid):
    try:
        return identity(pid)
    except (FileNotFoundError, ProcessLookupError):
        return None


def signal_owned_identity(expected, sig):
    """Signal only a process bound to a pidfd and the captured start identity."""
    if not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal'):
        return False
    _boot, pid, _started = expected
    pidfd = None
    try:
        pidfd = os.pidfd_open(pid, 0)
        if identity(pid) != expected:
            return False
        signal.pidfd_send_signal(pidfd, sig)
        return True
    except (OSError, ValueError, IndexError, TypeError):
        return False
    finally:
        if pidfd is not None:
            os.close(pidfd)


def process_state(pid):
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(') ', 1)[1].split()
    return fields[0]


def wait_owned_exit(expected, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            current = identity_if_present(expected[1])
        except (OSError, ValueError, IndexError, TypeError):
            return False
        if current is None or current != expected:
            return True
        try:
            if process_state(expected[1]) == 'Z':
                return True
        except (FileNotFoundError, ProcessLookupError):
            return True
        except (OSError, ValueError, IndexError, TypeError):
            return False
        time.sleep(.02)
    return False


def cleanup_owned_processes(host, host_identity, owned):
    """Stop the owned host and its producer PIDs without signaling process groups."""
    failures = []
    try:
        host_running = host is not None and host.poll() is None
    except (OSError, ValueError):
        host_running = True
        failures.append('host_poll_unavailable')
    if host_running:
        try:
            sent = host_identity is not None and signal_owned_identity(host_identity, signal.SIGTERM)
        except (OSError, ValueError, IndexError, TypeError):
            sent = False
        if not sent:
            failures.append('host_pidfd_sigterm_failed')
        try:
            host.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                killed = host_identity is not None and signal_owned_identity(host_identity, signal.SIGKILL)
            except (OSError, ValueError, IndexError, TypeError):
                killed = False
            if not killed:
                failures.append('host_pidfd_sigkill_failed')
            try:
                host.wait(timeout=3)
            except subprocess.TimeoutExpired:
                failures.append('host_exit_unconfirmed')
            except OSError:
                failures.append('host_wait_failed')
        except (OSError, ValueError):
            failures.append('host_wait_failed')
    for expected in owned:
        try:
            current = identity_if_present(expected[1])
        except (OSError, ValueError, IndexError, TypeError):
            failures.append('producer_identity_unavailable')
            continue
        if current is None:
            continue
        if current != expected:
            failures.append('producer_identity_changed_not_signaled')
            continue
        try:
            signaled = signal_owned_identity(expected, signal.SIGKILL)
        except (OSError, ValueError, IndexError, TypeError):
            signaled = False
        if not signaled:
            # The process may have exited between identity sampling and pidfd
            # opening; only a positive identity disappearance is accepted.
            try:
                still_owned = identity_if_present(expected[1]) == expected
            except (OSError, ValueError, IndexError, TypeError):
                failures.append('producer_identity_unavailable')
                continue
            if still_owned:
                failures.append('producer_pidfd_sigkill_failed')
                continue
        try:
            exited = wait_owned_exit(expected, 3)
        except (OSError, ValueError, IndexError, TypeError):
            exited = False
        if not exited:
            failures.append('producer_exit_unconfirmed')
    return failures


class RetainableTemporaryDirectory:
    """Keep only this run's directory when requested or evidence is needed."""

    def __init__(self, *, prefix, dir):
        self.prefix = prefix
        self.parent = dir
        self.path = None
        self.retain = False

    def __enter__(self):
        self.path = Path(tempfile.mkdtemp(prefix=self.prefix, dir=self.parent))
        self.path.chmod(0o700)
        return str(self.path)

    def __exit__(self, exc_type, exc, traceback):
        if not self.retain and exc_type is None and self.path is not None:
            shutil.rmtree(self.path)
        return False


class HostRejection(RuntimeError):
    """A command explicitly rejected before host admission."""

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def request(endpoint, command, timeout=3):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(timeout)
        connection.connect(str(endpoint))
        data = json.dumps({'protocol': PROTOCOL, 'command': command}, separators=(',', ':')).encode()
        check(len(data) <= MAX_IPC, 'request exceeds limit')
        connection.sendall(struct.pack('<I', len(data)) + data)

        def read(size):
            result = bytearray()
            while len(result) < size:
                chunk = connection.recv(size - len(result))
                check(bool(chunk), 'truncated IPC response')
                result.extend(chunk)
            return result
        length, = struct.unpack('<I', read(4))
        check(0 < length <= MAX_IPC, 'invalid response size')
        response = json.loads(read(length))
        check(response['protocol'] == PROTOCOL, 'protocol mismatch')
        error = response.get('error')
        if error is not None:
            rejection = response.get('rejected_before_admission')
            if rejection in ('queue_full',):
                raise HostRejection(rejection, f"host rejected before admission ({rejection}): {error}")
            raise RuntimeError(f"host: {error}")
        return response['views']


def snapshot(endpoint, spec):
    views = request(endpoint, {'command': 'snapshot', 'session': spec['session'], 'generation': spec['generation']})
    check(len(views) == 1, 'snapshot missing')
    view = views[0]
    check(view['storage_error'] is None, f"host degraded: {view['storage_error']}")
    check(view['generation'] == spec['generation'], 'generation changed')
    return view


def text(view):
    return ''.join(cell['text'] for cell in view['frame']['cells'])


def percentile(values, percentile_value):
    if not values:
        return None
    return sorted(values)[max(0, math.ceil(percentile_value * len(values)) - 1)]


def verify_journal(path, spec):
    """One event plus 1 KiB tail retained; never retain the 60 MiB stream."""
    crc = count = sequence = total_output = 0
    pending = b''
    kinds = {str(kind): 0 for kind in range(1, 5)}
    with open(path, 'rb') as source:
        prefix = source.read(12)
        check(len(prefix) == 12 and prefix[:8] == b'PIGJNL01', 'journal magic/header')
        size, = struct.unpack('<I', prefix[8:])
        check(size <= 65536, 'oversize header')
        raw = source.read(size)
        checksum = source.read(4)
        check(len(raw) == size and len(checksum) == 4, 'truncated header')
        check(zlib.crc32(prefix + raw) == struct.unpack('<I', checksum)[0], 'header checksum')
        header = json.loads(raw)
        check(header['session_id'] == spec['session'] and header['generation'] == spec['generation'], 'header identity')
        while True:
            raw = source.read(33)
            if not raw:
                break
            check(len(raw) == 33, 'truncated event header')
            seq, = struct.unpack('<Q', raw[:8])
            generation = str(uuid.UUID(bytes=raw[8:24]))
            kind = raw[24]
            size, checksum = struct.unpack('<II', raw[25:33])
            check(seq == sequence + 1, 'lost/duplicate/out-of-order sequence')
            check(generation == spec['generation'], 'mixed generation')
            check(kind in (1, 2, 3, 4) and size <= 1024 * 1024, 'invalid event kind/size')
            payload = source.read(size)
            check(len(payload) == size, 'truncated payload')
            check(zlib.crc32(raw[:29] + payload) == checksum, 'event checksum')
            sequence = seq
            kinds[str(kind)] += 1
            if kind == 1:
                total_output += len(payload)
                pending += payload
                if len(pending) > 1024:
                    body, pending = pending[:-1024], pending[-1024:]
                    crc = zlib.crc32(body, crc)
                    count += len(body)
            elif kind == 2:
                geometry = json.loads(payload)
                check(1 <= geometry['rows'] <= 512 and 2 <= geometry['cols'] <= 1024, 'invalid resize')
        marker = re.search(rb'\r\nPIGADE_ENDCRC:(\d+):([0-9a-f]{8}):(\d+):(\d+):(\d+):(\d+):(\d+)\r\n$', pending)
        check(marker is not None, f'producer ENDCRC missing (truncation/loss/backpressure timeout): {pending[-1000:]!r}')
        crc = zlib.crc32(pending[:marker.start()], crc)
        count += marker.start()
        stream, expected_crc, expected_count, start_ns, elapsed_ns, max_late_ns, body_bytes = marker.groups()
        check(crc == int(expected_crc, 16), 'producer output rolling CRC mismatch: lost/duplicated/modified bytes')
        check(count == int(expected_count), 'producer output byte count mismatch')
        check(kinds['4'] == 1, 'exactly one durable Exit expected')
    return {'sequence': sequence, 'event_counts': kinds, 'output_bytes': total_output,
            'counted_output_bytes_before_trailer': count, 'output_crc32': f'{crc:08x}',
            'journal_bytes': path.stat().st_size, 'journal_framing_and_producer_crc_valid': True,
            'stream': int(stream), 'producer_start_ns': int(start_ns),
            'producer_body_write_seconds': int(elapsed_ns) / 1e9,
            'producer_max_schedule_lateness_ms': int(max_late_ns) / 1e6,
            'producer_body_bytes': int(body_bytes), 'core_profile': header['profile']}


def run(args):
    args.directory.mkdir(parents=True, exist_ok=True)
    filesystem = subprocess.check_output(['stat', '-f', '-c', '%T', str(args.directory)], text=True).strip()
    check(filesystem not in ('tmpfs', 'ramfs'), 'durable filesystem required, not /tmp tmpfs')
    check(filesystem == 'btrfs', 'this experiment explicitly requires btrfs')
    result = {'captured_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'product_version': (ROOT / 'VERSION').read_text().strip(), 'protocol': PROTOCOL,
              'filesystem': filesystem, 'directory_parent': str(args.directory.resolve()),
              'target': {'streams': 4, 'seconds': args.seconds, 'body_bytes_per_second_per_stream': RATE},
              'status': 'running', 'sessions': [], 'resources': [], 'errors': [],
              'scope': 'Actual release PTY/host/fsync/IPC/checkpoint workload; no UI/GPU/provider processes. ACK is input-to-durable-snapshot-observed, NOT input-to-present.',
              'not_measured': ['UI present/frame time', 'GPU memory', 'queue high-water instrumentation', 'VM reboot', '24h soak', '1GiB cold-history gate'],
              'bounded_implementation_limits': {'PTY_read_chunk_bytes': 16384, 'output_event_batch_bytes': 65536, 'output_collection_window_microseconds': 250, 'actor_queue_items': 8,
                  'input_queue_items': 8, 'input_chunk_bytes': 16384, 'priority_control_items': 1,
                  'IPC_workers': 16, 'IPC_jobs': 16, 'IPC_message_limit_bytes': MAX_IPC,
                  'note': 'Source limits, not measured occupancy; one in-flight chunk per worker/reader plus retained bounded query/deferred buffers.'},
              'source_sha256': {name: digest(ROOT / name) for name in ['crates/session-host/src/lib.rs', 'crates/session-host/src/input.rs', 'crates/session-host/src/ipc_server.rs']}}
    result['screen_mode'] = 'normal_with_cold_history' if args.normal_history else 'alternate_TUI'
    result['source_sha256'].update({name: digest(ROOT / name) for name in ['crates/session-host/src/history.rs', 'crates/terminal/src/history.rs', 'crates/storage/src/archive.rs']})
    if args.normal_history:
        result['not_measured'].append('input ACK latency (normal-history workload)')
        result['scope'] = 'Actual normal-screen PTY/host/fsync/cold-history workload; no UI/GPU/provider processes; no input latency claim.'
    host = None
    host_identity = None
    owned = []
    stop = threading.Event()
    threads = []
    ticks = os.sysconf('SC_CLK_TCK')
    page = os.sysconf('SC_PAGE_SIZE')
    started_wall = time.monotonic()
    preserve_requested = bool(getattr(args, 'preserve_state', False))
    state_directory = RetainableTemporaryDirectory(prefix='p-', dir=args.directory)
    # This exact fresh directory is owned by this run. Failed or explicitly
    # preserved runs retain journals and recovery artifacts for inspection.
    with state_directory as temporary:
        state = Path(temporary)
        frozen = state / 'host-release'
        shutil.copyfile(args.binary, frozen)
        frozen.chmod(0o700)
        result['binary_sha256'] = digest(frozen)
        result['binary_version'] = subprocess.check_output([str(frozen), '--version'], text=True).strip()
        log = open(state / 'host.log', 'wb')
        try:
            host = subprocess.Popen([str(frozen), 'serve', str(state)], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log, start_new_session=True)
            result['host_pid'] = host.pid
            host_identity = identity(host.pid)
            result['host_identity'] = host_identity
            endpoint = state / 'host.sock'
            deadline = time.monotonic() + 10
            while True:
                check(host.poll() is None, 'host exited during startup')
                try:
                    request(endpoint, {'command': 'list'})
                    break
                except (OSError, RuntimeError):
                    check(time.monotonic() < deadline, 'host startup deadline')
                    time.sleep(.01)
            specs = []
            for index in range(4):
                spec = {'session': str(uuid.uuid4()), 'generation': str(uuid.uuid4()), 'operation': str(uuid.uuid4()),
                        'executable': '/usr/bin/python3', 'args': ['-u', '-c', PRODUCER, str(index), str(args.seconds), str(RATE), 'normal' if args.normal_history else 'alternate'],
                        'cwd': str(state), 'geometry': {'rows': 36, 'cols': 120}}
                view = request(endpoint, {'command': 'launch', 'spec': spec})[0]
                owned.append(identity(view['pid']))
                specs.append(spec)
                deadline = time.monotonic() + 10
                while f'READY:{index}' not in text(snapshot(endpoint, spec)):
                    check(time.monotonic() < deadline, 'producer did not become ready')
                    time.sleep(.01)
                result['sessions'].append({'stream': index, 'session': spec['session'], 'generation': spec['generation'], 'pid': view['pid'], 'ACK_observations_ms': [], 'ACK_errors': []})
            for spec in specs:
                request(endpoint, {'command': 'input', 'session': spec['session'], 'generation': spec['generation'], 'bytes': list(b'GO\n')})
            # GO and the first PING must not be consumed in the same startup read.
            # Wait for the producer to enter its steady-state command loop.
            for index, spec in enumerate(specs):
                deadline = time.monotonic() + 10
                while f'STARTED:{index}' not in text(snapshot(endpoint, spec)):
                    check(time.monotonic() < deadline, 'producer STARTED handshake deadline')
                    time.sleep(.005)
            if args.normal_history:
                for spec in specs:
                    request(endpoint, {'command': 'input', 'session': spec['session'], 'generation': spec['generation'], 'bytes': list(b'STREAM\n')})
            measurement_start = time.monotonic()
            first_cpu = None

            def observe(index):
                spec, record = specs[index], result['sessions'][index]
                counter = 0
                try:
                    deadline = time.monotonic() + args.seconds + 40
                    next_ping = time.monotonic()
                    while not stop.is_set():
                        now = time.monotonic()
                        check(now < deadline, 'producer drain/exit deadline exceeded')
                        view = snapshot(endpoint, spec)
                        if view['state'] == 'exited':
                            record['observed_exit_ns'] = time.monotonic_ns()
                            record['final_view'] = view
                            return
                        check(view['state'] == 'running', f"producer state: {view['state']}")
                        if not args.normal_history and now >= next_ping and now - measurement_start < args.seconds - 1:
                            token = f'{index}_{counter:04d}'
                            command = f'PING:{token}\n'.encode()
                            sent = time.monotonic_ns()
                            request(endpoint, {'command': 'input', 'session': spec['session'], 'generation': spec['generation'], 'bytes': list(command)})
                            ack_deadline = time.monotonic() + 5
                            while f'ACK:{token}' not in text(view):
                                check(time.monotonic() < ack_deadline, f'ACK timeout: {token}; state={view["state"]}; frame_tail={text(view)[-1000:]!r}')
                                time.sleep(.005)
                                view = snapshot(endpoint, spec)
                            record['ACK_observations_ms'].append((time.monotonic_ns() - sent) / 1e6)
                            counter += 1
                            next_ping = now + 2
                        # Poll more closely near nominal completion; this polling lag is
                        # included in observed-exit throughput and never called PTY lag.
                        stop.wait(.02 if time.monotonic() - measurement_start > args.seconds - 1 else .2)
                except Exception as error:
                    record['ACK_errors'].append(str(error))
                    stop.set()

            for index in range(4):
                thread = threading.Thread(target=observe, args=(index,), daemon=True)
                thread.start()
                threads.append(thread)
            next_progress = 10
            while any(thread.is_alive() for thread in threads):
                check(host.poll() is None, 'host died during workload')
                fields = Path(f'/proc/{host.pid}/stat').read_text().rsplit(') ', 1)[1].split()
                cpu = (int(fields[11]) + int(fields[12])) / ticks
                rss = int(Path(f'/proc/{host.pid}/statm').read_text().split()[1]) * page
                elapsed = time.monotonic() - measurement_start
                first_cpu = cpu if first_cpu is None else first_cpu
                result['resources'].append({'wall_seconds': elapsed, 'host_RSS_bytes': rss, 'host_CPU_seconds': cpu})
                if elapsed >= next_progress:
                    print(f'PTY stress {elapsed:.1f}s; host RSS {rss / 1024**2:.1f}MiB; no early success claim', file=sys.stderr, flush=True)
                    next_progress += 10
                time.sleep(.25)
            for thread in threads:
                thread.join(timeout=1)
            result['resource_sample_interval_seconds'] = .25
            result['resource_scope'] = 'Main host process RSS/CPU includes every native host thread; Python PTY child processes and Python harness excluded.'
            result['host_peak_sampled_RSS_bytes'] = max(row['host_RSS_bytes'] for row in result['resources'])
            result['host_CPU_seconds_during_workload'] = result['resources'][-1]['host_CPU_seconds'] - first_cpu
            result['host_average_logical_CPU_percent'] = result['host_CPU_seconds_during_workload'] / result['resources'][-1]['wall_seconds'] * 100
            for index, (spec, record) in enumerate(zip(specs, result['sessions'])):
                check(not record['ACK_errors'], str(record['ACK_errors']))
                live = record.pop('final_view')
                record['pipeline_metrics'] = live.get('metrics')
                record['cold_history_summary'] = live.get('history')
                archive_directory = state / (spec['generation'] + '.history')
                record['archive_bytes'] = sum(path.stat().st_size for path in archive_directory.glob('*.archive'))
                record['archive_index_bytes'] = (archive_directory / 'rows.index').stat().st_size
                verified = verify_journal(state / f"{spec['generation']}.journal", spec)
                record.update(verified)
                check(record['producer_body_bytes'] == RATE * args.seconds, 'producer emitted wrong body size')
                check(record['sequence'] == live['sequence'], 'live/journal watermark mismatch')
                replay_start = time.monotonic()
                replay = subprocess.run([str(frozen), 'replay', str(state / f"{spec['generation']}.journal")], text=True, capture_output=True, timeout=120, check=False)
                check(replay.returncode == 0, f'full replay failed: {replay.stderr[:2000]}')
                restored = json.loads(replay.stdout)
                check(restored['frame'] == live['frame'], 'full replay/live frame mismatch')
                check(restored.get('history') == live.get('history'), 'full replay/live cold continuation mismatch')
                check(restored['sequence'] == live['sequence'], 'replay sequence mismatch')
                record['full_replay_frame_equivalent'] = True
                verification = subprocess.run([str(frozen), 'verify-recovery', str(state / f"{spec['generation']}.journal")], text=True, capture_output=True, timeout=120, check=False)
                check(verification.returncode == 0, f'complete-state recovery failed: {verification.stderr[:2000]}')
                record['complete_recovery_verification'] = json.loads(verification.stdout)
                check(record['complete_recovery_verification']['complete_state_equal'], 'complete model equality not established')
                record['full_replay_seconds'] = time.monotonic() - replay_start
                record['producer_body_MiB_per_second'] = record['producer_body_bytes'] / record['producer_body_write_seconds'] / 1024**2
                record['observed_exit_seconds_from_producer_start'] = (record.pop('observed_exit_ns') - record['producer_start_ns']) / 1e9
                record['observed_exit_body_MiB_per_second'] = record['producer_body_bytes'] / record['observed_exit_seconds_from_producer_start'] / 1024**2
                record['producer_completed_body_within_target_seconds'] = record['producer_body_write_seconds'] <= args.seconds
                record['host_exit_observed_within_target_seconds'] = record['observed_exit_seconds_from_producer_start'] <= args.seconds
                record['ACK_percentiles_ms'] = {f'p{p}': percentile(record['ACK_observations_ms'], p / 100) for p in [50, 95, 99]}
            latencies = [value for session in result['sessions'] for value in session['ACK_observations_ms']]
            result['ACK_percentiles_ms'] = {f'p{p}': percentile(latencies, p / 100) for p in [50, 95, 99]}
            result['ACK_sample_count'] = len(latencies)
            result['percentile_method'] = 'nearest rank; input command start to snapshot observing parsed+durable ACK; includes IPC/poll lag, excludes present'
            result['all_journals_and_producer_output_valid'] = True
            result['all_full_replay_frames_equal'] = True
            result['all_checkpoint_tail_full_models_equal'] = all(s['complete_recovery_verification']['complete_state_equal'] for s in result['sessions'])
            result['all_cold_history_records_equal'] = all(s['complete_recovery_verification']['cold_history_equal'] for s in result['sessions'])
            result['target_producer_rate_achieved_all_streams'] = all(s['producer_completed_body_within_target_seconds'] for s in result['sessions'])
            result['target_host_exit_observed_within_deadline_all_streams'] = all(s['host_exit_observed_within_target_seconds'] for s in result['sessions'])
            result['status'] = 'integrity_pass' if result['target_producer_rate_achieved_all_streams'] else 'integrity_pass_rate_target_missed'
        except Exception as error:
            result['status'] = 'failed'
            result['errors'].append(str(error))
        finally:
            stop.set()
            for thread in threads:
                thread.join(timeout=4)
            cleanup_failures = cleanup_owned_processes(host, host_identity, owned)
            if any(thread.is_alive() for thread in threads):
                cleanup_failures.append('observer_thread_exit_unconfirmed')
            if cleanup_failures:
                result['status'] = 'failed'
                result.setdefault('errors', []).extend(cleanup_failures)
            result['cleanup_failures'] = cleanup_failures
            log.close()
            result['host_log_tail'] = (state / 'host.log').read_text(errors='replace')[-4000:]
            # Remove internal snapshots from partial/failure evidence: giant frames
            # are unnecessary and the run already records exact comparison results.
            for record in result['sessions']:
                record.pop('final_view', None)
            result['runner_wall_seconds_including_replay'] = time.monotonic() - started_wall
            retain_state = preserve_requested or result.get('status') != 'integrity_pass'
            state_directory.retain = retain_state
            result['state_retained'] = retain_state
            result['state_directory'] = str(state) if retain_state else None
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({key: result.get(key) for key in ['status', 'errors', 'ACK_sample_count', 'ACK_percentiles_ms', 'host_peak_sampled_RSS_bytes', 'host_average_logical_CPU_percent', 'target_producer_rate_achieved_all_streams', 'target_host_exit_observed_within_deadline_all_streams']}), flush=True)
    return 1 if result['status'] == 'failed' else 0


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=ROOT / 'benchmarks/results/native-pty-stress.json')
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/pigade-session-host')
    parser.add_argument('--seconds', type=int, default=60)
    parser.add_argument('--preserve-state', action='store_true',
                        help='retain this fresh private run directory after success or failure')
    parser.add_argument('--normal-history', action='store_true', help='Exercise normal scrollback/cold archive; input latency is not measured in this mode')
    arguments = parser.parse_args()
    check(1 <= arguments.seconds <= 60, 'test duration must be 1..60 seconds')
    sys.exit(run(arguments))
