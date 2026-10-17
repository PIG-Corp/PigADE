import importlib.util
import json
import os
from pathlib import Path
import pty
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("soak_pty", ROOT / "scripts/soak-pty.py")
SOAK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SOAK)


def read_until(master, needle, timeout=3.0):
    data = bytearray()
    deadline = time.monotonic() + timeout
    while needle not in data and time.monotonic() < deadline:
        ready, _, _ = select.select([master], [], [], min(.05, deadline - time.monotonic()))
        if ready:
            try:
                data.extend(os.read(master, 65536))
            except OSError:
                break
    return bytes(data)


class SoakAckDiagnosticsTest(unittest.TestCase):
    def test_actual_pty_producer_reports_ack_lifecycle(self):
        with tempfile.TemporaryDirectory() as temp:
            sidecar = SOAK.AckSidechannel(Path(temp) / "events.jsonl")
            master, slave = pty.openpty()
            child = subprocess.Popen([sys.executable, "-I", "-u", "-c", SOAK.PRODUCER,
                                      "0", "30", "64", sidecar.address],
                                     stdin=slave, stdout=slave, stderr=slave, close_fds=True)
            os.close(slave)
            try:
                self.assertIn(b"READY:0", read_until(master, b"READY:0"))
                os.write(master, b"PING:SOAK-0-42\n")
                self.assertIn(b"ACK:SOAK-0-42", read_until(master, b"ACK:SOAK-0-42"))
                os.write(master, b"RELEASE:SOAK-0-42\n")
                deadline = time.monotonic() + 2
                events = {}
                while time.monotonic() < deadline:
                    events = sidecar.current_token(0, 42)
                    if "release_observed" in events:
                        break
                    time.sleep(.005)
                self.assertEqual(set(events), {"ping_received", "ack_write_completed", "release_observed"})
                ordered = [events[name] for name in
                           ("ping_received", "ack_write_completed", "release_observed")]
                self.assertEqual([event["event_sequence"] for event in ordered], [0, 1, 2])
                self.assertEqual([event["token_index"] for event in ordered], [42, 42, 42])
                self.assertEqual(sorted(event["producer_monotonic_ns"] for event in ordered),
                                 [event["producer_monotonic_ns"] for event in ordered])
                self.assertEqual(sidecar.summary()["records_written"], 3)
                diagnostics = SOAK.process_wait_diagnostics(
                    SOAK.STRESS.identity(os.getpid()), [SOAK.STRESS.identity(child.pid)])
                self.assertFalse(diagnostics["diagnostic_errors"])
                self.assertLessEqual(diagnostics["host"]["thread_count_captured"], 32)
                self.assertEqual(diagnostics["children"][0]["pid"], child.pid)
                os.write(master, b"FINISH\n")
                child.wait(timeout=3)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=3)
                os.close(master)
                sidecar.close()

    def test_sidechannel_file_cap_and_omission_count(self):
        with tempfile.TemporaryDirectory() as temp:
            sidecar = SOAK.AckSidechannel(Path(temp) / "events.jsonl", max_records=2)
            try:
                for sequence in range(5):
                    packet = SOAK.ACK_SIDECAR_PACKET.pack(1, 0, sequence, 100 + sequence, sequence)
                    sidecar.socket.sendto(packet, "\0" + sidecar.address[1:])
                summary = sidecar.summary()
                self.assertEqual(summary["records_written"], 2)
                self.assertEqual(summary["records_omitted"], 3)
                sidecar.file.flush()
                rows = [json.loads(line) for line in sidecar.path.read_text().splitlines()]
                self.assertEqual(len(rows), 2)
                self.assertNotIn("text", rows[0])
                self.assertNotIn("bytes", rows[0])
            finally:
                sidecar.close()

    def test_producer_send_failure_advances_sequence_once(self):
        with tempfile.TemporaryDirectory() as temp:
            sidecar = SOAK.AckSidechannel(Path(temp) / "events.jsonl")
            master, slave = pty.openpty()
            prefix = SOAK.PRODUCER.split("def emit(data):", 1)[0]
            body = r'''
original_sidecar=sidecar
class FailSecondSend:
 def __init__(self): self.calls=0
 def sendto(self,packet,address):
  self.calls+=1
  if self.calls==2: raise BlockingIOError("synthetic drop")
  return original_sidecar.sendto(packet,address)
sidecar=FailSecondSend()
milestone(1,b"SOAK-0-1")
milestone(1,b"SOAK-0-2")
milestone(1,b"SOAK-0-3")
os.write(1,('SEQ:%d'%sidecar_sequence).encode())
'''
            child = subprocess.Popen([sys.executable, "-I", "-u", "-c", prefix + body,
                                      "0", "30", "64", sidecar.address],
                                     stdin=slave, stdout=slave, stderr=slave, close_fds=True)
            os.close(slave)
            try:
                self.assertIn(b"SEQ:3", read_until(master, b"SEQ:3"))
                events = sidecar.summary()
                rows = [json.loads(line) for line in sidecar.path.read_text().splitlines()]
                self.assertEqual([row["event_sequence"] for row in rows], [0, 2])
                self.assertEqual(events["sequence_gaps"], 1)
            finally:
                child.wait(timeout=3)
                os.close(master)
                sidecar.close()

    def test_timeout_record_keeps_only_sanitized_frame_metadata(self):
        with tempfile.TemporaryDirectory() as temp:
            sidecar = SOAK.AckSidechannel(Path(temp) / "events.jsonl")
            view = {"state": "running", "sequence": 77, "storage_error": None,
                    "frame": {"cells": [{"text": "SECRET_FRAME_TEXT"}]}}
            other_view = {"state": "running", "sequence": 65, "storage_error": None,
                          "frame": {"cells": [{"text": "ACK:SOAK-0-9"}]}}
            record = SOAK.ack_timeout_record(
                elapsed=8.5, stream=0, token_index=9, views=[view, other_view],
                view_durations_ns=[1234, 500], view_completed_monotonic_ns=[1000, 500],
                ping_started_ns=10, ping_completed_ns=20, ping_status="writer_queue_admitted",
                ping_views_count=1, sidechannel=sidecar,
                process_diagnostics={"host": None, "children": [], "diagnostic_errors": []})
            encoded = json.dumps(record)
            self.assertNotIn("SECRET_FRAME_TEXT", encoded)
            self.assertIn('"target_token_visible": false', encoded)
            self.assertEqual(record["last_sessions"][0]["sequence"], 77)
            self.assertEqual(record["last_sessions"][0]["snapshot_duration_ns"], 1234)
            self.assertIsNone(record["last_sessions"][1]["target_token_visible"])
            self.assertEqual(record["last_sessions"][1]["snapshot_completed_monotonic_ns"], 500)
            self.assertIsNotNone(record["last_sessions"][1]["snapshot_age_at_diagnostic_ns"])
            self.assertEqual(record["ping"]["request_duration_ns"], 10)
            self.assertEqual(record["timeout_seconds"], SOAK.ACK_TIMEOUT_SECONDS)
            sidecar.close()


if __name__ == "__main__":
    unittest.main()
