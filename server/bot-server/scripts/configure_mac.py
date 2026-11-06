#!/usr/bin/env python3
"""Write Mac paths into brain.json without launching inference or modifying keys."""
from __future__ import annotations

import argparse
import datetime
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config-dir", type=Path, default=Path.home() / "Library/Application Support/ai.reasonia.espbot.server")
    parser.add_argument("--python", type=Path, help="Local model venv python; defaults to local-models/.venv/bin/python")
    parser.add_argument("--model-dir", type=Path)
    parser.add_argument("--opencode", help="Absolute OpenCode executable, or resolve it from PATH")
    parser.add_argument("--inherit-config", type=Path, help="Existing OpenCode v2 JSON/JSONC file; never overwritten")
    parser.add_argument("--voice", default="Tingting", help="Name of an already installed macOS voice")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("Run this preparation script on the intended Mac.")
    project = Path(__file__).resolve().parents[1]
    directory = args.config_dir.expanduser().resolve()
    directory.mkdir(parents=True, exist_ok=True)
    directory.chmod(0o700)
    path = directory / "brain.json"
    if path.is_symlink():
        parser.error("brain.json must not be a symlink")
    if path.exists():
        config = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(config, dict):
            parser.error("brain.json must contain an object")
        backup = directory / f"brain.backup-{datetime.datetime.now(datetime.timezone.utc):%Y%m%dT%H%M%S%fZ}.json"
        shutil.copyfile(path, backup)
        backup.chmod(0o600)
    else:
        config = {"schema_version": 1, "auto_start": False}
    python = (args.python or project / "local-models/.venv/bin/python").expanduser().absolute()
    if not python.is_file():
        parser.error(f"Local Python not found: {python}; create the venv first")
    executable = args.opencode or shutil.which("opencode")
    if not executable:
        parser.error("Install OpenCode v2.2.5+ first, or pass --opencode /absolute/path/opencode")
    executable_path = Path(executable).expanduser().absolute()
    if not executable_path.is_file():
        parser.error("--opencode must identify an installed executable")
    models = config.setdefault("models", {})
    models.update({"python": str(python), "script": str(project / "local-models/worker.py"),
                   "model_dir": str((args.model_dir or directory / "models").expanduser().resolve()), "voice": args.voice})
    harness = config.setdefault("harness", {})
    harness["executable"] = str(executable_path)
    if args.inherit_config:
        inherited = args.inherit_config.expanduser().resolve()
        if not inherited.is_file():
            parser.error("--inherit-config must identify an existing OpenCode v2 configuration")
        harness["inherit_config"] = str(inherited)
    # Remaining route and memory fields deliberately use the Rust serde defaults.
    fd, name = tempfile.mkstemp(prefix=".brain-", suffix=".json", dir=directory)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(config, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)
    print(f"Saved {path}")
    print("No service, microphone, camera, model download, inference or tests were started.")
    print("Restart/reload Bot Server after setting the provider-key environment variables.")


if __name__ == "__main__":
    main()
