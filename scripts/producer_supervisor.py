#!/usr/bin/env python3
"""Supervise one owned AWI producer; no third-party dependencies.

JSON config (per worker, mode without --monitor-only):
  command (argv list), heartbeat, log, status, startup_seconds (1800),
  heartbeat_seconds (120), progress_seconds (900), poll_seconds (5).
An optional env object adds non-secret environment settings.

Optional single-writer fencing (opt-in, backward compatible):
  fence       : shared path to fence.json (same volume as the publication).
  fence_gen   : the generation this supervisor is allowed to write at.
  fence_owner : stable identity of this supervisor (e.g. "fast:worker-12").

IMPORTANT SCOPE: this supervisor-side fence is failover *coordination*, not an
atomic double-write mutex. The onus of not publishing is on the publisher: the
real Rust producer (producer.rs) must itself validate the fence generation
BEFORE each publish and refuse to write if it is no longer the holder. Without
that publisher-side check, a partitioned old writer keeps publishing and this
supervisor can only stop it best-effort (see the `test_gap_uncaged_publisher_*`
regression marker). The fence CAS below uses an flock; on NFS this is advisory
and must run on a POSIX filesystem where locks are honored, otherwise the
failover fails closed (it refuses to launch a second writer) rather than risk
a split brain.

--monitor-only reads worker status paths and writes fleet health. Run this on
the master host. With auto_recover enabled the master is the failover owner:
if a worker AND its on-worker supervisor are both unreachable beyond the grace
period, the master CAS-bumps the shared fence generation and launches a local
fenced fallback producer for that shard. The fallback uses its OWN isolated
heartbeat/status namespace under failover_state_dir, so it can never be
mistaken for the real worker's heartbeat (no self-triggered oscillation).
When the real worker's SHARED status becomes fresh again, the master drains
(waits for) the local fallback to fully exit before returning ownership.
"""

import argparse
import fcntl
import json
import logging
import logging.handlers
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path


def read_json(path):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError):
        return {}


def atomic_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(f".tmp-{os.getpid()}")
    with temporary.open("w") as stream:
        json.dump(value, stream, ensure_ascii=False)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def health_reason(heartbeat, child_pid, elapsed, busy_elapsed, config, now_ms):
    """Idle is healthy without publication; busy work has its own deadline."""
    if heartbeat.get("pid") != child_pid:
        if elapsed > config.get("startup_seconds", 1800):
            return "startup heartbeat missing"
        return None
    if now_ms - heartbeat.get("heartbeat_ms", 0) > 1000 * config.get(
        "heartbeat_seconds", 120
    ):
        # Indexing/publication may legitimately block inside a single call.
        deadline = config.get("progress_seconds", 900)
        if heartbeat.get("phase") == "idle" or busy_elapsed > deadline:
            return "heartbeat expired"
    if heartbeat.get("phase") != "idle" and busy_elapsed > config.get(
        "progress_seconds", 900
    ):
        return "useful progress expired"
    return None


def stop_group(child):
    if child.poll() is not None:
        return
    os.killpg(child.pid, signal.SIGTERM)
    try:
        child.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=10)


# --------------------------------------------------------------------------- #
# Single-writer fencing.
#
# fence.json (shared, same volume as the publication):
#   {"generation": int, "owner": str, "updated_ms": int}
# The active writer is the holder of the current generation. A supervisor only
# lets its child publish while the fence still points at its own generation.
# This turns the "empty lock proves nothing" gap into an explicit generation
# that a partitioned old worker observes the instant the shared file is visible.
# --------------------------------------------------------------------------- #

FENCE_RETRY = 20


def read_fence(path):
    return read_json(path)


def cas_bump_fence(path, owner, expect_gen=None):
    """Atomically claim the fence for `owner` at generation = max(current)+1.

    `expect_gen` (optional) is the generation the caller thinks it is replacing.
    If provided and the file's generation has moved on, returns None (lost the
    race). Returns the new generation on success. Uses an exclusive flock on a
    sibling lock to serialize the CAS across processes sharing the volume.
    """
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    lock_path = path.with_suffix(path.suffix + ".caslock")
    for _ in range(FENCE_RETRY):
        try:
            fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o644)
        except OSError:
            time.sleep(0.05)
            continue
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            current = read_json(path)
            cur_gen = int(current.get("generation", 0))
            if expect_gen is not None and cur_gen != int(expect_gen):
                return None
            new_gen = cur_gen + 1
            atomic_json(
                path,
                {
                    "generation": new_gen,
                    "owner": owner,
                    "updated_ms": int(time.time() * 1000),
                },
            )
            return new_gen
        finally:
            fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)
    return None


