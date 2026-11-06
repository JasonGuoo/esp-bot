#!/usr/bin/env python3
"""Explicitly download immutable public model assets; never run model code.

Usage: python provision_models.py --model-dir /absolute/path --only asr speaker face
The worker does not call this program. See models.json for every source URL.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import tempfile
import urllib.request
from pathlib import Path

from model_store import ModelStore, ServiceError, contained_path, sha256_file


def download(asset: dict, directory: Path) -> dict:
    destination = contained_path(directory, asset["path"])
    if not asset["url"].startswith("https://"):
        raise ServiceError("manifest_invalid", "Model downloads must use HTTPS.")
    destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    digest = hashlib.sha256()
    count = 0
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination.parent, prefix=".download-", delete=False) as output:
            temporary = Path(output.name)
            request = urllib.request.Request(asset["url"], headers={"User-Agent": "esp-bot-local-model-provisioner/1"})
            with urllib.request.urlopen(request, timeout=60) as response:
                if not response.geturl().startswith("https://"):
                    raise ServiceError("download_failed", "Refusing a non-HTTPS download redirect.")
                for chunk in iter(lambda: response.read(1024 * 1024), b""):
                    count += len(chunk)
                    if count > asset["max_bytes"]:
                        raise ServiceError("download_too_large", "Model asset exceeds the pinned size bound.")
                    digest.update(chunk)
                    output.write(chunk)
            output.flush()
            os.fsync(output.fileno())
        actual = digest.hexdigest()
        if count == 0 or (asset.get("sha256") and actual != asset["sha256"]):
            raise ServiceError("download_checksum_mismatch", "Downloaded model does not match its published SHA256.")
        os.replace(temporary, destination)
        temporary = None
        destination.chmod(0o600)
        return {"sha256": actual, "bytes": count}
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def provision(store: ModelStore, kind: str) -> None:
    spec = store.spec(kind)
    directory = store.directory(kind)
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    receipt_path = contained_path(directory, "installation.json")
    old_files: dict = {}
    try:
        old_files = store._receipt(kind)["files"]
    except (ServiceError, OSError):
        pass
    # A worker cannot mistake an interrupted replacement for a complete bundle.
    receipt_path.unlink(missing_ok=True)
    files = {}
    for asset in spec["files"]:
        path = contained_path(directory, asset["path"])
        old = old_files.get(asset["path"], {})
        expected = asset.get("sha256") or old.get("sha256")
        if (
            path.is_file()
            and expected
            and 0 < path.stat().st_size <= asset["max_bytes"]
            and sha256_file(path) == expected
        ):
            files[asset["path"]] = {"sha256": expected, "bytes": path.stat().st_size}
            print(f"{kind}: retained {asset['path']}", flush=True)
        else:
            print(f"{kind}: downloading {asset['path']}", flush=True)
            files[asset["path"]] = download(asset, directory)
    receipt = {
        "schema_version": 1,
        "model_id": spec["model_id"],
        "revision": spec["revision"],
        "repository": spec["repository"],
        "installed_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "files": files,
    }
    temporary = contained_path(directory, ".installation.tmp")
    with temporary.open("w", encoding="utf-8") as output:
        json.dump(receipt, output, indent=2, ensure_ascii=False)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, receipt_path)
    receipt_path.chmod(0o600)
    print(f"{kind}: installed pinned bundle; no model was executed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--only", nargs="+", choices=("asr", "speaker", "face"), default=("asr", "speaker", "face"))
    args = parser.parse_args()
    os.umask(0o077)
    store = ModelStore(args.model_dir)
    try:
        for kind in dict.fromkeys(args.only):
            provision(store, kind)
    except (ServiceError, OSError, ValueError) as exc:
        # Provisioning deals only with public asset URLs; no private media/key is read.
        parser.exit(1, f"Model provisioning failed: {exc}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
