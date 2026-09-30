#!/usr/bin/env python3
"""Supervise one owned AWI producer; no third-party dependencies.

JSON config: command (argv list), heartbeat, log, status, startup_seconds (1800),
heartbeat_seconds (120), progress_seconds (900), poll_seconds (5).
An optional env object adds non-secret environment settings.
--monitor-only reads heartbeat/status paths from a JSON list and writes fleet
health. Run this on a separate host to detect loss of the producer's worker.
"""

import argparse
import fcntl
import json
import logging
import logging.handlers
import os
import signal
import subprocess
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


def monitor(config, stopping):
    while not stopping.is_set():
        now_ms = int(time.time() * 1000)
        workers = []
        for entry in config["producers"]:
            state = read_json(entry["status"])
            age = now_ms - state.get("observed_ms", 0)
            workers.append(
                {
                    "name": entry["name"],
                    "age_ms": age,
                    "state": (
                        "worker_or_supervisor_unreachable"
                        if age > config.get("timeout_seconds", 120) * 1000
                        else state.get("state", "unknown")
                    ),
                    "detail": state,
                }
            )
        atomic_json(config["status"], {"observed_ms": now_ms, "producers": workers})
        stopping.wait(config.get("poll_seconds", 10))


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
    environment = {**os.environ, **config.get("env", {})}
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
        logger.info("started pid=%d restart=%d", child.pid, restarts)
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
                exited = child.poll()
                atomic_json(
                    config["status"],
                    {
                        "observed_ms": int(time.time() * 1000),
                        "supervisor_pid": os.getpid(),
                        "producer_pid": child.pid,
                        "restarts": restarts,
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
    with lock_path.open("a") as lease:
        fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.monitor_only:
            monitor(config, stopping)
        else:
            supervise(config, stopping)


if __name__ == "__main__":
    main()
