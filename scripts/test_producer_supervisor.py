"""Synthetic watchdog checks; do not touch production processes."""

import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

from producer_supervisor import health_reason


class SupervisorTests(unittest.TestCase):
    def test_idle_progress_and_startup_deadlines(self):
        config = {"startup_seconds": 20, "heartbeat_seconds": 5, "progress_seconds": 10}
        idle = {"pid": 1, "phase": "idle", "heartbeat_ms": 100_000}
        self.assertIsNone(health_reason(idle, 1, 100, 100, config, 100_000))
        self.assertEqual(
            health_reason(idle, 1, 100, 100, config, 110_000), "heartbeat expired"
        )
        busy = {**idle, "phase": "scan"}
        self.assertIsNone(health_reason(busy, 1, 100, 3, config, 106_000))
        self.assertEqual(
            health_reason(busy, 1, 100, 11, config, 100_000), "useful progress expired"
        )
        self.assertIsNone(health_reason(idle, 2, 10, 10, config, 100_000))
        self.assertEqual(
            health_reason(idle, 2, 21, 21, config, 100_000), "startup heartbeat missing"
        )

    def test_exit_restart_and_owned_shutdown(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / "child.py"
            fixture.write_text(
                "import json, os, pathlib, time\n"
                f"root = pathlib.Path({str(root)!r})\n"
                "marker = root / 'starts'\n"
                "count = int(marker.read_text())+1 if marker.exists() else 1\n"
                "marker.write_text(str(count))\n"
                "if count == 1: raise SystemExit(7)\n"
                "while True:\n"
                " (root/'heartbeat').write_text(json.dumps(dict(pid=os.getpid(),"
                "phase='idle',heartbeat_ms=int(time.time()*1000),last_progress_ms=1)))\n"
                " time.sleep(0.05)\n"
            )
            config = {
                "command": [sys.executable, str(fixture)],
                "heartbeat": str(root / "heartbeat"),
                "status": str(root / "status"),
                "log": str(root / "log"),
                "poll_seconds": 0.05,
                "startup_seconds": 1,
                "heartbeat_seconds": 1,
                "progress_seconds": 1,
            }
            config_path = root / "config.json"
            config_path.write_text(json.dumps(config))
            supervisor = subprocess.Popen(
                [
                    sys.executable,
                    str(Path(__file__).with_name("producer_supervisor.py")),
                    str(config_path),
                ]
            )
            child_pid = None
            try:
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    try:
                        status = json.loads((root / "status").read_text())
                        if (
                            status["restarts"] == 1
                            and status["heartbeat"].get("phase") == "idle"
                        ):
                            child_pid = status["producer_pid"]
                            break
                    except (OSError, ValueError, KeyError):
                        pass
                    time.sleep(0.05)
                self.assertIsNotNone(child_pid)
                time.sleep(1.2)
                self.assertEqual((root / "starts").read_text(), "2")
            finally:
                supervisor.terminate()
                supervisor.wait(timeout=15)
            with self.assertRaises(ProcessLookupError):
                os.kill(child_pid, 0)


if __name__ == "__main__":
    unittest.main()
