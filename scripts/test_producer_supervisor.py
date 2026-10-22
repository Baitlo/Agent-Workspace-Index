"""Synthetic watchdog + failover safety checks; do not touch production.

Every test runs against a TemporaryDirectory with fake producers only. No real
PID, config, publication directory, or production process is referenced. The
failover suite proves the single-writer / fencing / handback / restart-boundary
properties that an empty lock file can never prove on its own.
"""

import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

from producer_supervisor import health_reason, read_json

SUP = Path(__file__).with_name("producer_supervisor.py")


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
                    str(SUP),
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


# --------------------------------------------------------------------------- #
# Failover / fencing failure-injection suite.
# --------------------------------------------------------------------------- #

# A fake producer. It enforces fencing itself: it appends one line to
# publication.log ONLY while the shared fence generation equals the generation
# it was launched at. This is the single-writer invariant under test.
FAKE_PRODUCER = r"""
import json, os, time, pathlib, sys
root = pathlib.Path(os.environ["AWI_TEST_ROOT"])
gen = int(os.environ.get("AWI_FENCE_GEN", "0"))
owner = os.environ.get("AWI_FENCE_OWNER", "anon")
hb_path = os.environ["AWI_PROD_HEARTBEAT"]
fence_path = os.environ.get("AWI_FENCE_PATH", "")
pub_log = root / "publication.log"
# Probe the env the supervisor actually handed us (guard-armed check).
try:
    (root / "env_probe.json").write_text(json.dumps({
        "AWI_FENCE_PATH": fence_path,
        "AWI_FENCE_GEN": os.environ.get("AWI_FENCE_GEN", ""),
        "AWI_FENCE_OWNER": os.environ.get("AWI_FENCE_OWNER", ""),
    }))
except Exception:
    pass
def rj(p):
    try: return json.loads(pathlib.Path(p).read_text())
    except Exception: return {}
while True:
    try:
        hb = {"pid": os.getpid(), "phase": "idle",
              "heartbeat_ms": int(time.time()*1000),
              "last_progress_ms": int(time.time()*1000)}
        tmp = hb_path + ".tmp"
        pathlib.Path(tmp).write_text(json.dumps(hb))
        os.replace(tmp, hb_path)
        active = int(rj(fence_path).get("generation", 0)) if fence_path else gen
        if active == gen:
            with open(pub_log, "a") as f:
                f.write("%.3f pid=%d gen=%d owner=%s\n" % (time.time(), os.getpid(), gen, owner))
    except Exception:
        pass
    time.sleep(0.05)
"""


def wait_for(pred, timeout=5.0, step=0.03):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        val = pred()
        if val:
            return val
        time.sleep(step)
    return None


def read_pub_log(path):
    rows = []
    if not path.exists():
        return rows
    for line in path.read_text().splitlines():
        m = re.match(r"^([\d.]+) pid=(\d+) gen=(\d+) owner=(\S+)$", line.strip())
        if m:
            rows.append(
                {
                    "t": float(m.group(1)),
                    "pid": int(m.group(2)),
                    "gen": int(m.group(3)),
                    "owner": m.group(4),
                }
            )
    return rows


