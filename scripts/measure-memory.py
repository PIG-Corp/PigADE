#!/usr/bin/env python3
"""Measure release session-host RSS with 12 headless PTYs and hot history.

Example: python3 scripts/measure-memory.py --directory ~/.local/state/pigade/benchmarks
Only the host process is counted. The Python harness and PTY producer processes,
UI, GPU, provider processes, and provider/network activity are out of scope.
"""
import argparse
import datetime
import importlib.util
import json
import os
from pathlib import Path
import platform
import signal
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("stress_pty", ROOT / "scripts/stress-pty.py")
STRESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STRESS)
PRODUCER = r'''
import os,sys,termios,tty
stream=sys.argv[1]; tty.setraw(0,termios.TCSANOW)
def command():
 return sys.stdin.buffer.readline().removesuffix(b'\n')
def emit(data):
 view=memoryview(data)
 while view:
  view=view[os.write(1,view):]
emit(('READY:'+stream+'\r\n').encode())
while command()!=b'GO': pass
line=b'x'*119+b'\r\n'
for _ in range(10036): emit(line)
emit(('DONE:'+stream+'\r\n').encode())
while command()!=b'FINISH': pass
'''
SESSIONS = 12
ROWS = 36
COLS = 120
OUTPUT_LINES = 10036
HOT_ROWS = 10000
LIMIT_BYTES = 700 * 1024**2


def check(ok, message):
    if not ok:
        raise RuntimeError(message)


def host_sample(pid, ticks, page_size):
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(') ', 1)[1].split()
    cpu = (int(fields[11]) + int(fields[12])) / ticks
    rss = int(Path(f'/proc/{pid}/statm').read_text().split()[1]) * page_size
    return rss, cpu


def filesystem(path):
    return subprocess.check_output(['stat', '-f', '-c', '%T', str(path)], text=True).strip()


