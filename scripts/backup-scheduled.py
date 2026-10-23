#!/usr/bin/env python3
"""Private database/config snapshots, restore checks and successful-only retention."""
import argparse
import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import uuid

SPEC = importlib.util.spec_from_file_location('mneme_backup', Path(__file__).with_name('backup.py'))
backup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backup)


def snapshot(destination, config, retain=14, database=None, config_files=None):
    if not 1 <= retain <= 365:
        raise ValueError('保留数量必须为 1–365')
    destination, config = Path(destination), Path(config)
    config_files = [Path(path) for path in (config_files or [])]
    if config_files:
        if len({path.name for path in config_files}) != len(config_files):
            raise ValueError('配置文件名重复，拒绝覆盖')
        if any(not path.is_file() or path.is_symlink() for path in config_files):
            raise ValueError('配置文件不存在或为符号链接')
    elif not config.is_dir():
        raise ValueError('配置目录不存在，拒绝生成不完整备份')
    if not config_files and destination.resolve().is_relative_to(config.resolve()):
        raise ValueError('备份目录不能位于配置目录内')
    os.umask(0o077)
    destination.mkdir(parents=True, exist_ok=True)
    destination.chmod(0o700)
    stage = Path(tempfile.mkdtemp(prefix='.partial-', dir=destination))
    try:
        target = stage/'mneme.sqlite3'
        if database is None:
            volume = subprocess.check_output(['podman','volume','inspect','mneme-data','--format','{{.Mountpoint}}'], text=True).strip()
            if not volume:
                raise ValueError('数据卷不存在')
            subprocess.run(['podman','unshare',sys.executable,str(Path(__file__).with_name('backup.py')),str(Path(volume)/'mneme.sqlite3'),str(target),'--verify-restore'], check=True, stdout=subprocess.DEVNULL)
        else:
            backup.create_backup(database, target)
            backup.verify_restore(target)
        if config_files:
            (stage/'config').mkdir()
            for path in config_files:
                shutil.copy2(path,stage/'config'/path.name)
        else:
            shutil.copytree(config,stage/'config',symlinks=True)
        files = {}
        for path in stage.rglob('*'):
            if path.is_symlink():
                raise ValueError('配置包含符号链接，请改用普通文件后备份')
            path.chmod(0o700 if path.is_dir() else 0o600)
            if path.is_file():
                with path.open('rb') as data:
                    files[str(path.relative_to(stage))] = hashlib.file_digest(data,'sha256').hexdigest()
        (stage/'manifest.json').write_text(json.dumps({'created_at':dt.datetime.now(dt.timezone.utc).isoformat(),'sqlite_restore_verified':True,'files':files},indent=2)+'\n')
        name = 'mneme-'+dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ')+'-'+uuid.uuid4().hex[:8]
        published = destination/name
        stage.rename(published)
        descriptor = os.open(destination,os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        successful = sorted(path for path in destination.iterdir() if re.fullmatch(r'mneme-\d{8}T\d{6}Z-[a-f0-9]{8}',path.name) and path.is_dir() and not path.is_symlink() and (path/'manifest.json').is_file())
        # Always keep the snapshot just published, including after a clock rollback.
        previous = [path for path in successful if path != published]
        for path in previous[:max(0,len(previous)-(retain-1))]:
            shutil.rmtree(path)
        return published
    finally:
        if stage.exists():
            shutil.rmtree(stage)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--destination',type=Path,default=Path.home()/'.local/state/mneme/backups')
    parser.add_argument('--config',type=Path,default=Path.home()/'.config/mneme')
    parser.add_argument('--retain',type=int,default=14)
    parser.add_argument('--config-file',type=Path,action='append',default=[],help='Explicit config files for a custom layout; replaces --config directory')
    parser.add_argument('--database',type=Path,help='Native SQLite path; omit for the rootless Podman volume')
    args = parser.parse_args()
    print(snapshot(args.destination,args.config,args.retain,args.database,args.config_file))


if __name__ == '__main__':
    main()
