#!/usr/bin/env python3
"""Fetch original S31-Korvo-1 V1.1 case STL/STEP, pinned and hash-verified.

Only downloads exact Espressif assets in hardware/3d-print/.../original.
No build/CI setup. Uses Python standard library.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
import urllib.request
from urllib.parse import quote

ROOT = Path(__file__).resolve().parents[1]
PACK = ROOT / "hardware" / "3d-print" / "esp32-s31-korvo-1-v1.1"
OUT = PACK / "original"
MANIFEST = PACK / "manifest.json"


def git_blob_sha(data: bytes) -> str:
    return hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify-only", action="store_true", help="check all eight files without downloading")
    parser.add_argument("--force", action="store_true", help="replace files with unexpected hashes")
    args = parser.parse_args()
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    ref = manifest["source_commit"]
    folder = manifest["source_folder"]
    OUT.mkdir(parents=True, exist_ok=True)
    failures = 0

    for entry in manifest["files"]:
        name = entry["name"]
        path = OUT / name
        expected_size = entry["size_bytes"]
        expected_sha = entry["sha_git_blob"]
        if path.is_file():
            data = path.read_bytes()
            if len(data) == expected_size and git_blob_sha(data) == expected_sha:
                print(f"OK       {name} ({len(data)} bytes)")
                continue
            if not args.force:
                print(f"INVALID  {name}: will not overwrite without --force")
                failures += 1
                continue
        if args.verify_only:
            print(f"MISSING  {name}")
            failures += 1
            continue

        url = (
            "https://raw.githubusercontent.com/"
            + manifest["source_repo"] + "/" + ref + "/"
            + folder + quote(name)
        )
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "esp-bot-cad-vendor/1.0"})
            with urllib.request.urlopen(req, timeout=35) as response:
                data = response.read(12_000_000)
            if len(data) != expected_size or git_blob_sha(data) != expected_sha:
                raise ValueError("vendor file size or SHA-1 mismatch; refused to save")
            with tempfile.NamedTemporaryFile(dir=OUT, prefix=".download-", delete=False) as tmp:
                tmp.write(data)
                tmp_path = Path(tmp.name)
            os.replace(tmp_path, path)
            print(f"DOWNLOADED {name} ({len(data)} bytes)")
        except Exception as exc:
            failures += 1
            print(f"FAILED    {name}: {exc}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
