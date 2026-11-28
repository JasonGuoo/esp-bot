"""Actual filesystem/SQLite/venv relocation regressions; no user data or models."""
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import relocate_data_root as migration


class RelocationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.old, self.new, self.backup = (self.base / p for p in ("小呆 Server", "Xiaodai_Server", "backups/original"))
        self.old.mkdir()
        self.config = {"models": {"python": str(self.old / "engines/test-python/bin/python")},
                       "external": str(self.old) + "-unrelated/file", "voice": "unchanged"}
        (self.old / "brain.json").write_text(json.dumps(self.config))

    def test_relocation_preserves_history_ids_assets_and_real_python_launcher(self):
        env = self.old / "engines/test-python"
        subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(env)], check=True, capture_output=True)
        launcher = env / "bin/probe"
        launcher.write_text('#!/bin/sh\n\'\'\'exec\' "' + str(env / "bin/python") + '" "$0" "$@"\n\' \'\'\'\nimport sys; print(sys.prefix)\n')
        launcher.chmod(0o755)
        alias = env / "bin/local-python"; alias.symlink_to(env / "bin/python")
        dbpath = self.old / migration.DATABASES[-1]; dbpath.parent.mkdir(parents=True)
        db = sqlite3.connect(dbpath)
        db.executescript('CREATE TABLE project(id TEXT PRIMARY KEY,worktree TEXT); CREATE TABLE session_v2(id TEXT PRIMARY KEY,directory TEXT); CREATE TABLE session_message(id TEXT PRIMARY KEY,data TEXT);')
        db.execute('INSERT INTO project VALUES (?,?)', ('project-id', str(self.old / 'runtime/opencode/workspace')))
        db.execute('INSERT INTO session_v2 VALUES (?,?)', ('session-id', str(self.old / 'runtime/opencode/workspace')))
        db.execute('INSERT INTO session_message VALUES (?,?)', ('message-id', 'Historical user text: ' + str(self.old)))
        db.commit(); db.close()
        (self.old / 'asset.bin').write_bytes(b'original media\x00\xff')
        report = migration.relocate(self.old, self.new, self.backup)
        self.assertFalse(self.old.exists())
        self.assertFalse((self.new / migration.MARKER).exists())
        self.assertEqual(json.loads((self.backup / 'brain.json').read_text()), self.config)
        current = json.loads((self.new / 'brain.json').read_text())
        self.assertEqual(current['models']['python'], str(self.new / 'engines/test-python/bin/python'))
        self.assertEqual(current['external'], self.config['external'])
        self.assertEqual((self.new / 'asset.bin').read_bytes(), b'original media\x00\xff')
        output = subprocess.check_output([str(self.new / 'engines/test-python/bin/probe')], text=True).strip()
        self.assertEqual(Path(output).resolve(), (self.new / 'engines/test-python').resolve())
        self.assertTrue((self.new / 'engines/test-python/bin/local-python').exists())
        db = sqlite3.connect(self.new / migration.DATABASES[-1])
        self.assertEqual(db.execute('SELECT * FROM session_v2').fetchone(), ('session-id', str(self.new / 'runtime/opencode/workspace')))
        self.assertEqual(db.execute('SELECT * FROM session_message').fetchone(), ('message-id', 'Historical user text: ' + str(self.old)))
        db.close()
        self.assertEqual(report['opencode_directories'], 2)

    def test_existing_destination_and_malformed_config_do_not_change_source(self):
        self.new.mkdir(); (self.new / 'keep').write_text('existing')
        with self.assertRaises(RuntimeError): migration.relocate(self.old, self.new, self.backup)
        self.assertEqual((self.new / 'keep').read_text(), 'existing')
        self.assertEqual(json.loads((self.old / 'brain.json').read_text()), self.config)
        self.new.joinpath('keep').unlink(); self.new.rmdir()
        (self.old / 'brain.json').write_text('{invalid')
        with self.assertRaises(json.JSONDecodeError): migration.relocate(self.old, self.new, self.backup)
        self.assertFalse(self.new.exists())
        self.assertFalse((self.old / migration.MARKER).exists())

    def test_failed_rewrite_keeps_original_and_fences_incomplete_destination(self):
        with patch.object(migration, 'rewrite_files', side_effect=OSError('injected disk failure')):
            with self.assertRaises(OSError): migration.relocate(self.old, self.new, self.backup)
        self.assertEqual(json.loads((self.old / 'brain.json').read_text()), self.config)
        self.assertFalse((self.old / migration.MARKER).exists())
        self.assertTrue((self.new / migration.MARKER).exists())
        self.assertFalse(self.backup.exists())

    def test_open_database_and_source_symlink_are_rejected(self):
        path = self.old / migration.DATABASES[0]; path.parent.mkdir(parents=True)
        db = sqlite3.connect(path); db.execute('CREATE TABLE example(id INTEGER)'); db.commit()
        try:
            with self.assertRaisesRegex(RuntimeError, 'in use'):
                migration.relocate(self.old, self.new, self.backup)
        finally:
            db.close()
        self.assertFalse(self.new.exists())
        link = self.base / 'linked'; link.symlink_to(self.old, target_is_directory=True)
        with self.assertRaises(RuntimeError): migration.relocate(link, self.new, self.backup)


if __name__ == '__main__':
    unittest.main()
