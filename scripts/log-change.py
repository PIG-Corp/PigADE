#!/usr/bin/env python3
"""Append one accountable agent entry under an advisory lock; never rewrite history."""
import argparse
import datetime
import fcntl
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--agent", required=True)
parser.add_argument("--summary", required=True)
parser.add_argument("--files", nargs="+", required=True)
parser.add_argument("--validation", required=True)
parser.add_argument("--next", required=True)
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
entry = (
    f"\n## {timestamp} — {args.agent}\n\n"
    f"{args.summary}\n\n"
    f"- Files: {', '.join(args.files)}\n"
    f"- Verification: {args.validation}\n"
    f"- Risks / next: {args.next}\n"
)
with (root / "WORKLOG.md").open("a", encoding="utf-8") as worklog:
    fcntl.flock(worklog, fcntl.LOCK_EX)
    worklog.write(entry)
    worklog.flush()
    import os
    os.fsync(worklog.fileno())
    fcntl.flock(worklog, fcntl.LOCK_UN)
