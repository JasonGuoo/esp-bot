#!/usr/bin/env python3
"""Relocate an installed Bot to ~/Xiaodai_Server after stopping the App.

Clone first, rewrite only operational paths, keep the original as a full backup.
Never rewrite historical message/instruction bodies or access provider Keychain.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sqlite3
import stat
import subprocess

from migrate_data_root import copy_tree, replace_prefix

MARKER = ".migration-in-progress"
DATABASES = ("runtime/memory/memory.sqlite3", "runtime/tasks/tasks.sqlite3",
             "runtime/tasks/artifacts/manifest.sqlite3", "runtime/xdg/data/opencode/opencode.db")


def rewrite_files(root: Path, old: Path) -> list[str]:
    changed = []
    for name in ("brain.json", "config.json", "opencode-tools.json"):
        path = root / name
        if path.is_symlink():
            raise RuntimeError("Configuration must be a real file")
        if path.is_file():
            before = json.loads(path.read_text())
            after = replace_prefix(before, old, root)
            if after != before:
                path.write_text(json.dumps(after, ensure_ascii=False, indent=2) + "\n")
                changed.append(name)
            path.chmod(0o600)
    # Python venv launchers/activation scripts carry absolute interpreter paths.
    # Preserve packages and external base-Python links; never touch signed Apps.
    engines = root / "engines"
    candidates = list(engines.glob("*/bin/*")) + list(engines.glob("*/pyvenv.cfg"))
    candidates += list(engines.glob("*/lib/python*/site-packages/*.pth"))
    candidates += list(engines.glob("*/lib/python*/site-packages/*.egg-link"))
    for path in candidates:
        if path.is_symlink():
            target = os.readlink(path)
            replacement = replace_prefix(target, old, root)
            if replacement != target:
                path.unlink(); path.symlink_to(replacement)
                changed.append(str(path.relative_to(root)))
        elif path.is_file() and path.stat().st_size <= 256 * 1024:
            try:
                before = path.read_text()
            except UnicodeError:
                continue
            if str(old) in before:
                # These are generated launchers, never conversation content.
                mode = stat.S_IMODE(path.stat().st_mode)
                path.write_text(before.replace(str(old), str(root)))
                path.chmod(mode)
                changed.append(str(path.relative_to(root)))
    return changed


def check_database(path: Path) -> None:
    if path.is_symlink():
        raise RuntimeError("Database must be a real file")
    if not path.exists():
        return
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True) as db:
        if db.execute("PRAGMA quick_check").fetchall() != [("ok",)]:
            raise RuntimeError("Database integrity check failed: " + path.name)


def relocate_opencode(root: Path, old: Path) -> int:
    path = root / DATABASES[-1]
    if not path.exists():
        return 0
    count = 0
    with sqlite3.connect(path) as db:
        db.execute("BEGIN IMMEDIATE")
        tables = {r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        # Existing IDs, project ownership, message blobs/hashes and session ledger
        # remain intact; only the execution-directory metadata moves.
        for table, column in (("project", "worktree"), ("session_v2", "directory")):
            if table not in tables:
                continue
            count += db.execute(
                f'UPDATE "{table}" SET "{column}" = ? || substr("{column}", ?) '
                f'WHERE "{column}" = ? OR substr("{column}", 1, ?) = ?',
                (str(root), len(str(old)) + 1, str(old), len(str(old)) + 1, str(old) + "/"),
            ).rowcount
    return count


def relocate(source: Path, destination: Path, backup: Path, app_bundle: Path | None = None) -> dict:
    source, destination, backup = (p.expanduser().absolute() for p in (source, destination, backup))
    if source.is_symlink() or not (source / "brain.json").is_file():
        raise RuntimeError("Source must be a real, configured Bot data directory")
    for target in (destination, backup):
        if target.exists() or target.is_symlink() or target == source or target.is_relative_to(source):
            raise RuntimeError("Destination/backup must be new and outside the source")
    if backup.is_relative_to(destination) or destination.is_relative_to(backup):
        raise RuntimeError("Destination and backup must not contain each other")
    if (source / MARKER).exists():
        raise RuntimeError("An earlier migration is incomplete")
    database_files = [str(source / p) for p in DATABASES if (source / p).is_file()]
    if database_files:
        opened = subprocess.run(["/usr/sbin/lsof", "-t", *database_files], capture_output=True, timeout=10)
        if opened.returncode == 0:
            raise RuntimeError("Bot databases are in use; stop and quit the App before relocating")
        if opened.returncode != 1:
            raise RuntimeError("Could not verify that Bot databases are closed")
    # Parse before creating a destination. No partial install for malformed JSON.
    for name in ("brain.json", "config.json", "opencode-tools.json"):
        path = source / name
        if path.is_symlink():
            raise RuntimeError("Configuration must be a real file")
        if path.exists():
            if not isinstance(json.loads(path.read_text()), dict):
                raise RuntimeError("Configuration must contain a JSON object")
    for name in DATABASES:
        check_database(source / name)
    (source / MARKER).write_text("Relocation in progress; do not launch the App.\n")
    try:
        copy_tree(source, destination)
        destination.chmod(0o700)
        changed = rewrite_files(destination, source)
        directories = relocate_opencode(destination, source)
        for name in DATABASES:
            check_database(destination / name)
        if app_bundle is not None:
            installed = destination / "ESP Bot Server.app"
            if installed.exists():
                retained = destination / "runtime/relocation/previous.app"
                retained.parent.mkdir(mode=0o700, parents=True, exist_ok=False)
                installed.rename(retained)
            copy_tree(app_bundle, installed)
        report = {"source": str(source), "destination": str(destination), "backup": str(backup),
                  "changed_files": changed, "opencode_directories": directories,
                  "history_rewritten": False, "databases_checked": list(DATABASES)}
        (destination / "relocation.json").write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
        backup.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        source.rename(backup)
        (backup / MARKER).unlink()
        (destination / MARKER).unlink()
        return report
    except BaseException:
        # Keep an incomplete destination fenced. The original is still usable
        # at source (or at backup if the last filesystem operation failed).
        if destination.is_dir():
            (destination / MARKER).write_text("Incomplete relocation; inspect before using.\n")
        if (source / MARKER).is_file():
            (source / MARKER).unlink()
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path.home() / "小呆 Server")
    parser.add_argument("--destination", type=Path, default=Path.home() / "Xiaodai_Server")
    parser.add_argument("--backup", type=Path, required=True)
    parser.add_argument("--app-bundle", type=Path, required=True, help="New local build using the English data root")
    args = parser.parse_args()
    os.umask(0o077)
    result = relocate(args.source, args.destination, args.backup, args.app_bundle)
    print(f"Relocated to {result['destination']}; original retained at {result['backup']}")
    print(f"Updated {len(result['changed_files'])} operational files and {result['opencode_directories']} session/project directories; history unchanged.")


if __name__ == "__main__":
    main()
