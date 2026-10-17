"""Verified, resumable downloads with Hugging Face / ModelScope failover."""
import fcntl
import hashlib
import json
import logging
import os
from pathlib import Path
import re
import time
from urllib.parse import quote, urlsplit

import requests


MANIFEST = json.loads(Path(__file__).with_name("artifacts.json").read_text())
EMBED_MODEL, EMBED_REVISION = MANIFEST[0]["repo"], MANIFEST[0]["revision"]
RERANK_MODEL, RERANK_REVISION = MANIFEST[1]["repo"], MANIFEST[1]["revision"]
CODE_REVISION = MANIFEST[2]["revision"]
LOG = logging.getLogger("uvicorn.error")
CHUNK_SIZE = 64 * 1024
TIMEOUT = (5, 15)  # No library backoff or Xet retry loop before trying the next source.


class ArtifactError(Exception):
    """A fixed diagnostic category, never an upstream URL or response body."""


def cache_path():
    home = Path(os.environ.get("HF_HOME", "~/.cache/huggingface")).expanduser()
    return Path(os.environ.get("HF_HUB_CACHE") or
                os.environ.get("HUGGINGFACE_HUB_CACHE") or home / "hub").expanduser()


def offline_mode():
    return os.environ.get("HF_HUB_OFFLINE", "").upper() in {"1", "ON", "YES", "TRUE"}


def verified(path, artifact):
    if not path.is_file() or path.stat().st_size != artifact["size"]:
        return False
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest() == artifact["sha256"]


def failure_category(error):
    if isinstance(error, ArtifactError):
        return str(error)
    if isinstance(error, requests.exceptions.SSLError):
        return "tls"
    if isinstance(error, requests.Timeout):
        return "timeout"
    if isinstance(error, requests.HTTPError):
        return f"http_{error.response.status_code}"
    if isinstance(error, requests.ConnectionError):
        return "connection"
    return "network"


class Downloader:
    def __init__(self, cache, sources, endpoints=None):
        self.cache = Path(cache)
        self.sources = list(sources)
        self.endpoints = endpoints or {
            "huggingface": os.environ.get("HF_ENDPOINT", "https://huggingface.co").rstrip("/"),
            "modelscope": "https://modelscope.cn/models",
        }
        self.session = requests.Session()
        self.session.max_redirects = 5

    def close(self):
        self.session.close()

    def url(self, source, spec, name, artifact):
        repo = spec["repo"] if source == "huggingface" else spec["modelscope_repo"]
        revision = spec["revision"] if source == "huggingface" else artifact["modelscope_revision"]
        return f"{self.endpoints[source]}/{repo}/resolve/{revision}/{quote(name, safe='/')}"

    def transfer(self, url, partial, artifact, source):
        offset = partial.stat().st_size if partial.exists() else 0
        if offset >= artifact["size"]:
            partial.unlink()
            offset = 0
        headers = {"Accept-Encoding": "identity", "User-Agent": "mneme-models/0.1"}
        if offset:
            headers["Range"] = f"bytes={offset}-"
        with self.session.get(url, headers=headers, stream=True, timeout=TIMEOUT) as response:
            response.raise_for_status()
            if response.status_code == 206:
                match = re.fullmatch(r"bytes (\d+)-(\d+)/(\d+)", response.headers.get("Content-Range", ""))
                if (not match or int(match[1]) != offset or int(match[2]) < offset
                        or int(match[2]) >= artifact["size"] or int(match[3]) != artifact["size"]):
                    raise ArtifactError("invalid_range")
            elif response.status_code == 200:
                offset = 0  # Servers ignoring Range must restart, rather than append.
            else:
                raise ArtifactError("unexpected_status")
            LOG.info("model download connected source=%s host=%s resume_bytes=%d total_bytes=%d",
                     source, urlsplit(response.url).hostname, offset, artifact["size"])
            last_report = time.monotonic()
            with partial.open("ab" if offset else "wb") as handle:
                for chunk in response.iter_content(CHUNK_SIZE):
                    if offset + len(chunk) > artifact["size"]:
                        raise ArtifactError("oversized_body")
                    handle.write(chunk)
                    offset += len(chunk)
                    now = time.monotonic()
                    if now - last_report >= 10:
                        LOG.info("model download progress source=%s bytes=%d total_bytes=%d percent=%.1f",
                                 source, offset, artifact["size"], offset * 100 / artifact["size"])
                        last_report = now
                handle.flush()
                os.fsync(handle.fileno())
        if offset != artifact["size"]:
            raise ArtifactError("incomplete_body")

    def publish(self, blob, target):
        target.parent.mkdir(parents=True, exist_ok=True)
        link = target.with_name(target.name + ".mneme-link")
        link.unlink(missing_ok=True)
        link.symlink_to(os.path.relpath(blob, target.parent))
        os.replace(link, target)

    def ensure(self, spec, name, artifact, offline=False):
        repo = self.cache / ("models--" + spec["repo"].replace("/", "--"))
        target = repo / "snapshots" / spec["revision"] / name
        blob = repo / "blobs" / artifact["sha256"]
        if verified(target, artifact):
            LOG.info("model cache verified model=%s file=%s", spec["repo"], name)
            return target
        if verified(blob, artifact):
            self.publish(blob, target)
            return target
        if offline:
            raise RuntimeError(f"pinned model cache missing or corrupt in offline mode: {spec['repo']}/{name}")
        blob.parent.mkdir(parents=True, exist_ok=True)
        # Partials stay separate by source; a broken source cannot poison the
        # next one's prefix. Hugging Face's own .incomplete files are untouched.
        for source in tuple(self.sources):
            partial = blob.with_name(blob.name + f".{source}.part")
            started = time.monotonic()
            LOG.info("model download starting model=%s file=%s source=%s", spec["repo"], name, source)
            try:
                if not verified(partial, artifact):
                    self.transfer(self.url(source, spec, name, artifact), partial, artifact, source)
                if not verified(partial, artifact):
                    partial.unlink(missing_ok=True)
                    raise ArtifactError("checksum_mismatch")
                os.replace(partial, blob)
                self.publish(blob, target)
                LOG.info("model download verified model=%s file=%s source=%s", spec["repo"], name, source)
                # Prefer the working source for the rest of this startup, so a
                # failed CDN is not retried for every subsequent artifact.
                self.sources.remove(source)
                self.sources.insert(0, source)
                return target
            except (requests.RequestException, ArtifactError) as error:
                LOG.warning("model download failed model=%s file=%s source=%s category=%s elapsed_seconds=%.1f",
                            spec["repo"], name, source, failure_category(error), time.monotonic() - started)
        raise RuntimeError(f"all download sources failed for pinned artifact: {spec['repo']}/{name}")


def prepare_cache():
    sources = [source.strip() for source in os.environ.get(
        "MNEME_MODEL_SOURCES", "huggingface,modelscope").split(",")]
    if not sources or len(sources) != len(set(sources)) or any(
            source not in {"huggingface", "modelscope"} for source in sources):
        raise RuntimeError("MNEME_MODEL_SOURCES must list huggingface and/or modelscope without duplicates")
    cache = cache_path()
    cache.mkdir(parents=True, exist_ok=True)
    LOG.info("preparing pinned model cache sources=%s offline=%s", ",".join(sources), offline_mode())
    with (cache / ".mneme-download.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        downloader = Downloader(cache, sources)
        try:
            for spec in MANIFEST:
                for name, artifact in spec["files"].items():
                    downloader.ensure(spec, name, artifact, offline=offline_mode())
        finally:
            downloader.close()
    return cache
