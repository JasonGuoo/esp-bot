#!/usr/bin/env python3
"""Prepare a local S31 sdkconfig for the complete 小呆小呆 MultiNet7 wake.

Uses only the Python standard library. Preserves unrelated settings, backs up
an existing sdkconfig before changes, and never builds, downloads or flashes.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import os
from pathlib import Path
import re
import stat
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
ASSIGNMENT = re.compile(r"^(CONFIG_[A-Z0-9_]+)=(.*)$")
DISABLED = re.compile(r"^# (CONFIG_[A-Z0-9_]+) is not set$")
SETTINGS = {
    "CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK": "y",
    "CONFIG_BOT_AUDIO_MULTINET_THRESHOLD_PERCENT": "80",
    "CONFIG_BOT_AUDIO_MULTINET_WINDOW_MS": "3000",
    "CONFIG_SR_MN_CN_MULTINET7_QUANT": "y",
    "CONFIG_SR_MN_CN_NONE": "n",
    "CONFIG_SR_MN_EN_NONE": "y",
    "CONFIG_MODEL_IN_FLASH": "y",
    "CONFIG_MODEL_IN_SDCARD": "n",
    "CONFIG_SR_VADN_WEBRTC": "y",
    "CONFIG_BOT_AUDIO_WAKE_MODEL_NAME": '""',
    "CONFIG_BOT_AUDIO_MODEL_PARTITION": '"model"',
}
CHOICE_PREFIXES = ("CONFIG_SR_MN_CN_", "CONFIG_SR_MN_EN_", "CONFIG_SR_VADN_")


def setting(line: str) -> tuple[str, str] | None:
    line = line.rstrip("\r\n")
    match = ASSIGNMENT.fullmatch(line)
    if match:
        return match.group(1), match.group(2)
    match = DISABLED.fullmatch(line)
    return (match.group(1), "n") if match else None


def prepare(original: str, use_multinet: bool) -> tuple[str, list[str]]:
    lines = original.splitlines(keepends=True)
    entries = [entry for line in lines if (entry := setting(line))]
    values = dict(entries)

    if values.get("CONFIG_IDF_TARGET", '"esp32s31"') != '"esp32s31"':
        raise ValueError("This helper only prepares ESP32-S31; sdkconfig has another target.")
    if values.get("CONFIG_BOT_AUDIO_MODEL_PARTITION", '"model"') != '"model"':
        raise ValueError(
            "A different BOT_AUDIO_MODEL_PARTITION is configured. Nothing changed. "
            "Check the actual partition table before selecting the project's model partition."
        )
    custom_wake = any(
        key == "CONFIG_BOT_AUDIO_WAKE_MODEL_NAME" and value != '""'
        for key, value in entries
    )
    if custom_wake and not use_multinet:
        raise ValueError(
            "An existing dedicated WakeNet entry is configured; it has priority in firmware. "
            "Nothing changed. To explicitly select MultiNet7 for this test, rerun with "
            "--use-multinet. A backup is saved before clearing the entry name. "
            "External model paths and WakeNet model selections are retained."
        )

    updates = dict(SETTINGS)
    # Kconfig choices must have only one selected member. Canonical comments
    # also prevent ESP-SR's packer from treating an explicit '=n' as selected.
    for key, value in entries:
        if key.startswith(CHOICE_PREFIXES) and value in ("y", "n"):
            updates.setdefault(key, "n")

    newline = "\r\n" if "\r\n" in original else "\n"
    seen: set[str] = set()
    result: list[str] = []
    changed: list[str] = []
    for line in lines:
        entry = setting(line)
        if not entry or entry[0] not in updates:
            result.append(line)
            continue
        key, _ = entry
        if key in seen:
            if key not in changed:
                changed.append(key)
            continue
        seen.add(key)
        value = updates[key]
        rendered = f"# {key} is not set" if value == "n" else f"{key}={value}"
        ending = "\r\n" if line.endswith("\r\n") else "\n" if line.endswith("\n") else ""
        replacement = rendered + ending
        result.append(replacement)
        if replacement != line:
            changed.append(key)

    missing = [key for key in updates if key not in seen]
    if missing:
        if result and not result[-1].endswith(("\n", "\r")):
            result[-1] += newline
        result.append(newline + "# Complete 小呆小呆 wake test; see docs/wake-test.md" + newline)
        for key in missing:
            value = updates[key]
            rendered = f"# {key} is not set" if value == "n" else f"{key}={value}"
            result.append(rendered + newline)
        changed.extend(missing)
    return "".join(result), changed


def backup_config(path: Path, data: bytes, directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    digest = hashlib.sha256(data).hexdigest()[:12]
    backup = directory / f"{path.name}.{stamp}.{digest}.bak"
    fd = os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    return backup


def write_config(path: Path, data: bytes, original: bytes | None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    mode = stat.S_IMODE(path.stat().st_mode) if original is not None else 0o600
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, prefix=".wake-config-", delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, mode)
        if (path.read_bytes() if path.exists() else None) != original:
            raise ValueError("sdkconfig changed while preparing it; refused to overwrite the newer file.")
        os.replace(temporary, path)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdkconfig", type=Path, default=ROOT / "firmware/sdkconfig",
                        help="configuration to update (default: firmware/sdkconfig)")
    parser.add_argument("--defaults", type=Path, default=ROOT / "firmware/sdkconfig.defaults",
                        help="seed only when sdkconfig is absent; never replaces existing settings")
    parser.add_argument("--backup-dir", type=Path,
                        help="private backup directory (default: beside sdkconfig, .bringup/wake-config)")
    parser.add_argument("--dry-run", action="store_true", help="describe changes without writing files")
    parser.add_argument("--use-multinet", action="store_true",
                        help="explicitly clear an existing dedicated WakeNet entry after backing it up")
    args = parser.parse_args()
    try:
        path = args.sdkconfig.expanduser().resolve()
        defaults = args.defaults.expanduser().resolve()
        if path == defaults or path == (ROOT / "firmware/sdkconfig.defaults").resolve():
            raise ValueError("Use a local sdkconfig output path; this helper does not edit defaults.")
        original = path.read_bytes() if path.exists() else None
        source = original if original is not None else defaults.read_bytes()
        prepared, changed = prepare(source.decode("utf-8"), args.use_multinet)
        data = prepared.encode("utf-8")
        if original == data:
            print(f"Already prepared: {path}")
            print("No file written and no extra backup created.")
            return 0

        print(f"{'Would update' if args.dry_run else 'Preparing'}: {path}")
        if original is None:
            print(f"Initial configuration comes from: {defaults}")
        print("Wake settings: MultiNet7 Chinese, score 0.80, window 3000 ms, model partition 'model'.")
        print("Affected keys: " + ", ".join(changed))
        if args.dry_run:
            print("Dry run: no configuration or backup written.")
            return 0
        if original is not None:
            directory = (args.backup_dir.expanduser().resolve() if args.backup_dir
                         else path.parent / ".bringup/wake-config")
            backup = backup_config(path, original, directory)
            print(f"Original configuration backup: {backup}")
        write_config(path, data, original)
        print("Prepared. Other compiler, camera, display, network and provisioning settings are retained.")
        print("Next: reconfigure/build with ESP-IDF, then follow docs/wake-test.md for model backup and flashing.")
        print("This helper does not download, build, connect to a board or flash anything.")
        return 0
    except (OSError, UnicodeError, ValueError) as exc:
        print(f"Cannot prepare wake test: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
