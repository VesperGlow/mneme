#!/usr/bin/env python3
"""Real causal/embedding/reranker evaluation in disposable local services.

No .env or production database is used. Default generation is offline. Explicit
--deepseek-relay requires a separately authorized, budget-limited local relay.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tarfile
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("isolated_eval", ROOT / "scripts/memory-eval.py")
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
RUNTIME = ROOT / "artifacts/memory-e2e-runtime"
FILES = [
    ("llama-b11669-bin-ubuntu-x64.tar.gz", "https://github.com/ggml-org/llama.cpp/releases/download/b11669/llama-b11669-bin-ubuntu-x64.tar.gz", "cf07c1c112d709bd67e522017471de57ed0ec8bea031d732c5422351c07f65ac"),
    ("Qwen3-0.6B-Q8_0.gguf", "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/23749fefcc72300e3a2ad315e1317431b06b590a/Qwen3-0.6B-Q8_0.gguf", "9465e63a22add5354d9bb4b99e90117043c7124007664907259bd16d043bb031"),
]

MODELS = {
    "qwen3-0.6b": FILES[1],
    "qwen3-4b": ("Qwen3-4B-Q4_K_M.gguf", "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/bc640142c66e1fdd12af0bd68f40445458f3869b/Qwen3-4B-Q4_K_M.gguf", "7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5"),
    "qwen2.5-1.5b": ("qwen2.5-1.5b-instruct-q4_k_m.gguf", "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/91cad51170dc346986eccefdc2dd33a9da36ead9/qwen2.5-1.5b-instruct-q4_k_m.gguf", "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e")
}

def prepare(download, model):
    RUNTIME.mkdir(parents=True, exist_ok=True)
    for name, url, digest in (FILES[0], MODELS[model]):
        path = RUNTIME / name
        if not path.exists():
            if not download:
                raise SystemExit("Missing pinned runtime; use --download-free-model only with authorization.")
            with urllib.request.urlopen(url, timeout=120) as src, path.with_suffix(".partial").open("wb") as dest:
                import shutil
                shutil.copyfileobj(src, dest)
            path.with_suffix(".partial").replace(path)
        with path.open("rb") as f:
            actual = hashlib.file_digest(f, "sha256").hexdigest()
        if actual != digest:
            raise SystemExit(f"SHA256 mismatch: {path}")
    binary = RUNTIME / "llama/llama-b11669/llama-server"
    if not binary.exists():
        with tarfile.open(RUNTIME / FILES[0][0]) as archive:
            archive.extractall(RUNTIME / "llama", filter="data")
    return binary


def build_inputs():
    paths = [ROOT/'Cargo.toml',ROOT/'Cargo.lock',ROOT/'config/default.toml',ROOT/'tests/memory_e2e.rs']
    paths += sorted((ROOT/'src').glob('*.rs'))
    paths += sorted((ROOT/'services/models').glob('*.py'))
    paths += sorted((ROOT/'tests/fixtures/memory').glob('*.json'))
    paths += sorted((ROOT/'migrations').glob('*.sql'))
    paths += [ROOT/'scripts/memory-e2e-report.py']
    return {str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}


def verify_frozen_fixtures(directory):
    """Verify author-frozen holdouts without exposing their gold to the agent."""
    verified = {}
    manifests = sorted(Path(directory).glob('e2e-blind*.manifest.json'))
    if not manifests:
        raise ValueError('No frozen holdout manifests found')
    for path in manifests:
        manifest = json.loads(path.read_text())
        dataset = path.with_name(path.name.replace('.manifest.json', '.json'))
        digest = hashlib.sha256(dataset.read_bytes()).hexdigest()
        if digest != manifest['sha256']:
            raise ValueError(f'Frozen holdout changed: {dataset.name}')
        profiles = json.loads(dataset.read_text())['profiles']
        turns = sum(len(p['turns']) for p in profiles)
        queries = sum(len(p['queries']) for p in profiles)
        if turns != manifest['turns'] or queries != manifest['queries']:
            raise ValueError(f'Frozen holdout coverage mismatch: {dataset.name}')
        verified[dataset.name] = {'sha256': digest, 'turns': turns, 'queries': queries}
    return verified


def main():
    def interrupted(signum, frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--download-free-model", action="store_true")
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--require-quality", action="store_true", help="Return failure when measured gold quality thresholds are missed")
    parser.add_argument("--verify-fixtures-only", action="store_true", help="Verify frozen holdout hashes and coverage without models, containers or API calls")
    parser.add_argument("--generation-model", choices=MODELS, default="qwen3-0.6b")
    parser.add_argument("--deepseek-relay", action="store_true", help="Use only the owned official-provider test relay, with its shared budget")
    parser.add_argument("--split", choices=("dev", "heldout", "unseen", "confirm", "blind", "blind2", "blind3", "blind4", "blind5", "combined", "core", "quality", "full", "all"), default="all")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--models-memory", default="4g")
    parser.add_argument("--models-source", type=Path, default=ROOT/'services/models', help="Read-only model service source for a checksum-recorded baseline comparison")
    parser.add_argument("--cpu-dtype", choices=("float32","bfloat16"), default="float32")
    parser.add_argument("--rerank-dtype", choices=("float32","bfloat16"), default="float32")
    parser.add_argument("--frozen-test-binary", type=Path, help="Optional checksum-verified pre-fix evaluator; never a production binary")
    parser.add_argument("--test", default="real_local_e2e", choices=("real_local_e2e", "real_local_api", "real_local_load", "real_local_batch", "real_local_unknown", "real_recall_replay"))
    args = parser.parse_args()
    frozen_fixtures = verify_frozen_fixtures(ROOT/'tests/fixtures/memory')
    if args.verify_fixtures_only:
        args.output.mkdir(parents=True, exist_ok=True)
        (args.output/'fixture-integrity.json').write_text(json.dumps({'status':'integrity_verified_only','quality_evaluated':False,'fixtures':frozen_fixtures},indent=2)+'\n')
        print('Frozen holdout hashes and coverage verified; no inference or quality measurement performed.')
        return
    args.models_source=args.models_source.resolve()
    if not all((args.models_source/name).is_file() for name in ('app.py','engine.py','artifacts.py')):
        parser.error('model source must contain app.py, engine.py and artifacts.py')
    if args.split in ('unseen','confirm') and args.test!='real_local_e2e':
        parser.error('novel-topic splits are separate post-fix quality suites')
    binary = None if args.deepseek_relay else prepare(args.download_free_model, args.generation_model)
    if args.prepare_only:
        return
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    for port in ((17474, 18090) if args.deepseek_relay else (17474, 18090, 18091)):
        base.free_port(port)
    relay = None
    if args.deepseek_relay:
        relay = base.http('http://127.0.0.1:18091/health')
        expected = str(ROOT/'scripts/memory-deepseek-relay.py').encode()
        commandline = Path(f"/proc/{relay['pid']}/cmdline").read_bytes().split(b'\0')
        if relay['provider'] != 'official-deepseek-test-relay' or relay['model'] != 'deepseek-flash' or not any(arg == expected or arg == b'scripts/memory-deepseek-relay.py' for arg in commandline):
            raise SystemExit('Refusing an unowned generation service')
    available = int(next(l.split()[1] for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')))
    if available < 4800000:
        raise SystemExit(f"Need 4.8 GB available RAM; found {available} KiB")
    run = "e2e-" + uuid.uuid4().hex[:12]
    env = {**os.environ, "MNEME_EVAL_RUN": run, "MNEME_EVAL_OUTPUT": str(out / "results.json"),
           "MNEME_E2E_SPLIT": args.split,
           "MNEME_E2E_SCHEMA":'off' if args.deepseek_relay else 'on', "CARGO_INCREMENTAL": "0",
           "CARGO_PROFILE_DEV_DEBUG":"0", "CARGO_PROFILE_TEST_DEBUG":"0"}
    inputs = build_inputs()
    frozen = None
    if args.frozen_test_binary:
        args.frozen_test_binary = args.frozen_test_binary.resolve()
        frozen = json.loads(Path(str(args.frozen_test_binary)+'.json').read_text())
        with args.frozen_test_binary.open('rb') as f:
            assert hashlib.file_digest(f,'sha256').hexdigest()==frozen['sha256'], 'frozen evaluator SHA256 mismatch'
        assert hashlib.sha256((ROOT/'tests/fixtures/memory/e2e.json').read_bytes()).hexdigest()==frozen['gold_sha256'], 'gold changed since baseline'
    else:
        base.command("cargo", "test", "--locked", "--test", "memory_e2e", "--no-run", env=env)
        if build_inputs()!=inputs:
            raise SystemExit('Evaluation inputs changed during compilation; run again with a stable source tree')
    containers, generation = [], None
    meta = {"run": run, "split": args.split, "runtime": [FILES[0], MODELS[args.generation_model]], "cpus": os.cpu_count(), "available_kib": available,
            "frozen_fixtures": frozen_fixtures,
            "source_sha256": hashlib.sha256(b''.join(p.read_bytes() for p in sorted((ROOT/'src').glob('*.rs')))).hexdigest(),
            "gold_sha256": hashlib.sha256((ROOT/'tests/fixtures/memory/e2e.json').read_bytes()).hexdigest(),
            "unseen_gold_sha256": hashlib.sha256((ROOT/'tests/fixtures/memory/e2e-unseen.json').read_bytes()).hexdigest(),
            "generation": args.generation_model+"; offline CPU; thinking disabled; 16384 context; 1 slot; 2 threads; seed42",
            "extraction": "actual memory::worker; no answer fixtures", "sqlite": "TempDir per profile", "api_cost": 0, "build_inputs": inputs, "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    if frozen:
        meta['frozen_binary']=frozen
        meta['source_sha256']=frozen['source_sha256']
    if relay:
        meta['runtime']=[]
        meta['generation']='deepseek-flash; official API; thinking disabled; temperature 0; bounded output'
        meta['budget_before']=relay
        meta['api_cost']='See shared peak-price upper-bound budget ledger'
    meta['json_schema']=not bool(frozen) and not bool(relay)
    meta['prejudge']=False
    meta['cpu_dtype']=args.cpu_dtype
    meta['rerank_dtype']=args.rerank_dtype
    meta['model_service_source']={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in args.models_source.glob('*.py')}
    try:
        glog = (out / "generation.log").open("w")
        if not relay:
            generation = subprocess.Popen([str(binary), "-m", str(RUNTIME/MODELS[args.generation_model][0]), "--host", "127.0.0.1", "--port", "18091",
            "--alias", "mneme-eval-local,mneme-eval-qwen3-0.6b", "-c", "16384", "-np", "1", "-t", "2", "-tb", "2", "-ngl", "0",
            "--cache-ram", "0", "--batch-size", "256", "--ubatch-size", "128", "--metrics", "--no-webui", "--seed", "42",
            "--chat-template-kwargs", '{"enable_thinking":false}', "--reasoning", "off"], stdout=glog, stderr=glog, start_new_session=True)
        for _ in range(90):
            if generation is not None and generation.poll() is not None:
                raise SystemExit("Generation startup failed; inspect generation.log")
            try:
                base.http("http://127.0.0.1:18091/health"); break
            except OSError:
                time.sleep(1)
        else:
            raise SystemExit("Generation startup timed out")
        neo = "mneme-memory-"+run+"-neo4j"
        base.command("podman", "run", "-d", "--pull=never", "--name", neo, "--label", "mneme.memory-eval="+run,
            "--memory=1536m", "--cpus=2", "-p", "127.0.0.1:17474:7474", "--tmpfs", "/data:rw,size=512m",
            "-e", "NEO4J_AUTH=neo4j/"+base.PASSWORD, "-e", "NEO4J_server_memory_heap_initial__size=256m",
            "-e", "NEO4J_server_memory_heap_max__size=512m", "-e", "NEO4J_server_memory_pagecache_size=64m",
            "-e", "NEO4J_db_tx__log_preallocate=false", "-e", "NEO4J_db_tx__log_rotation_size=16M",
            "-e", "NEO4J_db_tx__log_rotation_retention__policy=2 files", "-e", "NEO4J_server_bolt_enabled=false",
            "-e", "NEO4J_dbms_usage__report_enabled=false", "docker.io/library/neo4j:5.26.32-community-trixie", capture_output=True)
        containers.append(neo)
        base.ready("http://127.0.0.1:17474/", neo)
        assert not base.http(base.GRAPH, {"statements":[{"statement":"CREATE (:MnemeEvaluation {id:$id})", "parameters":{"id":run}}]}, auth=True)['errors']
        models = "mneme-memory-"+run+"-models"
        base.command("podman", "run", "-d", "--pull=never", "--name", models, "--label", "mneme.memory-eval="+run,
            "--memory="+args.models_memory, "--cpus=3", "-p", "127.0.0.1:18090:8090", "-e", "HF_HUB_OFFLINE=1",
            "-e", "MNEME_CPU_THREADS=2", "-e", "MNEME_CPU_DTYPE="+args.cpu_dtype,
            "-e", "MNEME_CPU_RERANK_DTYPE="+args.rerank_dtype, "-e", "MNEME_BUNDLED_MODEL_CACHE=/models/hub",
            "-v", str(args.models_source)+":/app:ro", "-v", "mneme-model-cache:/models:ro",
            "localhost/mneme-models:bundled-qa", "uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8090", "--workers", "1", "--no-access-log", capture_output=True)
        containers.append(models)
        meta['models'] = base.ready("http://127.0.0.1:18090/health", models, 180)
        (out/'metadata.json').write_text(json.dumps(meta,indent=2))
        samples=[]
        with (out/'test.log').open('w') as log:
            test_command=([str(args.frozen_test_binary),args.test,'--ignored','--nocapture'] if frozen else ['cargo','test','--locked','--test','memory_e2e',args.test,'--','--ignored','--nocapture'])
            proc=subprocess.Popen(test_command,cwd=ROOT,env=env,stdout=log,stderr=subprocess.STDOUT)
            try:
                while proc.poll() is None:
                    sample={'at':time.time(),'host':[l for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith(('MemAvailable:','SwapFree:'))]}
                    try:
                        generation_pid=relay['pid'] if relay else generation.pid
                        sample['generation_status']=[l for l in Path(f'/proc/{generation_pid}/status').read_text().splitlines() if l.startswith(('VmRSS:','VmHWM:','VmSwap:','Threads:'))]
                        sample['generation_stat']=Path(f'/proc/{generation_pid}/stat').read_text()
                        sample['containers']=json.loads(subprocess.run(['podman','stats','--no-stream','--format','json',*containers],capture_output=True,text=True,timeout=10).stdout)
                        sample['models']=base.http('http://127.0.0.1:18090/health')
                    except (OSError,ValueError,subprocess.TimeoutExpired) as error:
                        sample['monitor_error']=type(error).__name__
                    samples.append(sample)
                    # Guard against sustained global memory exhaustion; never OOM a host for an eval.
                    avail=int(next(l.split()[1] for l in sample['host'] if l.startswith('MemAvailable:')))
                    if avail<350000:
                        proc.terminate();raise SystemExit('Aborted: host available memory below 350 MiB')
                    time.sleep(1)
            finally:
                if proc.poll() is None:
                    proc.terminate();proc.wait(timeout=30)
                (out/'resources.json').write_text(json.dumps(samples,indent=2))
            if proc.returncode:
                raise SystemExit(f'Evaluation failed: {out}/test.log')
        meta['models_after']=base.http('http://127.0.0.1:18090/health')
        if relay:
            meta['budget_after']=base.http('http://127.0.0.1:18091/health')
        with urllib.request.urlopen('http://127.0.0.1:18091/metrics') as r:(out/'generation.metrics').write_bytes(r.read())
        (out/'metadata.json').write_text(json.dumps(meta,indent=2))
        if args.test=='real_local_e2e':
            report_spec=importlib.util.spec_from_file_location('e2e_report',ROOT/'scripts/memory-e2e-report.py')
            report=importlib.util.module_from_spec(report_spec);report_spec.loader.exec_module(report)
            summary=report.summarize(out)
            (out/'summary.json').write_text(json.dumps(summary,ensure_ascii=False,indent=2))
            print(json.dumps({k:summary[k] for k in ('writes','retrieval','answers','quality_gate')},ensure_ascii=False),flush=True)
            if (args.require_quality or args.split.startswith('blind')) and not summary['quality_gate']['passed']:
                raise SystemExit('Measured quality gate failed; inspect summary.json and raw cases')
    finally:
        for name in reversed(containers):
            with (out/(name+'.log')).open('w') as log:
                subprocess.run(['podman','logs','--tail','100',name],stdout=log,stderr=log)
            subprocess.run(['podman','rm','--force',name],stdout=subprocess.DEVNULL)
        if generation is not None:
            if generation.poll() is None:
                os.killpg(generation.pid, signal.SIGTERM)
                try:
                    generation.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(generation.pid, signal.SIGKILL)
                    generation.wait(timeout=10)
            glog.close()
    print(out)

if __name__=='__main__':main()
