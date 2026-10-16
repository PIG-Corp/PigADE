#!/usr/bin/env python3
"""Require an appended engineering entry in every nonempty integration commit."""
import subprocess
import sys

changed = subprocess.check_output(["git", "diff", "--cached", "--name-only", "-z"]).decode().split("\0")
changed = [name for name in changed if name]
if not changed:
    sys.exit(0)
diff = subprocess.check_output(["git", "diff", "--cached", "--unified=0", "--", "WORKLOG.md"], text=True)
entry = any(line.startswith("+## ") and " — " in line for line in diff.splitlines())
if not entry:
    sys.exit("Commit requires your own new WORKLOG entry. Use python3 scripts/log-change.py, then stage WORKLOG.md.")
