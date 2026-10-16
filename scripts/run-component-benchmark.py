#!/usr/bin/env python3
"""Run the release component benchmark on an explicit durable filesystem."""
import argparse
import datetime
import json
import os
import pathlib
import resource
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--directory", type=pathlib.Path, required=True)
args = parser.parse_args()
args.directory.mkdir(parents=True, exist_ok=True)
filesystem = subprocess.check_output(["stat", "-f", "-c", "%T", str(args.directory)], text=True).strip()
if filesystem in ("tmpfs", "ramfs"):
    parser.error("a RAM filesystem cannot measure durable device writes")
root = pathlib.Path(__file__).resolve().parents[1]
environment = dict(os.environ, PIGADE_BENCH_DIR=str(args.directory.resolve()))
start = time.monotonic()
process = subprocess.run([str(root / "target/release/examples/benchmark")], env=environment, capture_output=True, text=True, check=True)
result = json.loads(process.stdout)
usage = resource.getrusage(resource.RUSAGE_CHILDREN)
result.update({
    "captured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "product_version": (root / "VERSION").read_text().strip(),
    "filesystem": filesystem,
    "benchmark_process_peak_rss_kib": usage.ru_maxrss,
    "benchmark_process_user_seconds": usage.ru_utime,
    "benchmark_process_system_seconds": usage.ru_stime,
    "runner_wall_seconds": time.monotonic() - start,
    "scope_warning": "Component process only; no UI, IPC presentation, GPU, concurrent32sessions, long-history or soak certification.",
})
print(json.dumps(result, ensure_ascii=False, indent=2))
