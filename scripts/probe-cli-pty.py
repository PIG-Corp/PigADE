#!/usr/bin/env python3
"""Launch installed CLI version/help probes in the real PigADE PTY; no tasks/auth changes."""
import argparse
import datetime
import json
import pathlib
import shutil
import socket
import struct
import subprocess
import tempfile
import time
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=pathlib.Path, default=pathlib.Path("target/release/pigade-session-host"))
parser.add_argument("--directory", type=pathlib.Path, required=True)
args = parser.parse_args()
args.directory.mkdir(parents=True, exist_ok=True)
binary = args.binary.resolve()
results = {"captured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(), "scope": "PTY launch of --version/--help only; not interactive TUI, context, auth, or resume certification", "probes": []}


def exact(stream, length):
    data = bytearray()
    while len(data) < length:
        chunk = stream.recv(length - len(data))
        if not chunk:
            raise RuntimeError("unexpected IPC EOF")
        data.extend(chunk)
    return data


with tempfile.TemporaryDirectory(prefix="cli-probe-", dir=args.directory) as directory:
    root = pathlib.Path(directory)
    endpoint = root / "host.sock"
    with (root / "host.log").open("w") as log:
        host = subprocess.Popen([str(binary), "serve", str(root)], stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 10
            while not endpoint.exists():
                if host.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError("host did not start")
                time.sleep(0.01)

            def request(command):
                payload = json.dumps({"protocol": 2, "command": command}).encode()
                with socket.socket(socket.AF_UNIX) as stream:
                    stream.settimeout(15)
                    stream.connect(str(endpoint))
                    stream.sendall(struct.pack("<I", len(payload)) + payload)
                    length = struct.unpack("<I", exact(stream, 4))[0]
                    if not 0 < length <= 8 * 1024 * 1024:
                        raise RuntimeError("invalid IPC response size")
                    response = json.loads(exact(stream, length))
                if response["error"]:
                    raise RuntimeError(response["error"])
                return response["views"]

            for cli in ("claude", "codex", "opencode"):
                executable = shutil.which(cli)
                if executable is None:
                    results["probes"].append({"cli": cli, "status": "missing"})
                    continue
                for option in ("--version", "--help"):
                    session, generation, operation = (str(uuid.uuid4()) for _ in range(3))
                    spec = {"session": session, "generation": generation, "operation": operation,
                            "executable": str(pathlib.Path(executable).resolve()), "args": [option],
                            "cwd": str(root), "geometry": {"rows": 36, "cols": 120}}
                    start = time.monotonic()
                    request({"command": "launch", "spec": spec})
                    deadline = time.monotonic() + 15
                    while True:
                        view = request({"command": "snapshot", "session": session, "generation": generation})[0]
                        if view["storage_error"] or view["state"] == "degraded":
                            raise RuntimeError(view["storage_error"])
                        if view["state"] == "exited":
                            break
                        if time.monotonic() > deadline:
                            request({"command": "stop", "session": session, "generation": generation})
                            raise RuntimeError(f"{cli} {option} did not exit within timeout")
                        time.sleep(0.02)
                    text = "".join(cell["text"] for cell in view["frame"]["cells"])
                    # Public help/version content only. Never inspect provider home files.
                    results["probes"].append({"cli": cli, "argv": [option], "state": view["state"],
                                              "session": session, "generation": generation,
                                              "durable_sequence": view["sequence"],
                                              "elapsed_ms": (time.monotonic() - start) * 1000,
                                              "journal_bytes": (root / f"{generation}.journal").stat().st_size,
                                              "version_text": text.strip() if option == "--version" else None})
        finally:
            host.terminate()
            try:
                host.wait(timeout=5)
            except subprocess.TimeoutExpired:
                host.kill()
                host.wait()
print(json.dumps(results, ensure_ascii=False, indent=2))
