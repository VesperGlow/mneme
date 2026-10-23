#!/usr/bin/env python3
"""Consistent SQLite backup, including committed WAL data. Run inside podman unshare."""
import argparse
from contextlib import closing
import os
from pathlib import Path
import sqlite3
import tempfile


def create_backup(database, destination):
    database, destination = Path(database), Path(destination)
    if not database.is_file():
        raise ValueError("源数据库不存在")
    if destination.exists():
        raise FileExistsError("目标已存在，拒绝覆盖")
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix='.mneme-backup-', dir=destination.parent)
    os.close(descriptor)
    temporary = Path(name)
    try:
        with closing(sqlite3.connect(f"{database.resolve().as_uri()}?mode=ro", uri=True)) as source:
            with closing(sqlite3.connect(temporary)) as dest:
                source.backup(dest)
                if dest.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                    raise ValueError("备份完整性检查失败")
        with temporary.open('rb') as data:
            os.fsync(data.fileno())
        # Publish only a complete backup; never overwrite a concurrent writer.
        os.link(temporary, destination)
        descriptor = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    finally:
        temporary.unlink(missing_ok=True)


def verify_restore(database):
    """Restore into a separate database and check pages, FKs and table counts."""
    with tempfile.TemporaryDirectory(prefix='mneme-restore-') as directory:
        restored = Path(directory)/'restored.sqlite3'
        create_backup(database, restored)
        with closing(sqlite3.connect(f'{Path(database).resolve().as_uri()}?mode=ro', uri=True)) as source:
            with closing(sqlite3.connect(restored)) as target:
                if target.execute('PRAGMA foreign_key_check').fetchone() is not None:
                    raise ValueError('恢复后的外键检查失败')
                tables = [row[0] for row in source.execute("SELECT name FROM sqlite_master WHERE type='table'")]
                counts = {}
                for table in tables:
                    quoted = '"'+table.replace('"','""')+'"'
                    before = source.execute(f'SELECT COUNT(*) FROM {quoted}').fetchone()[0]
                    after = target.execute(f'SELECT COUNT(*) FROM {quoted}').fetchone()[0]
                    if before != after:
                        raise ValueError('恢复后的数据行数不一致')
                    counts[table] = after
                return counts


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("database", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument('--verify-restore', action='store_true')
    args = parser.parse_args()
    os.umask(0o077)
    create_backup(args.database, args.destination)
    if args.verify_restore:
        verify_restore(args.destination)
    print(args.destination)


if __name__ == '__main__':
    main()
