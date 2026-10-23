"""Exercise committed WAL recovery, restore checks and failed-backup retention."""
import importlib.util
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('scheduled_backup', ROOT/'scripts/backup-scheduled.py')
scheduled = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scheduled)
backup = scheduled.backup


class BackupTests(unittest.TestCase):
    def test_live_wal_backup_is_complete_private_and_never_overwrites(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with closing(sqlite3.connect(root/'live.db')) as source, source:
                source.execute('PRAGMA journal_mode=WAL')
                source.execute('PRAGMA wal_autocheckpoint=0')
                source.execute('CREATE TABLE messages(id INTEGER PRIMARY KEY,content TEXT)')
                source.execute('INSERT INTO messages(content) VALUES(?)',('原始中文记忆',))
                source.commit()
                self.assertTrue((root/'live.db-wal').stat().st_size)
                target = root/'snapshot.db'
                backup.create_backup(root/'live.db',target)
                self.assertEqual(os.stat(target).st_mode & 0o777,0o600)
                self.assertEqual(backup.verify_restore(target)['messages'],1)
                with closing(sqlite3.connect(target)) as restored:
                    self.assertEqual(restored.execute('SELECT content FROM messages').fetchone()[0],'原始中文记忆')
                with self.assertRaises(FileExistsError):
                    backup.create_backup(root/'live.db',target)

    def test_only_successful_snapshots_trigger_retention_and_failed_restore_keeps_previous_backups(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);config=root/'config';config.mkdir()
            (config/'app.env').write_text('FIXTURE_SECRET=private-test-only\n')
            database=root/'live.db'
            with closing(sqlite3.connect(database)) as source, source:
                source.executescript('CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent_id INTEGER REFERENCES parents(id));')
            destination=root/'backups'
            destination.mkdir(); (destination/'unmanaged.txt').write_text('keep')
            for _ in range(3):
                newest=scheduled.snapshot(destination,config,retain=2,database=database)
            snapshots=list(destination.glob('mneme-*'))
            self.assertEqual(len(snapshots),2)
            self.assertTrue(newest.is_dir())
            self.assertEqual((newest/'config/app.env').read_text(),(config/'app.env').read_text())
            self.assertEqual((newest/'config/app.env').stat().st_mode & 0o777,0o600)
            manifest=json.loads((newest/'manifest.json').read_text())
            self.assertTrue(manifest['sqlite_restore_verified'])
            self.assertIn('config/app.env',manifest['files'])
            with closing(sqlite3.connect(database)) as source, source:
                source.execute('INSERT INTO children VALUES(999)')
            with self.assertRaisesRegex(ValueError,'外键检查失败'):
                scheduled.snapshot(destination,config,retain=1,database=database)
            self.assertEqual(set(destination.glob('mneme-*')),set(snapshots))
            self.assertEqual(list(destination.glob('.partial-*')),[])
            self.assertTrue((destination/'unmanaged.txt').is_file())

    def test_recursive_destination_and_symlinked_config_fail_without_publishing(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);config=root/'config';config.mkdir()
            database=root/'live.db'
            with closing(sqlite3.connect(database)) as source, source:
                source.execute('CREATE TABLE data(value TEXT)')
            with self.assertRaisesRegex(ValueError,'配置目录内'):
                scheduled.snapshot(config/'backups',config,database=database)
            (config/'link').symlink_to(database)
            with self.assertRaisesRegex(ValueError,'符号链接'):
                scheduled.snapshot(root/'backups',config,database=database)
            self.assertEqual(list((root/'backups').iterdir()),[])

    def test_custom_layout_copies_only_reviewed_config_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);database=root/'live.db'
            with closing(sqlite3.connect(database)) as source, source:
                source.execute('CREATE TABLE data(value TEXT)')
            env=root/'api.env';env.write_text('FIXTURE_SECRET=test-only')
            unit=root/'api.container';unit.write_text('[Container]\nImage=fixture')
            published=scheduled.snapshot(root/'backups',root/'nonexistent-default',database=database,config_files=[env,unit])
            self.assertEqual({p.name for p in (published/'config').iterdir()},{'api.env','api.container'})
