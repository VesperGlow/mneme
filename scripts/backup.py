#!/usr/bin/env python3
"""Consistent SQLite backup, including committed WAL data. Run inside podman unshare."""
import argparse
import os
from pathlib import Path
import sqlite3

parser = argparse.ArgumentParser()
parser.add_argument("database", type=Path)
parser.add_argument("destination", type=Path)
args = parser.parse_args()
os.umask(0o077)
if not args.database.is_file():
    raise SystemExit("源数据库不存在")
if args.destination.exists():
    raise SystemExit("目标已存在，拒绝覆盖")
args.destination.parent.mkdir(parents=True, exist_ok=True)
with sqlite3.connect(f"{args.database.resolve().as_uri()}?mode=ro", uri=True) as source:
    with sqlite3.connect(args.destination) as dest:
        source.backup(dest)
        if dest.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
            raise SystemExit("备份完整性检查失败")
print(args.destination)
