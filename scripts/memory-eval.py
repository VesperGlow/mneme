#!/usr/bin/env python3
"""Disposable memory evaluation. Never loads .env or calls a chat provider.

Requires existing local images: --pull=never, offline model files, loopback-only
ports, temporary SQLite files and container-owned ephemeral Neo4j data.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.request
import uuid
import base64
import hashlib

ROOT = Path(__file__).resolve().parents[1]
PASSWORD = "mneme-memory-eval-only"
GRAPH = "http://127.0.0.1:17474/db/neo4j/tx/commit"


def command(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, cwd=ROOT, **kwargs)


def http(url, body=None, auth=False):
    headers = {"Content-Type": "application/json"}
    if auth:
        headers["Authorization"] = "Basic " + base64.b64encode(f"neo4j:{PASSWORD}".encode()).decode()
    request = urllib.request.Request(url, data=None if body is None else json.dumps(body).encode(), headers=headers)
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.load(response)


def free_port(port):
    with socket.socket() as sock:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            sock.bind(("127.0.0.1", port))
        except OSError:
            raise SystemExit(f"Refusing to use occupied port {port}; existing services are never stopped.") from None


def ready(url, process_name, seconds=120):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            return http(url)
        except (OSError, urllib.error.URLError, json.JSONDecodeError):
            running = command("podman", "inspect", "--format", "{{.State.Running}}", process_name, capture_output=True).stdout.strip()
            if running != "true":
                command("podman", "logs", "--tail", "30", process_name)
                raise SystemExit(f"Disposable service failed to start: {process_name}")
            time.sleep(1)
    raise SystemExit(f"Disposable service startup timed out: {process_name}")


def monitor_run(args, env, output, containers):
    with output.with_suffix(".log").open("w") as log:
        process = subprocess.Popen(args, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
        samples = []
        while process.poll() is None:
            mem = Path("/proc/meminfo").read_text()
            sample = {"at": time.time(), "host_memory": [line for line in mem.splitlines() if line.startswith(("MemAvailable:", "SwapFree:"))]}
            if containers:
                result = subprocess.run(["podman", "stats", "--no-stream", "--format", "json", *containers], text=True, capture_output=True, timeout=10)
                try:
                    sample["containers"] = json.loads(result.stdout)
                except json.JSONDecodeError:
                    sample["monitor_error"] = result.stderr[-500:]
                if any(name.endswith("-models") for name in containers):
                    try:
                        sample["model_health"] = http("http://127.0.0.1:18090/health")
                    except (OSError, urllib.error.URLError):
                        pass
            samples.append(sample)
            time.sleep(1)
        output.with_suffix(".resources.json").write_text(json.dumps(samples, indent=2))
        if process.returncode:
            raise SystemExit(f"Evaluation failed; inspect {output.with_suffix('.log')}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("load", "semantic", "all"), default="load")
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts" / ("memory-eval-" + time.strftime("%Y%m%d-%H%M%S")))
    parser.add_argument("--neo4j-image", default="docker.io/library/neo4j:5.26.32-community-trixie")
    parser.add_argument("--models-image", default="localhost/mneme-models:bundled-qa")
    parser.add_argument("--model-cache-volume", help="Optional EXISTING pinned model cache, mounted read-only; never downloaded or modified")
    parser.add_argument("--cpu-dtype", choices=("float32","bfloat16"), default="float32")
    parser.add_argument("--rerank-dtype", choices=("float32","bfloat16"), default="float32")
    parser.add_argument("--real-model-load", action="store_true", help="Also measure real models at 1/5/20/50 concurrency, bounded by their existing queue")
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    if shutil.disk_usage(ROOT).free < 1024**3:
        raise SystemExit("Less than 1 GiB free disk; no evaluation started.")
    available = int(next(line.split()[1] for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemAvailable:")))
    required = 1600000 if args.mode == "load" else 3500000
    if available < required:
        raise SystemExit(f"Only {available} KiB available RAM; this mode requires {required} KiB.")
    free_port(17474)
    if args.mode != "load":
        free_port(18090)
    if args.model_cache_volume:
        command("podman", "volume", "exists", args.model_cache_volume)
    run = "eval-" + uuid.uuid4().hex[:12]
    containers = []
    env = {**os.environ, "MNEME_EVAL_RUN": run, "CARGO_INCREMENTAL": "0",
           "CARGO_PROFILE_DEV_DEBUG":"0", "CARGO_PROFILE_TEST_DEBUG":"0"}
    metadata = {"run": run, "mode": args.mode, "git": command("git", "rev-parse", "HEAD", capture_output=True).stdout.strip(),
                "rustc": command("rustc", "--version", capture_output=True).stdout.strip(), "cpus": os.cpu_count(),
                "memory_available_kib": available, "real_model_load": args.real_model_load,
                "cpu_dtype": args.cpu_dtype, "rerank_dtype":args.rerank_dtype, "models": "offline local" if args.mode != "load" else "deterministic HTTP fixture",
                "llm": "never called", "sqlite": "TempDir per scale; real user data never opened"}
    metadata["source_sha256"] = hashlib.sha256(b"".join(p.read_bytes() for p in sorted((ROOT / "src").glob("*.rs")))).hexdigest()
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2))
    try:
        name = "mneme-memory-" + run + "-neo4j"
        command("podman", "run", "-d", "--pull=never", "--name", name, "--label", "mneme.memory-eval=" + run,
                "--memory=1536m", "--cpus=2", "-p", "127.0.0.1:17474:7474", "--tmpfs", "/data:rw,size=512m",
                "-e", "NEO4J_AUTH=neo4j/" + PASSWORD, "-e", "NEO4J_server_memory_heap_initial__size=256m",
                "-e", "NEO4J_server_memory_heap_max__size=512m", "-e", "NEO4J_server_memory_pagecache_size=64m",
                "-e", "NEO4J_db_tx__log_preallocate=false", "-e", "NEO4J_db_tx__log_rotation_size=16M",
                "-e", "NEO4J_db_tx__log_rotation_retention__policy=2 files", "-e", "NEO4J_server_bolt_enabled=false",
                "-e", "NEO4J_dbms_usage__report_enabled=false", args.neo4j_image, capture_output=True)
        containers.append(name)
        ready("http://127.0.0.1:17474/", name)
        result = http(GRAPH, {"statements": [{"statement": "CREATE (:MnemeEvaluation {id:$id})", "parameters": {"id": run}}]}, auth=True)
        if result["errors"]:
            raise SystemExit("Cannot install disposable database marker")
        # Compile before collecting latency or resource samples.
        for suite in ("memory_load", "memory_semantics"):
            command("cargo", "test", "--locked", "--test", suite, "--no-run", env=env)
        if args.mode in ("load", "all"):
            for test, label in (("isolated_write_matrix", "writes"), ("isolated_load_matrix", "retrieval")):
                print(f"Running progressive {label} matrix (100/1000/10000 x 1/5/20/50)", flush=True)
                path = args.output / (label + ".json")
                env["MNEME_EVAL_OUTPUT"] = str(path)
                monitor_run(["cargo", "test", "--locked", "--test", "memory_load", test, "--", "--ignored", "--nocapture"], env, path, containers)
        if args.mode in ("semantic", "all"):
            model_name = "mneme-memory-" + run + "-models"
            cmd = ["podman", "run", "-d", "--pull=never", "--name", model_name, "--label", "mneme.memory-eval=" + run,
                   "--memory=3g", "--cpus=3", "-p", "127.0.0.1:18090:8090", "-e", "HF_HUB_OFFLINE=1", "-e", "MNEME_CPU_THREADS=2", "-e", "MNEME_CPU_DTYPE=" + args.cpu_dtype,
                   "-e", "MNEME_CPU_RERANK_DTYPE="+args.rerank_dtype,
                   "-v", str(ROOT / "services/models") + ":/app:ro"]
            if args.model_cache_volume:
                cmd += ["-v", args.model_cache_volume + ":/models:ro", "-e", "MNEME_BUNDLED_MODEL_CACHE=/models/hub"]
            command(*cmd, args.models_image, "uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8090", "--workers", "1", "--no-access-log", capture_output=True)
            containers.append(model_name)
            metadata["model_health"] = ready("http://127.0.0.1:18090/health", model_name, 180)
            (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2))
            path = args.output / "semantics.json"
            env["MNEME_EVAL_OUTPUT"] = str(path)
            env["MNEME_EVAL_REAL_LOAD"] = "1" if args.real_model_load else "0"
            monitor_run(["cargo", "test", "--locked", "--test", "memory_semantics", "--", "--ignored", "--nocapture"], env, path, containers)
            metadata["model_health_after"] = http("http://127.0.0.1:18090/health")
            (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2))
            env["MNEME_EVAL_DATASET"] = "heldout"
            env["MNEME_EVAL_REAL_LOAD"] = "0"
            path = args.output / "heldout.json"
            env["MNEME_EVAL_OUTPUT"] = str(path)
            monitor_run(["cargo", "test", "--locked", "--test", "memory_semantics", "--", "--ignored", "--nocapture"], env, path, containers)
            command("podman", "cp", str(ROOT / "tests/test_model_queue.py"), model_name + ":/tmp/test_model_queue.py")
            with (args.output / "queue-tests.log").open("w") as log:
                command("podman", "exec", "-e", "MNEME_MODEL_APP=/app/app.py", model_name, "python", "/tmp/test_model_queue.py", "-v", stdout=log, stderr=log)
    finally:
        for name in reversed(containers):
            with (args.output / (name + ".log")).open("w") as log:
                subprocess.run(["podman", "logs", "--tail", "100", name], stdout=log, stderr=log, check=False)
            # Only names created successfully by this invocation are eligible.
            subprocess.run(["podman", "rm", "--force", name], stdout=subprocess.DEVNULL, check=False)
    print(f"Results: {args.output}")


if __name__ == "__main__":
    main()