class FailoverTests(unittest.TestCase):
    def setUp(self):
        self._cm = tempfile.TemporaryDirectory()
        self.root = Path(self._cm.name)
        self.fake = self.root / "fake_producer.py"
        self.fake.write_text(FAKE_PRODUCER)
        self.env = {**os.environ, "AWI_TEST_ROOT": str(self.root)}
        self.fence = self.root / "fence.json"
        self.w_heartbeat = self.root / "worker.heartbeat.json"
        self.w_status = self.root / "worker.status.json"
        self.fleet_status = self.root / "fleet.status.json"
        self.state_dir = self.root / "failover.state"
        self.pub = self.root / "publication.log"
        self.procs = []

    def tearDown(self):
        for p in self.procs:
            try:
                if p.poll() is None:
                    os.killpg(p.pid, signal.SIGKILL)
                    p.wait(timeout=5)
            except (ProcessLookupError, PermissionError, subprocess.TimeoutExpired):
                pass
        # The master launches the fallback in its OWN detached session
        # (start_new_session), so it is not a child of any process we track.
        # Sweep every process whose cmdline references this test's temp root so
        # no fake producer is leaked on the (production) host.
        self._sweep_root()
        self._cm.cleanup()

    def _sweep_root(self):
        needle = str(self.root)
        try:
            out = subprocess.run(
                ["pgrep", "-f", needle],
                check=False,
                capture_output=True,
                text=True,
            ).stdout.split()
        except (OSError, subprocess.SubprocessError):
            out = []
        for pid in out:
            try:
                os.kill(int(pid), signal.SIGKILL)
            except (ProcessLookupError, PermissionError, ValueError):
                pass

    def _spawn(self, argv, extra_env=None):
        env = dict(self.env)
        if extra_env:
            env.update(extra_env)
        p = subprocess.Popen(
            argv,
            env=env,
            start_new_session=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.procs.append(p)
        return p

    def _seed_fence(self, gen, owner="worker:1"):
        import producer_supervisor as ps

        # advance the fence to exactly `gen` owned by the worker
        while True:
            cur = read_json(self.fence).get("generation", 0)
            if cur >= gen:
                break
            got = ps.cas_bump_fence(self.fence, owner)
            if got is None:
                time.sleep(0.02)

    def _worker_supervisor(self, gen=None, claim=False, owner="worker:1"):
        """On-worker supervise() writing the SHARED worker status/heartbeat."""
        if gen is not None:
            self._seed_fence(gen, owner)
        cfg = {
            "command": [sys.executable, str(self.fake)],
            "heartbeat": str(self.w_heartbeat),
            "status": str(self.w_status),
            "log": str(self.root / "worker.sup.log"),
            "fence": str(self.fence),
            "fence_owner": owner,
            "poll_seconds": 0.05,
            "startup_seconds": 2,
            "heartbeat_seconds": 10,
            "progress_seconds": 60,
            "env": {
                "AWI_PROD_HEARTBEAT": str(self.w_heartbeat),
                "AWI_FENCE_PATH": str(self.fence),
            },
        }
        if claim:
            cfg["fence_claim"] = True
        elif gen is not None:
            cfg["fence_gen"] = gen
        p = self.root / f"worker.cfg.{len(self.procs)}.json"
        p.write_text(json.dumps(cfg))
        return self._spawn([sys.executable, str(SUP), str(p)])

    def _master(self, extra=None):
        cfg = {
            "owner": "master:1",
            "auto_recover": True,
            "timeout_seconds": 0.5,
            "auto_recover_grace_seconds": 0.5,
            "auto_recover_drain_seconds": 2,
            "poll_seconds": 0.1,
            "status": str(self.fleet_status),
            "failover_state_dir": str(self.state_dir),
            "producers": [
                {
                    "name": "shard",
                    "status": str(self.w_status),
                    "fence": str(self.fence),
                    "fallback_command": [sys.executable, str(self.fake)],
                }
            ],
        }
        if extra:
            cfg.update(extra)
        p = self.root / "master.cfg.json"
        p.write_text(json.dumps(cfg))
        return self._spawn([sys.executable, str(SUP), str(p), "--monitor-only"])

    def _stop(self, proc):
        try:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=5)
        except (ProcessLookupError, PermissionError, subprocess.TimeoutExpired):
            pass

    # -- T1: takeover after worker+supervisor disconnect --------------------- #
    def test_takeover_after_disconnect_single_writer(self):
        # Worker (gen=1) publishes, then worker+supervisor die together.
        self._worker_supervisor(gen=1, owner="worker:1")
        rows = wait_for(lambda: read_pub_log(self.pub), timeout=5)
        self.assertTrue(rows and rows[-1]["gen"] == 1, "worker should publish gen=1")
        # Kill the whole worker process group (supervisor + producer).
        self._stop(self.procs[-1])
        # Shared worker status freezes -> master sees unreachable.
        wait_for(lambda: None, timeout=1.0)
        self._master()

        # After grace, master bumps fence to gen=2 and starts the fallback.
        def recovered():
            f = json.loads(self.fence.read_text())
            rows = read_pub_log(self.pub)
            return f["generation"] == 2 and rows and rows[-1]["gen"] == 2

        ok = wait_for(recovered, timeout=8)
        self.assertTrue(ok, "master should take over at fence gen=2")
        # Single-writer: after the first gen=2 write, NO gen=1 write appears.
        rows = read_pub_log(self.pub)
        first_g2 = next(i for i, r in enumerate(rows) if r["gen"] == 2)
        after = rows[first_g2:]
        self.assertTrue(
            all(r["gen"] == 2 for r in after),
            "old gen=1 writer must not publish after takeover",
        )

    # -- T2: fencing stops a partitioned (still-alive) old writer ----------- #
    def test_fencing_stops_partitioned_old_worker(self):
        # Old worker producer still running & publishing gen=1, but its on-worker
        # supervisor died so the SHARED status never refreshes (partition).
        self._spawn(
            [sys.executable, str(self.fake)],
            extra_env={
                "AWI_FENCE_GEN": "1",
                "AWI_FENCE_OWNER": "worker:1",
                "AWI_PROD_HEARTBEAT": str(self.w_heartbeat),
                "AWI_FENCE_PATH": str(self.fence),
            },
        )
        # write fence at gen=1 first (old worker's leadership)
        import producer_supervisor as ps

        self.assertEqual(ps.cas_bump_fence(self.fence, "worker:1"), 1)
        rows = wait_for(
            lambda: [r for r in read_pub_log(self.pub) if r["gen"] == 1], timeout=5
        )
        self.assertTrue(rows, "partitioned old worker should publish gen=1")
        old_pid = rows[-1]["pid"]
        # Master detects stale status and fails over.
        self._master()
        wait_for(
            lambda: json.loads(self.fence.read_text())["generation"] == 2, timeout=8
        )

        # The partitioned old writer must self-stop the moment it sees gen=2.
        def old_stopped():
            rows = read_pub_log(self.pub)
            g2 = [r for r in rows if r["gen"] == 2]
            if not g2:
                return False
            cutoff = g2[0]["t"]
            tail = [r for r in rows if r["t"] > cutoff + 0.4 and r["gen"] == 1]
            return len(tail) == 0

        ok = wait_for(old_stopped, timeout=6)
        self.assertTrue(ok, "partitioned gen=1 writer must stop after fence bump")
        # and the old process actually exits (it checks the fence each loop).
        try:
            os.kill(old_pid, 0)
            still = True
        except ProcessLookupError:
            still = False
        # The fake producer itself loops; it stays alive but writes nothing.
        # The key guarantee is that it STOPPED WRITING (asserted above).
        self.assertFalse(still and read_pub_log(self.pub)[-1]["gen"] == 1)

    # -- T3: handback drains fallback, no double-write, no mutual kill ------- #
    def test_handback_drains_fallback_and_worker_reclaims(self):
        # worker dies -> master takes over (gen=2 fallback publishing)
        self._worker_supervisor(gen=1, owner="worker:1")
        wait_for(
            lambda: [r for r in read_pub_log(self.pub) if r["gen"] == 1], timeout=5
        )
        self._stop(self.procs[-1])
        wait_for(lambda: None, timeout=1.0)
        self._master()
        wait_for(
            lambda: json.loads(self.fence.read_text())["generation"] == 2, timeout=8
        )
        fb_rows = wait_for(
            lambda: [r for r in read_pub_log(self.pub) if r["gen"] == 2], timeout=8
        )
        self.assertTrue(fb_rows, "fallback should publish gen=2")
        fb_pid = fb_rows[-1]["pid"]
        # Worker returns: a NEW on-worker supervisor claims fence -> gen=3 and
        # starts refreshing the SHARED worker status.
        self._worker_supervisor(claim=True, owner="worker:1")
        wait_for(
            lambda: json.loads(self.fence.read_text())["generation"] == 3, timeout=8
        )
        # Master sees fresh shared status -> drains the fallback.
        wait_for(
            lambda: (
                self.fleet_status.exists()
                and json.loads(self.fleet_status.read_text())["producers"][0]["state"]
                != "auto_recovering_local"
            ),
            timeout=8,
        )

        # Fallback process must be dead after drain.
        def fb_dead():
            try:
                os.kill(fb_pid, 0)
                return False
            except ProcessLookupError:
                return True

        ok = wait_for(fb_dead, timeout=6)
        self.assertTrue(ok, "master must drain (kill) the local fallback")
        # gen=3 worker is the sole writer; no gen=2 after worker reclaim.
        g3 = wait_for(
            lambda: [r for r in read_pub_log(self.pub) if r["gen"] == 3],
            timeout=8,
        )
        self.assertTrue(g3, "returning worker must publish at gen=3")
        rows = read_pub_log(self.pub)
        cutoff = g3[0]["t"]
        self.assertTrue(
            all(r["gen"] != 2 or r["t"] < cutoff for r in rows),
            "fallback must not publish after worker reclaims",
        )

    # -- T4: monitor restart reattaches, no double launch ------------------- #
    def test_monitor_restart_reattaches_no_double_launch(self):
        self._worker_supervisor(gen=1, owner="worker:1")
        wait_for(
            lambda: [r for r in read_pub_log(self.pub) if r["gen"] == 1], timeout=5
        )
        self._stop(self.procs[-1])
        wait_for(lambda: None, timeout=1.0)
        m = self._master()
        wait_for(
            lambda: json.loads(self.fence.read_text())["generation"] == 2, timeout=8
        )
        fb_rows = wait_for(
            lambda: [r for r in read_pub_log(self.pub) if r["gen"] == 2], timeout=8
        )
        fb_pid = fb_rows[-1]["pid"]
        # Restart the monitor (same state dir).
        self._stop(m)
        wait_for(lambda: None, timeout=0.5)
        self._master()  # reattaches
        wait_for(lambda: None, timeout=2.0)
        # Exactly one fallback pid keeps writing at gen=2 (no double launch).
        rows = [r for r in read_pub_log(self.pub) if r["gen"] == 2]
        pids = {r["pid"] for r in rows[-20:]}
        self.assertLessEqual(len(pids), 1, "restart must not launch a 2nd writer")
        self.assertIn(fb_pid, pids)

    # -- T5: fallback's own heartbeat must NOT look like worker recovery ---- #
    def test_no_self_heartbeat_oscillation(self):
        self._worker_supervisor(gen=1, owner="worker:1")
        wait_for(
            lambda: [r for r in read_pub_log(self.pub) if r["gen"] == 1], timeout=5
        )
        self._stop(self.procs[-1])
        wait_for(lambda: None, timeout=1.0)
        self._master()
        rows = wait_for(
            lambda: [r for r in read_pub_log(self.pub) if r["gen"] == 2], timeout=8
        )
        fb_pid = rows[-1]["pid"]
        # Watch for ~2.5s: the fallback pid must remain stable (no relaunch
        # oscillation) and fleet state stays auto_recovering_local.
        stable_since = time.monotonic()
        last_pid = fb_pid
        while time.monotonic() - stable_since < 2.5:
            tail = [r["pid"] for r in read_pub_log(self.pub) if r["gen"] == 2][-10:]
            self.assertTrue(
                all(p == last_pid for p in tail),
                "fallback must not be relaunched by its own heartbeat",
            )
            time.sleep(0.1)
        state = json.loads(self.fleet_status.read_text())["producers"][0]["state"]
        self.assertEqual(
            state,
            "auto_recovering_local",
            "fallback must keep running; no false recovery",
        )

    # -- T6: second supervisor on the same lock refuses --------------------- #
    def test_second_supervisor_refuses_lock(self):
        cfg = {
            "command": [sys.executable, "-c", "import time; time.sleep(30)"],
            "heartbeat": str(self.root / "hb"),
            "status": str(self.root / "status2"),
            "log": str(self.root / "sup.log"),
            "poll_seconds": 0.1,
        }
        cp = self.root / "lock.cfg.json"
        cp.write_text(json.dumps(cfg))
        first = self._spawn([sys.executable, str(SUP), str(cp)])
        wait_for(lambda: self.root.joinpath("status2").exists(), timeout=3)
        second = subprocess.run(
            [sys.executable, str(SUP), str(cp)],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertNotEqual(
            second.returncode, 0, "second supervisor must refuse to start"
        )
        self.assertIn("holds the lease", second.stderr)
        self._stop(first)

    # -- T7: CAS contention fails closed (exactly one winner) --------------- #
    def test_cas_bump_fail_closed_on_contention(self):
        import producer_supervisor as ps

        # seed gen=1 owned by worker
        self.assertEqual(ps.cas_bump_fence(self.fence, "worker:1"), 1)
        # caller believes current gen is 1 -> bump to 2 (wins)
        self.assertEqual(ps.cas_bump_fence(self.fence, "master:fb", expect_gen=1), 2)
        # another caller still believing gen=1 must LOSE (fail closed), not
        # blindly overwrite to 3.
        self.assertIsNone(
            ps.cas_bump_fence(self.fence, "intruder", expect_gen=1),
            "CAS must reject a stale expect_gen",
        )
        # fence must remain at 2.
        self.assertEqual(read_json(self.fence)["generation"], 2)

    # -- T8: HONEST GAP DEMO ------------------------------------------------ #
    # A publisher that does NOT consult the fence before publishing keeps
    # writing even after the supervisor has failed over. This proves the
    # supervisor-side fence is necessary coordination but NOT, by itself, an
    # atomic double-write guarantee: the real Rust publisher must validate the
    # token pre-publish. Kept as a regression marker, not a green check.
    def test_gap_uncaged_publisher_keeps_writing_after_takeover(self):
        import producer_supervisor as ps

        uncaged = (
            "import os,time,pathlib\n"
            f"root=pathlib.Path({str(self.root)!r})\n"
            "while True:\n"
            " with open(root/'uncaged.log','a') as f: f.write('W\\n')\n"
            " time.sleep(0.02)\n"
        )
        script = self.root / "uncaged.py"
        script.write_text(uncaged)
        ps.cas_bump_fence(self.fence, "worker:1")  # gen=1
        old = self._spawn([sys.executable, str(script)])  # never checks fence
        wait_for(lambda: (self.root / "uncaged.log").exists(), timeout=3)
        # master takes over (bumps fence to 2)
        self.assertEqual(ps.cas_bump_fence(self.fence, "master:fb"), 2)
        time.sleep(0.5)
        # The uncaged old process is STILL writing -> double-write would occur.
        lines_after = (self.root / "uncaged.log").read_text().count("W")
        self.assertGreater(
            lines_after,
            0,
            "uncaged publisher keeps writing: publisher-side "
            "token check is REQUIRED (documented gap)",
        )
        self._stop(old)

    # -- T9: primary worker must receive AWI_FENCE_PATH (guard armed) -------- #
    def test_primary_worker_env_carries_fence_path(self):
        # Deliberately do NOT put AWI_FENCE_PATH in config env: the supervisor
        # itself must forward it, otherwise the Rust commit guard sees no path
        # and is a no-op (primary writer unguarded).
        self._seed_fence(1, "worker:1")
        cfg = {
            "command": [sys.executable, str(self.fake)],
            "heartbeat": str(self.w_heartbeat),
            "status": str(self.w_status),
            "log": str(self.root / "w2.sup.log"),
            "fence": str(self.fence),
            "fence_owner": "worker:1",
            "fence_gen": 1,
            "poll_seconds": 0.05,
            "env": {"AWI_PROD_HEARTBEAT": str(self.w_heartbeat)},
        }
        p = self.root / "worker.probe.cfg.json"
        p.write_text(json.dumps(cfg))
        proc = self._spawn([sys.executable, str(SUP), str(p)])
        probe = wait_for(
            lambda: (
                json.loads((self.root / "env_probe.json").read_text())
                if (self.root / "env_probe.json").exists()
                else None
            ),
            timeout=5,
        )
        self.assertIsNotNone(probe, "fake producer never probed its env")
        self.assertEqual(
            probe["AWI_FENCE_PATH"],
            str(self.fence),
            "supervisor must forward AWI_FENCE_PATH to primary child",
        )
        self.assertEqual(probe["AWI_FENCE_GEN"], "1")
        self._stop(proc)


if __name__ == "__main__":
    unittest.main()
