#!/usr/bin/env python3
"""One-time, non-destructive migration of this Bot's local Mac data.

Stop the old app first. Session transfer uses OpenCode's own export/import;
the old global database is read through a temporary SQLite snapshot and is
never modified. Legacy provider Keychain entries are not read or migrated;
managed model keys come from the current App session or launch environment.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile

APP = Path(__file__).resolve().parents[1]
OLD_DEFAULT = Path.home() / "Library/Application Support/ai.reasonia.espbot.server"
DEST_DEFAULT = Path.home() / "Xiaodai_Server"


def read_jsonc_object(path: Path) -> dict:
    """Accept JSON comments/trailing commas while keeping JSON strings intact.

    The standard decoder finds string boundaries, including escaped quotes and
    backslashes. Replace only syntax outside strings; URLs and command arguments
    containing comment-like text must reach OpenCode unchanged. Retaining spaces
    and line breaks also preserves useful JSON error locations.
    """
    text = path.read_text(encoding="utf-8-sig")
    decoder = json.JSONDecoder()
    characters = list(text)
    index = 0
    while index < len(text):
        if text[index] == '"':
            _, index = decoder.raw_decode(text, index)
            continue
        if text.startswith("//", index):
            end = index + 2
            while end < len(text) and text[end] not in "\r\n":
                end += 1
        elif text.startswith("/*", index):
            closing = text.find("*/", index + 2)
            if closing < 0:
                raise ValueError(f"Unterminated JSONC block comment in {path}")
            end = closing + 2
        else:
            index += 1
            continue
        for position in range(index, end):
            if text[position] not in "\r\n":
                characters[position] = " "
        index = end

    text = "".join(characters)
    index = 0
    while index < len(text):
        if text[index] == '"':
            _, index = decoder.raw_decode(text, index)
            continue
        if text[index] == ",":
            following, preceding = index + 1, index - 1
            while following < len(text) and text[following] in " \t\r\n":
                following += 1
            while preceding >= 0 and text[preceding] in " \t\r\n":
                preceding -= 1
            if (following < len(text) and text[following] in "]}" and preceding >= 0
                    and text[preceding] not in "[{,:"):
                characters[index] = " "
        index += 1
    value = json.loads("".join(characters))
    if not isinstance(value, dict):
        raise ValueError(f"OpenCode configuration must contain a JSON object: {path}")
    return value


def copy_tree(source: Path, destination: Path) -> None:
    if not source.is_dir():
        raise RuntimeError(f"Required directory is missing: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    # APFS clones avoid duplicating large model files. A regular copy is fine
    # on other filesystems; neither variant alters the source.
    if sys.platform == "darwin":
        result = subprocess.run(["/bin/cp", "-cR", str(source), str(destination)],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if result.returncode == 0:
            return
        if destination.exists():
            raise RuntimeError(f"Could not clone {source}; partial destination retained: {destination}")
    shutil.copytree(source, destination, symlinks=True)


def copy_legacy(source: Path, target: Path) -> None:
    if not source.exists():
        return
    for item in source.rglob("*"):
        if item.is_symlink():
            raise RuntimeError(f"Legacy data contains a symbolic link; migration stopped: {item}")
        relative = item.relative_to(source)
        out = target / relative
        if item.is_dir():
            out.mkdir(mode=0o700, parents=True, exist_ok=True)
        elif item.name.endswith((".sqlite3-wal", ".sqlite3-shm", ".sqlite3-journal")):
            continue
        elif item.name.endswith(".sqlite3"):
            out.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            with sqlite3.connect(f"file:{item}?mode=ro", uri=True) as old, sqlite3.connect(out) as new:
                old.backup(new)
            out.chmod(0o600)
        elif item.is_file():
            out.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            shutil.copy2(item, out)


def replace_prefix(value, old: Path, new: Path):
    if isinstance(value, str):
        prefix = str(old)
        return str(new) + value[len(prefix):] if value == prefix or value.startswith(prefix + "/") else value
    if isinstance(value, list):
        return [replace_prefix(x, old, new) for x in value]
    if isinstance(value, dict):
        return {k: replace_prefix(v, old, new) for k, v in value.items()}
    return value


def env_for(root: Path) -> dict[str, str]:
    env = os.environ.copy()
    for key in ("OPENCODE_CONFIG", "OPENCODE_CONFIG_CONTENT", "OPENCODE_PASSWORD"):
        env.pop(key, None)
    for name in ("data", "config", "cache", "state"):
        folder = root / name
        folder.mkdir(mode=0o700, parents=True, exist_ok=True)
        env[f"XDG_{name.upper()}_HOME"] = str(folder)
    env["OPENCODE_DISABLE_PROJECT_CONFIG"] = "true"
    env["OPENCODE_DISABLE_CLAUDE_CODE"] = "true"
    env["TMPDIR"] = str(root / "tmp")
    (root / "tmp").mkdir(mode=0o700, exist_ok=True)
    return env


def transfer_sessions(root: Path, old_data: Path, executable: Path) -> int:
    ledger = root / "runtime/opencode/sessions.json"
    if not ledger.exists():
        return 0
    conversations = json.loads(ledger.read_text())["conversations"]
    ids = {sid for entry in conversations.values()
           for sid in [entry.get("current_session"), *entry.get("roots", [])] if isinstance(sid, str) and sid}
    if not ids:
        return 0
    old_db = old_data / "opencode/opencode.db"
    if not old_db.is_file():
        raise RuntimeError("Old OpenCode session database is missing; refusing to discard its session references")
    xdg = root / "runtime/xdg"
    workspace = root / "runtime/opencode/workspace"
    workspace.mkdir(mode=0o700, parents=True, exist_ok=True)
    new_env = env_for(xdg)
    with tempfile.TemporaryDirectory(prefix="session-migration-", dir=root / "runtime/tmp") as temp:
        scratch = Path(temp)
        old_env = env_for(scratch / "old-xdg")
        snapshot = Path(old_env["XDG_DATA_HOME"]) / "opencode/opencode.db"
        snapshot.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        with sqlite3.connect(f"file:{old_db}?mode=ro", uri=True) as old, sqlite3.connect(snapshot) as new:
            old.backup(new)
        snapshot.chmod(0o600)
        for index, sid in enumerate(sorted(ids)):
            exported = subprocess.run([str(executable), "session", "export", "--standalone", sid],
                                      env=old_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
            if exported.returncode:
                raise RuntimeError("OpenCode could not export an existing Bot session; original data remains unchanged")
            transfer = scratch / f"session-{index}.json"
            transfer.write_bytes(exported.stdout)
            transfer.chmod(0o600)
            imported = subprocess.run([str(executable), "session", "import", "--standalone", "--directory", str(workspace), str(transfer)],
                                      env=new_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
            transfer.unlink(missing_ok=True)
            if imported.returncode:
                raise RuntimeError("OpenCode could not import a Bot session; original data remains unchanged")
    return len(ids)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--legacy-dir", type=Path, default=OLD_DEFAULT)
    parser.add_argument("--destination", type=Path, default=DEST_DEFAULT)
    parser.add_argument("--old-opencode-data", type=Path, default=Path.home() / ".local/share")
    args = parser.parse_args()
    root, legacy = args.destination.expanduser().absolute(), args.legacy_dir.expanduser().absolute()
    if root.exists() or root.is_symlink():
        raise RuntimeError(f"Destination already exists; nothing was overwritten: {root}")
    if not (legacy / "brain.json").is_file():
        raise RuntimeError("Existing brain.json not found; this helper only migrates an existing installation")
    # Validate the small configuration inputs before creating an incomplete
    # destination or copying models. The original JSON/JSONC file stays intact.
    legacy_config = json.loads((legacy / "brain.json").read_text(encoding="utf-8"))
    inherited = legacy_config.get("harness", {}).get("inherit_config")
    inherited_config = read_jsonc_object(Path(inherited)) if inherited else None
    root.mkdir(mode=0o700, parents=False)
    marker = root / ".migration-in-progress"
    marker.write_text("Migration incomplete; do not start the app until it succeeds.\n")
    marker.chmod(0o600)
    try:
        print("Copying existing configuration and memory database…", flush=True)
        copy_legacy(legacy, root)
        (root / "runtime/tmp").mkdir(mode=0o700, parents=True, exist_ok=True)
        config_path = root / "brain.json"
        config = replace_prefix(legacy_config, legacy, root)
        models = config["models"]
        source_models = Path(models["model_dir"])
        copy_tree(source_models, root / "models/local")
        models["model_dir"] = str(root / "models/local")
        copy_tree(APP / "local-models/.venv", root / "engines/model-python")
        models["python"] = str(root / "engines/model-python/bin/python")
        sources = root / "engines/local-models"
        sources.mkdir(mode=0o700, parents=True)
        for name in ("worker.py", "model_store.py", "rma_qwen.py", "memory_extract.py", "say_guard.py", "qwen_robot.py", "models.json"):
            shutil.copy2(APP / "local-models" / name, sources / name)
        models["script"] = str(sources / "worker.py")
        if models.get("asr_backend") == "rma_qwen":
            rma_python = Path(models["rma_qwen_python"])
            rma_worker = Path(models["rma_qwen_worker"])
            rma_model = Path(models["rma_qwen_model"])
            copy_tree(rma_python.parent.parent, root / "engines/rma-python")
            rma_sources = root / "engines/rma-worker"
            rma_sources.mkdir(mode=0o700, parents=True)
            for file in rma_worker.parent.iterdir():
                if file.is_file() and file.suffix in (".py", ".txt", ".md"):
                    shutil.copy2(file, rma_sources / file.name)
            copy_tree(rma_model, root / "models/qwen3-asr")
            models.update(rma_qwen_python=str(root / "engines/rma-python/bin/python"),
                          rma_qwen_worker=str(rma_sources / rma_worker.name),
                          rma_qwen_model=str(root / "models/qwen3-asr"))
        print("Copying OpenCode and its configured tools…", flush=True)
        copy_tree(APP / "runtime/bin", root / "engines/opencode-bin")
        copy_tree(APP / "runtime/tooling", root / "engines/tooling")
        copy_tree(APP / ".venv-tools", root / "engines/tools-python")
        (root / "engines/tools").mkdir(mode=0o700, parents=True)
        shutil.copy2(APP / "tools/deepseek_search.py", root / "engines/tools/deepseek_search.py")
        harness = config["harness"]
        harness["executable"] = str(root / "engines/opencode-bin/opencode")
        if inherited_config is not None:
            mappings = [(APP / "runtime/tooling", root / "engines/tooling"),
                        (APP / ".venv-tools", root / "engines/tools-python"),
                        (APP / "tools", root / "engines/tools")]
            for source, target in mappings:
                inherited_config = replace_prefix(inherited_config, source, target)
            inherited_path = root / "opencode-tools.json"
            inherited_path.write_text(json.dumps(inherited_config, ensure_ascii=False, indent=2) + "\n")
            inherited_path.chmod(0o600)
            harness["inherit_config"] = str(inherited_path)
        config_path.write_text(json.dumps(config, ensure_ascii=False, indent=2) + "\n")
        config_path.chmod(0o600)
        print("Transferring only 小呆 OpenCode sessions…", flush=True)
        count = transfer_sessions(root, args.old_opencode_data, Path(harness["executable"]))
        (root / "README.md").write_text("# Xiaodai_Server\n\n记忆、家庭成员资料、OpenCode 会话、模型和工作文件保存在此目录。\n工作区：`runtime/opencode/workspace`。模型 API Key 仅来自当前 App 会话或启动环境。\n", encoding="utf-8")
        marker.unlink()
        print(f"Migration complete: {count} Bot session(s). Original data and Keychain entries were retained.")
    except Exception:
        print(f"Migration stopped. The incomplete directory was retained at {root}; original data was not removed.", file=sys.stderr)
        raise


if __name__ == "__main__":
    main()
