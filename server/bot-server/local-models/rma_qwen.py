"""Bridge the existing RMA Qwen worker to the local-model JSONL worker.

RMA owns model loading, version/weight checks, prewarming and inference. This
adapter only forwards bounded PCM through its existing private stdio protocol.
"""

from __future__ import annotations

import json
import importlib.util
import os
import platform
import queue
import struct
import subprocess
import sys
import threading
from pathlib import Path

from model_store import ServiceError

MODEL_ID = "qwen3-asr-0.6b-8bit:e2afc2b-desktop-v1"
MODEL_SHA256 = "b5bfe4abc1b4c6e58b633096682ec2b6297298add1527119936107d211adf0e8"
MAX_PCM_BYTES = 30 * 16000 * 2
MAX_RESPONSE_BYTES = 256 * 1024


class RmaQwen:
    def __init__(self, python: str | None, worker: str | None, model: str | None):
        self.python = Path(python) if python else None
        self.worker = Path(worker) if worker else None
        self.model = Path(model) if model else None
        self.process: subprocess.Popen | None = None
        self.ready = False

    def health(self) -> dict:
        supported = sys.platform == "darwin" and platform.machine() == "arm64"
        missing = []
        if self.python is None or not self.python.is_file() or not os.access(self.python, os.X_OK):
            missing.append("RMA Python")
        if self.worker is None or not self.worker.is_file():
            missing.append("RMA Qwen Worker")
        if importlib.util.find_spec("numpy") is None:
            missing.append("numpy")
        installed = bool(self.model and (self.model / "config.json").is_file()
                         and (self.model / "model.safetensors").is_file())
        loaded = bool(self.ready and self.process and self.process.poll() is None)
        return {"model_id": MODEL_ID, "backend": "rma_qwen3_asr_decode_only",
                "supported": supported, "installed": installed,
                "missing_dependencies": missing, "available": supported and installed and not missing,
                "integrity_checked": loaded, "loaded": loaded}

    def start(self) -> None:
        if self.ready and self.process and self.process.poll() is None:
            return
        self.stop()
        status = self.health()
        if not status["supported"]:
            raise ServiceError("unsupported_platform", "RMA Qwen ASR requires an Apple Silicon Mac.")
        if not status["installed"] or status["missing_dependencies"]:
            raise ServiceError("rma_asr_not_configured", "Configure the existing RMA Python, worker and model paths.")
        env = os.environ.copy()
        env.update({"PYTHONDONTWRITEBYTECODE": "1", "HF_HUB_OFFLINE": "1",
                    "TRANSFORMERS_OFFLINE": "1", "TOKENIZERS_PARALLELISM": "false"})
        try:
            process = subprocess.Popen(
                [str(self.python), "-u", str(self.worker), "--engine", "qwen", "--model", str(self.model)],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                bufsize=0, env=env,
            )
        except OSError as exc:
            raise ServiceError("rma_asr_start_failed", "Cannot start the configured RMA Qwen worker.") from exc
        self.process = process
        signals: queue.Queue[bool] = queue.Queue(maxsize=1)

        def drain_stderr() -> None:
            announced = False
            verified = False
            try:
                while True:
                    line = process.stderr.readline(8192)
                    if not line:
                        break
                    if not announced and line.startswith(b"RMA_RUNTIME "):
                        try:
                            provenance = json.loads(line[len(b"RMA_RUNTIME "):])
                            verified = (provenance.get("engine") == "qwen"
                                        and provenance.get("mode") == "decode_only"
                                        and provenance.get("revision") == "e2afc2b-desktop-v1"
                                        and provenance.get("weights_sha256") == MODEL_SHA256)
                        except (ValueError, AttributeError):
                            verified = False
                    if not announced and line.strip() == b"MODEL_LOADED":
                        announced = True
                        signals.put_nowait(verified)
            finally:
                if not announced:
                    try:
                        signals.put_nowait(False)
                    except queue.Full:
                        pass

        threading.Thread(target=drain_stderr, name="rma-qwen-stderr", daemon=True).start()
        try:
            ready = signals.get(timeout=110)
        except queue.Empty:
            self.stop()
            raise ServiceError("rma_asr_start_timeout", "RMA Qwen did not finish loading and prewarming.")
        if not ready or process.poll() is not None:
            self.stop()
            raise ServiceError("rma_asr_start_failed", "RMA Qwen failed its startup or model checks.")
        self.ready = True

    def transcribe(self, pcm: bytes) -> dict:
        if not pcm or len(pcm) > MAX_PCM_BYTES or len(pcm) % 2:
            raise ServiceError("unsupported_audio", "Expected at most 30 seconds of 16 kHz mono PCM16.")
        self.start()
        assert self.process and self.process.stdin and self.process.stdout
        try:
            pending = memoryview(struct.pack("<I", len(pcm)) + pcm)
            while pending:
                written = self.process.stdin.write(pending)
                if not written:
                    raise BrokenPipeError("RMA Qwen input closed")
                pending = pending[written:]
            self.process.stdin.flush()
            line = self.process.stdout.readline(MAX_RESPONSE_BYTES + 1)
            if not line or len(line) > MAX_RESPONSE_BYTES or not line.endswith(b"\n"):
                raise ValueError("Invalid or oversized response")
            result = json.loads(line)
            if not isinstance(result, dict) or result.get("error"):
                raise ValueError("RMA Qwen inference failed")
            text = result.get("text")
            if not isinstance(text, str) or len(text) > 4000:
                raise ValueError("Invalid transcript")
            return result
        except (OSError, ValueError, json.JSONDecodeError) as exc:
            self.stop()
            raise ServiceError("rma_asr_failed", "RMA Qwen could not complete this transcription.") from exc

    def stop(self) -> None:
        process, self.process = self.process, None
        self.ready = False
        if process is None:
            return
        if process.poll() is None:
            try:
                process.terminate()
                process.wait(timeout=3)
            except ProcessLookupError:
                pass
            except subprocess.TimeoutExpired:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                process.wait()
        for stream in (process.stdin, process.stdout, process.stderr):
            if stream:
                stream.close()
