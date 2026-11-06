"""Read-only model bundles shared by the JSONL worker and explicit provisioner.

This module never imports a model framework and never downloads anything.
Model source/revision changes require a different model_id and re-enrollment.
"""

from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path
from typing import Any


class ServiceError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def load_manifest() -> dict[str, Any]:
    path = Path(__file__).with_name("models.json")
    with path.open("r", encoding="utf-8") as stream:
        manifest = json.load(stream)
    if manifest.get("schema_version") != 1:
        raise ServiceError("manifest_invalid", "Unsupported model manifest version.")
    return manifest


def contained_path(root: Path, relative: str) -> Path:
    """No asset may leave the selected model directory or traverse a symlink."""
    part = Path(relative)
    if part.is_absolute() or not part.parts or ".." in part.parts:
        raise ServiceError("manifest_invalid", "Invalid model asset path.")
    root = root.resolve()
    candidate = root
    for item in part.parts:
        candidate = candidate / item
        if candidate.is_symlink():
            raise ServiceError("model_bundle_invalid", "Model asset symlinks are not supported.")
    if not candidate.resolve().is_relative_to(root):
        raise ServiceError("model_bundle_invalid", "Model asset is outside its bundle.")
    return candidate


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class ModelStore:
    """The installation receipt is written last; interrupted downloads stay unusable."""

    def __init__(self, root: Path):
        self.root = root.expanduser().resolve()
        self.manifest = load_manifest()
        self.verified: set[str] = set()

    def spec(self, kind: str) -> dict[str, Any]:
        try:
            return self.manifest["models"][kind]
        except KeyError as exc:
            raise ServiceError("unknown_model", "Unknown local model bundle.") from exc

    def directory(self, kind: str) -> Path:
        return contained_path(self.root, self.spec(kind)["directory"])

    def _receipt(self, kind: str) -> dict[str, Any]:
        spec = self.spec(kind)
        path = contained_path(self.directory(kind), "installation.json")
        if not path.is_file() or path.stat().st_size > 65536:
            raise ServiceError("model_missing", "Run the explicit model provisioner first.")
        try:
            receipt = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            raise ServiceError("model_bundle_invalid", "Cannot read the model installation receipt.") from exc
        if (
            not isinstance(receipt, dict)
            or receipt.get("schema_version") != 1
            or receipt.get("model_id") != spec["model_id"]
            or receipt.get("revision") != spec["revision"]
            or not isinstance(receipt.get("files"), dict)
        ):
            raise ServiceError("model_bundle_invalid", "Model receipt does not match the pinned manifest.")
        return receipt

    def status(self, kind: str) -> dict[str, Any]:
        spec = self.spec(kind)
        result = {
            "model_id": spec["model_id"],
            "revision": spec["revision"],
            "installed": False,
            "integrity_checked": kind in self.verified,
        }
        try:
            self._check_files(kind, verify_hashes=False)
            result["installed"] = True
        except (ServiceError, OSError) as exc:
            result["error_code"] = exc.code if isinstance(exc, ServiceError) else "model_unreadable"
        return result

    def _check_files(self, kind: str, *, verify_hashes: bool) -> Path:
        spec = self.spec(kind)
        directory = self.directory(kind)
        receipt = self._receipt(kind)
        for asset in spec["files"]:
            path = contained_path(directory, asset["path"])
            record = receipt["files"].get(asset["path"])
            if not isinstance(record, dict) or not path.is_file():
                raise ServiceError("model_missing", "Model bundle is incomplete; provision it again.")
            expected = record.get("sha256")
            size = record.get("bytes")
            if (
                not isinstance(expected, str)
                or not re.fullmatch(r"[0-9a-f]{64}", expected)
                or type(size) is not int
                or size <= 0
                or size > asset["max_bytes"]
                or path.stat().st_size != size
                or (asset.get("sha256") and asset["sha256"] != expected)
            ):
                raise ServiceError("model_bundle_invalid", "Model asset metadata does not match its receipt.")
            if verify_hashes and sha256_file(path) != expected:
                raise ServiceError("model_checksum_mismatch", "Model asset changed; provision it again.")
        return directory

    def require(self, kind: str) -> Path:
        directory = self._check_files(kind, verify_hashes=kind not in self.verified)
        self.verified.add(kind)
        return directory