def fence_lost(config, fence_gen, fence_owner):
    """Return a reason string if this supervisor has lost the fence, else None.

    Lost when the shared fence exists, carries a HIGHER generation, and that
    generation belongs to a different owner. A missing/younger fence means we
    still hold leadership.
    """
    fence_path = config.get("fence")
    if not fence_path or fence_gen is None:
        return None
    fence = read_json(fence_path)
    gen = int(fence.get("generation", 0))
    if gen > int(fence_gen):
        return f"fenced away by owner={fence.get('owner')} gen={gen}>{fence_gen}"
    return None


def _recover_logger():
    logger = logging.getLogger("awi-failover")
    if not logger.handlers:
        handler = logging.StreamHandler(sys.stderr)
        handler.setFormatter(logging.Formatter("%(asctime)s %(message)s"))
        logger.addHandler(handler)
        logger.setLevel(logging.INFO)
    return logger


def _load_failover_state(state_path):
    data = read_json(state_path)
    if not isinstance(data, dict):
        return {}
    return data.get("producers", {})


class _Reattach:
    """Stand-in for a Popen we did not launch (monitor restart reattach)."""

    def __init__(self, pid):
        self.pid = pid

    def poll(self):
        try:
            os.kill(self.pid, 0)
            return None
        except ProcessLookupError:
            return 0
        except PermissionError:
            return None

    def terminate(self):
        try:
            os.kill(self.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass


def monitor(config, stopping):
    """Aggregate producer heartbeats with an optional fenced failover loop."""
    log = _recover_logger()
    timeout_ms = config.get("timeout_seconds", 120) * 1000
    poll = config.get("poll_seconds", 10)
    auto_recover = config.get("auto_recover", False)
    grace_ms = config.get("auto_recover_grace_seconds", 240) * 1000
    drain_ms = config.get("auto_recover_drain_seconds", 15) * 1000
    state_dir = Path(config.get("failover_state_dir", str(config["status"]) + ".state"))
    state_dir.mkdir(parents=True, exist_ok=True)
    state_path = state_dir / "failover_state.json"
    master_owner = config.get("owner", "master:unknown")

    # Recover in-memory bookkeeping from persisted state so a monitor restart
    # neither double-launches a fallback nor orphans one (D4).
    persisted = _load_failover_state(state_path)
    unreachable_since = {}
    local_proc = {}
    for name, rec in list(persisted.items()):
        pid = rec.get("fallback_pid")
        alive = False
        if pid:
            try:
                os.kill(pid, 0)
                alive = True
            except (ProcessLookupError, PermissionError):
                alive = False
        if alive:
            local_proc[name] = _Reattach(pid)
            log.warning(
                "FAILOVER_REATTACH producer=%s fallback pid=%d gen=%s",
                name,
                pid,
                rec.get("gen"),
            )
        else:
            persisted.pop(name, None)
    atomic_json(state_path, {"producers": persisted})

    def persist():
        atomic_json(state_path, {"producers": dict(persisted)})

    while not stopping.is_set():
        now_ms = int(time.time() * 1000)
        workers = []
        for entry in config["producers"]:
            name = entry["name"]
            state = read_json(entry["status"])
            age = now_ms - state.get("observed_ms", 0)
            unreachable = age > timeout_ms
            eff_state = (
                "worker_or_supervisor_unreachable"
                if unreachable
                else state.get("state", "unknown")
            )
            rec = persisted.get(name)
            fence_path = entry.get("fence")
            current_gen = (
                int(read_fence(fence_path).get("generation", 0)) if fence_path else None
            )
            # Leadership moved away from our fallback if the shared fence was
            # claimed at a higher generation than the one we are running. This
            # lets us drain the fallback on a fence transition even before the
            # worker's shared status looks fresh, shrinking the handback overlap.
            leadership_lost = (
                rec is not None
                and rec.get("gen") is not None
                and current_gen is not None
                and current_gen > int(rec["gen"])
            )
            if unreachable and not leadership_lost:
                since = unreachable_since.get(name)
                if since is None:
                    unreachable_since[name] = now_ms
                    since = now_ms
                    log.warning(
                        "FAILOVER_ALERT producer=%s worker/supervisor unreachable",
                        name,
                    )
                proc = local_proc.get(name)
                alive = proc is not None and proc.poll() is None
                if alive:
                    # A local fallback is already carrying this shard; report it
                    # continuously (not only on the launch tick).
                    eff_state = "auto_recovering_local"
                past_grace = (now_ms - since) > grace_ms
                fallback_cmd = entry.get("fallback_command")
                if auto_recover and past_grace and not alive:
                    if not fallback_cmd:
                        log.warning(
                            "FAILOVER_SKIP producer=%s unreachable past grace "
                            "but no fallback_command configured",
                            name,
                        )
                    else:
                        gen = None
                        if fence_path:
                            gen = cas_bump_fence(
                                fence_path,
                                f"{master_owner}:fallback",
                                expect_gen=(rec or {}).get("gen"),
                            )
                            if gen is None:
                                # Fail closed: if we cannot atomically claim the
                                # fence (contention, or the lock volume is
                                # unresponsive), do NOT launch a second writer.
                                log.warning(
                                    "FAILOVER_CONTENTION producer=%s fence moved "
                                    "under us or lock unavailable; retry next tick",
                                    name,
                                )
                                continue
                        env = dict(os.environ)
                        fb_hb = state_dir / f"{name}.fallback.heartbeat.json"
                        if gen is not None:
                            env["AWI_FENCE_GEN"] = str(gen)
                            env["AWI_FENCE_OWNER"] = f"{master_owner}:fallback"
                        if fence_path:
                            env["AWI_FENCE_PATH"] = fence_path
                        # Isolated heartbeat namespace: the fallback writes its
                        # heartbeat here, NEVER to the worker's shared status, so
                        # its own heartbeat cannot be mistaken for worker recovery.
                        env["AWI_PROD_HEARTBEAT"] = str(fb_hb)
                        fb_status = state_dir / f"{name}.fallback.status.json"
                        env["AWI_FALLBACK_STATUS"] = str(fb_status)
                        log.warning(
                            "FAILOVER_RECOVER launching LOCAL fenced fallback "
                            "producer=%s gen=%s cmd=%s",
                            name,
                            gen,
                            fallback_cmd,
                        )
                        proc = subprocess.Popen(
                            fallback_cmd,
                            env=env,
                            stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL,
                            start_new_session=True,
                        )
                        local_proc[name] = proc
                        persisted[name] = {
                            "fallback_pid": proc.pid,
                            "gen": gen,
                            "since_ms": since,
                            "fallback_status": str(fb_status),
                        }
                        persist()
                        eff_state = "auto_recovering_local"
            else:
                # Recovery: worker shared status fresh, OR the fence generation
                # moved above our fallback's (leadership reclaimed). Either way
                # we must drain our local fallback so it stops writing.
                unreachable_since.pop(name, None)
                proc = local_proc.get(name)
                has_fallback = rec is not None or (
                    proc is not None and proc.poll() is None
                )
                if has_fallback:
                    why = (
                        f"fence gen {current_gen} > fallback gen {rec.get('gen')}"
                        if leadership_lost
                        else "worker shared status fresh"
                    )
                    log.warning(
                        "FAILOVER_RECOVERED producer=%s (%s); draining fallback",
                        name,
                        why,
                    )
                    target = proc.pid if proc is not None else None
                    if target is not None:
                        try:
                            os.killpg(target, signal.SIGTERM)
                        except (ProcessLookupError, PermissionError):
                            if proc is not None:
                                proc.terminate()
                        deadline = time.monotonic() + drain_ms / 1000.0
                        while time.monotonic() < deadline:
                            if proc.poll() is not None:
                                break
                            time.sleep(0.02)
                        else:
                            try:
                                os.killpg(target, signal.SIGKILL)
                            except (ProcessLookupError, PermissionError):
                                pass
                            if proc is not None:
                                try:
                                    proc.wait(timeout=5)
                                except subprocess.TimeoutExpired:
                                    pass
                    local_proc.pop(name, None)
                    persisted.pop(name, None)
                    persist()
            workers.append(
                {"name": name, "age_ms": age, "state": eff_state, "detail": state}
            )
        atomic_json(config["status"], {"observed_ms": now_ms, "producers": workers})
        stopping.wait(poll)


def supervise(config, stopping):
    log_path = Path(config["log"])
    log_path.parent.mkdir(parents=True, exist_ok=True)
    handler = logging.handlers.RotatingFileHandler(
        log_path, maxBytes=32 * 1024 * 1024, backupCount=3
    )
    handler.setFormatter(logging.Formatter("%(asctime)s %(message)s"))
    logger = logging.getLogger("awi-producer")
    logger.setLevel(logging.INFO)
    logger.addHandler(handler)
    restarts = 0
    backoff = 1
    fence_gen = config.get("fence_gen")
    fence_owner = config.get("fence_owner") or os.environ.get("AWI_FENCE_OWNER", "")
    fence_path = config.get("fence")
    # Primary claim: when the on-worker supervisor (re)starts it atomically
    # bumps the shared fence to a fresh generation and runs its child at that
    # generation. This is how a returning worker safely reclaims leadership:
    # its new generation is strictly higher than any fallback's, so the fallback
    # observes it and steps down (single-writer handback).
    if fence_path and config.get("fence_claim"):
        claimed = cas_bump_fence(fence_path, fence_owner or "worker:unknown")
        if claimed is not None:
            fence_gen = claimed
    environment = {**os.environ, **config.get("env", {})}
    # Pass the fence PATH too: without it the Rust publisher sees AWI_FENCE_PATH
    # unset and the commit guard is a no-op, so the PRIMARY writer would be
    # unguarded. GEN/OWNER alone are useless without the path.
    if fence_path:
        environment["AWI_FENCE_PATH"] = fence_path
    if fence_gen is not None:
        environment["AWI_FENCE_GEN"] = str(fence_gen)
    if fence_owner:
        environment["AWI_FENCE_OWNER"] = fence_owner
    while not stopping.is_set():
        child = subprocess.Popen(
            config["command"],
            env=environment,
            start_new_session=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
        )

        def drain(stream):
            for line in stream:
                logger.info("%s", line.rstrip())
            stream.close()

        reader = threading.Thread(target=drain, args=(child.stdout,), daemon=True)
        reader.start()
        logger.info(
            "started pid=%d restart=%d fence_gen=%s",
            child.pid,
            restarts,
            fence_gen,
        )
        started = time.monotonic()
        progress_at = started
        last_progress = None
        reason = "producer exited"
        try:
            while not stopping.wait(config.get("poll_seconds", 5)):
                heartbeat = read_json(config["heartbeat"])
                now = time.monotonic()
                progress = heartbeat.get("last_progress_ms")
                if heartbeat.get("pid") == child.pid:
                    if heartbeat.get("phase") == "idle" or progress != last_progress:
                        progress_at = now
                    last_progress = progress
                reason = health_reason(
                    heartbeat,
                    child.pid,
                    now - started,
                    now - progress_at,
                    config,
                    int(time.time() * 1000),
                )
                # Fencing: if another owner bumped the generation, fail closed
                # and stop the child immediately (single-writer guarantee).
                fenced = fence_lost(config, fence_gen, fence_owner)
                if fenced:
                    reason = fenced
                exited = child.poll()
                atomic_json(
                    config["status"],
                    {
                        "observed_ms": int(time.time() * 1000),
                        "supervisor_pid": os.getpid(),
                        "producer_pid": child.pid,
                        "restarts": restarts,
                        "fence_gen": fence_gen,
                        "fence_owner": fence_owner,
                        "state": "restarting"
                        if reason or exited is not None
                        else "running",
                        "reason": reason,
                        "exit_code": exited,
                        "heartbeat": heartbeat,
                    },
                )
                if reason or exited is not None:
                    break
            if time.monotonic() - started > 300:
                backoff = 1
        finally:
            stop_group(child)
            reader.join(timeout=3)
        if stopping.is_set():
            break
        # Do not auto-restart after being fenced away: we are no longer the
        # legitimate writer. Exit so the real owner takes over cleanly.
        if reason and reason.startswith("fenced away"):
            logger.warning("fenced away; exiting (no auto-restart): %s", reason)
            atomic_json(
                config["status"],
                {
                    "observed_ms": int(time.time() * 1000),
                    "state": "fenced",
                    "fence_gen": fence_gen,
                    "reason": reason,
                },
            )
            return
        restarts += 1
        logger.warning("restart: %s; backoff=%ds", reason or "process exited", backoff)
        stopping.wait(backoff)
        backoff = min(backoff * 2, 60)
    atomic_json(
        config["status"],
        {
            "observed_ms": int(time.time() * 1000),
            "state": "stopped",
            "restarts": restarts,
        },
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", type=Path)
    parser.add_argument("--monitor-only", action="store_true")
    args = parser.parse_args()
    config = json.loads(args.config.read_text())
    lock_path = Path(config["status"]).with_suffix(".lock")
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    stopping = threading.Event()
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, lambda *_: stopping.set())
    try:
        # The descriptor intentionally lives for the supervisor process lifetime.
        lease = open(lock_path, "a")  # noqa: SIM115
    except OSError as exc:
        raise SystemExit(f"cannot open supervisor lock {lock_path}: {exc}")
    try:
        try:
            fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as exc:  # BlockingIOError on lock held elsewhere
            raise SystemExit(
                f"another supervisor holds the lease on {lock_path} "
                f"(not starting, to avoid a double writer): {exc}"
            )
        if args.monitor_only:
            monitor(config, stopping)
        else:
            supervise(config, stopping)
    finally:
        lease.close()


if __name__ == "__main__":
    main()
