#!/usr/bin/env python3
"""Collect explicit benchmark context without credentials or private environment."""
import datetime
import json
import pathlib
import platform
import subprocess


def command(*argv):
    try:
        result = subprocess.run(argv, text=True, capture_output=True, timeout=15, check=False)
        return {"exit_code": result.returncode, "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"unavailable": str(error)}


result = {
    "captured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "system": platform.platform(),
    "cpu": command("lscpu"),
    "memory": command("free", "-b"),
    "filesystem": command("stat", "-f", "-c", "%T", "."),
    "disks": command("lsblk", "-o", "NAME,MODEL,ROTA,FSTYPE,SIZE"),
    "gpu_driver": command("lspci", "-nnk"),
    "monitors": command("hyprctl", "monitors", "-j"),
    "compositor": command("hyprctl", "version"),
    "rustc": command("rustc", "--version", "--verbose"),
    "node": command("node", "--version"),
    "cli_versions": {name: command(name, "--version") for name in ("claude", "codex", "opencode")},
    "power_governor": pathlib.Path("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").read_text().strip(),
    "note": "Current workstation; 1080p/180Hz is not the SPEC reference 1080p/60Hz. Component measurements cannot certify UI budgets.",
}
print(json.dumps(result, ensure_ascii=False, indent=2))