def run(args):
    args.directory.mkdir(parents=True, exist_ok=True)
    args.directory = args.directory.resolve()
    fs_type = filesystem(args.directory)
    check(fs_type not in ('tmpfs', 'ramfs'), f'durable filesystem required; found {fs_type}')
    check(args.binary.is_file() and os.access(args.binary, os.X_OK), f'release binary is not executable: {args.binary}')
    check(args.directory.is_dir(), 'benchmark directory is unavailable')
    result = {
        'captured_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'product_version': (ROOT / 'VERSION').read_text().strip(),
        'status': 'running', 'errors': [], 'sessions': [], 'resource_samples': [],
        'target': {'sessions': args.sessions, 'visible_sessions': min(4, args.sessions),
                   'geometry': {'rows': ROWS, 'cols': COLS}, 'output_lines_per_session': OUTPUT_LINES,
                   'hot_history_rows_per_session': HOT_ROWS, 'hold_seconds': args.hold_seconds,
                   'host_RSS_limit_bytes': LIMIT_BYTES},
        'measurement_scope': 'Release session-host process RSS and CPU only. Python harness, Python PTY producers, UI, GPU allocation, provider processes, models and network are excluded.',
        'interpretation': 'The 700 MiB host-only threshold is necessary for the product budget, not full product acceptance; it excludes UI, GPU and helper-process memory.',
        'context': 'Deterministic Python PTY producers only; no agent CLI, provider task, credentials, or network use.',
        'environment': {'os': platform.platform(), 'kernel': platform.release(),
                        'logical_cpu_count': os.cpu_count(), 'filesystem_type': fs_type,
                        'benchmark_directory': str(args.directory)},
        'protocol': STRESS.PROTOCOL,
        'steady_IPC_polling': not args.idle_no_poll,
        'source_sha256': {name: STRESS.digest(ROOT / name) for name in (
            'crates/session-host/src/lib.rs', 'crates/session-host/src/history.rs',
            'crates/session-host/src/checkpoint.rs', 'crates/storage/src/archive.rs',
            'crates/terminal/src/history.rs', 'scripts/measure-memory.py')},
    }
    host = None
    owned = []
    records = []
    sample_stop = threading.Event()
    phase = ['startup']
    sampler = None
    with tempfile.TemporaryDirectory(prefix='pigade-memory-', dir=args.directory) as temporary:
        state = Path(temporary)
        state.chmod(0o700)
        frozen = state / 'host-release'
        shutil.copyfile(args.binary, frozen)
        frozen.chmod(0o700)
        result['binary_sha256'] = STRESS.digest(frozen)
        result['binary_path'] = str(args.binary.resolve())
        result['binary_version'] = subprocess.check_output([str(frozen), '--version'], text=True).strip()
        log_path = state / 'host.log'
        log = log_path.open('wb')
        try:
            host = subprocess.Popen([str(frozen), 'serve', str(state)], stdin=subprocess.DEVNULL,
                                    stdout=subprocess.DEVNULL, stderr=log)
            result['host_pid'] = host.pid
            host_identity = STRESS.identity(host.pid)
            result['host_identity'] = host_identity
            endpoint = state / 'host.sock'
            deadline = time.monotonic() + 15
            while True:
                check(host.poll() is None, 'host exited during startup')
                try:
                    STRESS.request(endpoint, {'command': 'list'})
                    break
                except (OSError, RuntimeError):
                    check(time.monotonic() < deadline, 'host startup deadline')
                    time.sleep(.02)

            ticks = os.sysconf('SC_CLK_TCK')
            page_size = os.sysconf('SC_PAGE_SIZE')
            def record_sample(sample_phase):
                rss, cpu = host_sample(host.pid, ticks, page_size)
                result['resource_samples'].append({
                    'wall_seconds': time.monotonic() - started,
                    'phase': sample_phase, 'host_RSS_bytes': rss, 'host_CPU_seconds': cpu})
            def sample_resources():
                while not sample_stop.is_set():
                    try:
                        record_sample(phase[0])
                    except (FileNotFoundError, ProcessLookupError):
                        return
                    sample_stop.wait(.05)
            started = time.monotonic()
            sampler = threading.Thread(target=sample_resources, daemon=True)
            sampler.start()
            specs = []
            for index in range(args.sessions):
                spec = {'session': str(uuid.uuid4()), 'generation': str(uuid.uuid4()),
                        'operation': str(uuid.uuid4()), 'executable': sys.executable,
                        'args': ['-I', '-u', '-c', PRODUCER, str(index)], 'cwd': str(state),
                        'geometry': {'rows': ROWS, 'cols': COLS}}
                view = STRESS.request(endpoint, {'command': 'launch', 'spec': spec})[0]
                pid = view['pid']
                owned.append(STRESS.identity(pid))
                specs.append(spec)
                record = {'index': index, 'session': spec['session'], 'generation': spec['generation'],
                          'producer_pid': pid, 'geometry': spec['geometry'], 'journal': f"{spec['generation']}.journal"}
                records.append(record)
                result['sessions'].append(record)
                ready_by = time.monotonic() + 15
                while True:
                    view = STRESS.snapshot(endpoint, spec)
                    if f'READY:{index}' in STRESS.text(view):
                        break
                    check(time.monotonic() < ready_by, f'producer {index} READY timeout')
                    time.sleep(.02)
            result['startup_seconds'] = time.monotonic() - started
            phase[0] = 'load'
            record_sample('load')
            for spec in specs:
                STRESS.request(endpoint, {'command': 'input', 'session': spec['session'],
                                          'generation': spec['generation'], 'bytes': list(b'GO\n')})
            load_deadline = time.monotonic() + 180
            done = [False] * args.sessions
            last_views = [None] * args.sessions
            while not all(done):
                check(time.monotonic() < load_deadline, 'history load deadline')
                check(host.poll() is None, 'host exited during history load')
                for index, spec in enumerate(specs):
                    if done[index]:
                        continue
                    view = STRESS.snapshot(endpoint, spec)
                    last_views[index] = view
                    done[index] = (f'DONE:{index}' in STRESS.text(view) and
                                   view['frame']['history_lines'] == HOT_ROWS)
                if not all(done):
                    time.sleep(.25)
            result['load_seconds'] = time.monotonic() - started - result['startup_seconds']
            result['verified_hot_history_rows_per_session'] = [v['frame']['history_lines'] for v in last_views]
            result['retired_cold_rows_per_session'] = [v['history']['retired_rows'] for v in last_views]
            phase[0] = 'checkpoint_settle'
            record_sample('checkpoint_settle')
            # Allow the host's time-based checkpoint scheduler to run before hold.
            settle_until = time.monotonic() + 5
            while time.monotonic() < settle_until:
                check(host.poll() is None, 'host exited during checkpoint settle')
                for spec in specs[:min(4, args.sessions)]:
                    view = STRESS.snapshot(endpoint, spec)
                    check(view['frame']['history_lines'] == HOT_ROWS,
                          'history row count regressed during checkpoint settle')
                time.sleep(.25)
            phase[0] = 'steady'
            record_sample('steady')
            hold_until = time.monotonic() + args.hold_seconds
            next_snapshot = 0.0
            hold_snapshots = 0
            while time.monotonic() < hold_until:
                check(host.poll() is None, 'host exited during steady hold')
                now = time.monotonic()
                if not args.idle_no_poll and now >= next_snapshot:
                    for spec in specs[:min(4, args.sessions)]:
                        view = STRESS.snapshot(endpoint, spec)
                        check(view['frame']['history_lines'] == HOT_ROWS,
                              'history row count regressed during steady hold')
                    hold_snapshots += 1
                    next_snapshot = now + .25
                time.sleep(.02)
            result['steady_hold_snapshot_rounds'] = hold_snapshots
            result['resource_window_seconds'] = time.monotonic() - started
            sample_stop.set()
            sampler.join(timeout=2)
            samples = result['resource_samples']
            check(bool(samples), 'no host resource samples captured')
            result['sample_interval_seconds'] = .05
            result['resource_scope'] = 'Only /proc RSS and CPU for the main frozen release host PID, identified by boot ID/PID/start-time. Producer Python children and this harness are excluded.'
            result['peak_sampled_RSS_bytes_by_phase'] = {
                name: max((item['host_RSS_bytes'] for item in samples if item['phase'] == name), default=None)
                for name in ('startup', 'load', 'checkpoint_settle', 'steady')}
            result['host_CPU_seconds_during_resource_window'] = max(
                0, samples[-1]['host_CPU_seconds'] - samples[0]['host_CPU_seconds'])
            steady = [item for item in samples if item['phase'] == 'steady']
            if len(steady) >= 2 and steady[-1]['wall_seconds'] > steady[0]['wall_seconds']:
                result['steady_host_logical_CPU_percent'] = 100 * (steady[-1]['host_CPU_seconds'] - steady[0]['host_CPU_seconds']) / (steady[-1]['wall_seconds'] - steady[0]['wall_seconds'])
            result['peak_sampled_RSS_bytes'] = max(item['host_RSS_bytes'] for item in samples)
            result['memory_budget_pass'] = result['peak_sampled_RSS_bytes'] <= LIMIT_BYTES
            result['status'] = 'host_memory_budget_pass' if result['memory_budget_pass'] else 'host_memory_budget_missed'

            for spec in specs:
                STRESS.request(endpoint, {'command': 'input', 'session': spec['session'],
                                          'generation': spec['generation'], 'bytes': list(b'FINISH\n')})
            exit_deadline = time.monotonic() + 15
            while time.monotonic() < exit_deadline:
                all_exited = True
                for spec in specs:
                    view = STRESS.snapshot(endpoint, spec)
                    all_exited &= view['state'] == 'exited'
                if all_exited:
                    break
                time.sleep(.05)
            check(all_exited, 'producers did not exit before deadline')
            result['producers_exited_cleanly'] = True
            result['recovery_checks'] = []
            for record in records:
                journal = state / record['journal']
                command = subprocess.run([str(frozen), 'verify-recovery', str(journal)],
                                          text=True, capture_output=True, timeout=180, check=False)
                check(command.returncode == 0, f"verify-recovery failed for {record['generation']}: {command.stderr[-2000:]}")
                proof = json.loads(command.stdout)
                record['recovery_verification'] = proof
                record['terminal_profile'] = proof.get('profile')
                check(proof.get('complete_state_equal') is True, 'complete recovery state equality missing')
                result['recovery_checks'].append({'generation': record['generation'], 'complete_state_equal': True})
            result['recovery_verified_all_sessions'] = True
        except Exception as error:
            result['status'] = 'failed'
            result['errors'].append(f'{type(error).__name__}: {error}')
        finally:
            sample_stop.set()
            if sampler is not None:
                sampler.join(timeout=2)
            for owned_identity in owned:
                try:
                    if STRESS.identity(owned_identity[1]) == owned_identity:
                        os.kill(owned_identity[1], signal.SIGTERM)
                except (OSError, ValueError):
                    pass
            remaining = list(owned)
            child_deadline = time.monotonic() + 3
            while owned and time.monotonic() < child_deadline:
                remaining = []
                for owned_identity in owned:
                    try:
                        if STRESS.identity(owned_identity[1]) == owned_identity:
                            remaining.append(owned_identity)
                    except (OSError, ValueError):
                        pass
                if not remaining:
                    break
                time.sleep(.05)
            for owned_identity in remaining:
                try:
                    if STRESS.identity(owned_identity[1]) == owned_identity:
                        os.kill(owned_identity[1], signal.SIGKILL)
                except (OSError, ValueError):
                    pass
            if host is not None and host.poll() is None:
                try:
                    if STRESS.identity(host.pid) == host_identity:
                        host.terminate()
                        host.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    try:
                        if STRESS.identity(host.pid) == host_identity:
                            host.kill()
                            host.wait(timeout=5)
                    except (OSError, ValueError):
                        pass
                except (OSError, ValueError):
                    pass
            log.flush()
            log.close()
            result['host_log_tail'] = log_path.read_text(errors='replace')[-4000:]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    summary = {key: result.get(key) for key in ('status', 'errors', 'peak_sampled_RSS_bytes',
                                                 'memory_budget_pass', 'recovery_verified_all_sessions')}
    print(json.dumps(summary), flush=True)
    return 0 if result['status'] == 'host_memory_budget_pass' else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, required=True,
                        help='durable benchmark state directory; tmpfs/ramfs are rejected')
    parser.add_argument('--output', type=Path, default=ROOT / 'benchmarks/results/native-memory-12-sessions.json')
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/pigade-session-host')
    parser.add_argument('--sessions', type=int, default=SESSIONS)
    parser.add_argument('--hold-seconds', type=int, default=10)
    parser.add_argument('--idle-no-poll', action='store_true', help='Disable steady snapshots for host-only idle CPU measurement; does not certify UI idle')
    args = parser.parse_args()
    check(1 <= args.sessions <= 32, '--sessions must be 1..32')
    check(1 <= args.hold_seconds <= 3600, '--hold-seconds must be 1..3600')
    return run(args)


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as error:
        print(f'memory diagnostic: {error}', file=sys.stderr)
        sys.exit(2)
