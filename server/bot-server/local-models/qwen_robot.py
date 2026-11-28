"""Local MLX Qwen CustomVoice + the owner's auditioned little-robot effect.

Inference uses upstream mlx-audio, inside the existing supervised TTS lanes.
No downloads, sockets, microphone access or independently detached children.
"""
from __future__ import annotations

import hashlib
import importlib.util
import io
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import time
import wave

from model_store import ServiceError

MODEL_ID = "mlx-community/Qwen3-TTS-12Hz-1.7B-CustomVoice-8bit"
REVISION = "41d3337e8b7f2843a75841595fc14e4b9a7a4b96"
WEIGHTS = {
    "model.safetensors": "b696f4bf45f54497b6de758981e76f1410bd68af05a657477dee466b0202df1a",
    "speech_tokenizer/model.safetensors": "836b7b357f5ea43e889936a3709af68dfe3751881acefe4ecf0dbd30ba571258",
}
VOICE = "Vivian/little-robot-v1"
INSTRUCTION = "自然温暖、轻快亲切的日常聊天语气。"
ROBOT_FILTER = "rubberband=pitch=1.122462:formant=preserved:transients=smooth,aecho=0.9:1.0:6:0.08,alimiter=limit=0.92:level=false"
MAX_TOKENS = 1400
MAX_SECONDS = 120


def ffmpeg_path() -> str | None:
    return shutil.which("ffmpeg") or next((p for p in (
        "/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"
    ) if Path(p).is_file()), None)


def robot_effect(wav: bytes, rate: int = 180) -> bytes:
    """Bounded PCM in/out; run in the worker's process group so Stop owns ffmpeg."""
    from worker import pcm_from_wav
    _, frames = pcm_from_wav(wav, MAX_SECONDS)
    if type(rate) is not int or not 100 <= rate <= 260:
        raise ServiceError("tts_rate_invalid", "Speech rate must be between 100 and 260.")
    if frames / 16000 / (rate / 180) + 0.006 > MAX_SECONDS:
        raise ServiceError("tts_output_too_large", "Split this reply into shorter speech segments.")
    executable = ffmpeg_path()
    if not executable:
        raise ServiceError("tts_effect_unavailable", "Local ffmpeg with rubberband is required.")
    effect = ROBOT_FILTER + (f",atempo={rate / 180:.8f}" if rate != 180 else "")
    with tempfile.TemporaryDirectory(prefix="xiaodai-robot-") as directory:
        output = Path(directory) / "robot.wav"
        try:
            result = subprocess.run([
                executable, "-nostdin", "-hide_banner", "-loglevel", "error", "-y",
                "-i", "pipe:0", "-af", effect, "-ar", "16000", "-ac", "1",
                "-c:a", "pcm_s16le", str(output),
            ], input=wav, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
        except subprocess.TimeoutExpired as exc:
            raise ServiceError("tts_effect_timeout", "The robot effect exceeded its time budget.") from exc
        if result.returncode or not output.is_file():
            raise ServiceError("tts_effect_failed", "The local robot effect could not produce audio.")
        if output.stat().st_size > MAX_SECONDS * 32000 + 65536:
            raise ServiceError("tts_output_too_large", "Split this reply into shorter speech segments.")
        result = output.read_bytes()
    pcm_from_wav(result, MAX_SECONDS)
    return result


class QwenRobot:
    def __init__(self, model_dir: str | None):
        self.directory = Path(model_dir) if model_dir else None
        self.model = None
        self.verified = False

    def status(self) -> dict:
        files = (*WEIGHTS, "config.json", "vocab.json", "merges.txt", "tokenizer_config.json",
                 "speech_tokenizer/config.json", "speech_tokenizer/preprocessor_config.json")
        installed = bool(self.directory and all((self.directory / p).is_file() for p in files))
        dependencies = [name for name in ("mlx", "mlx_audio", "numpy")
                        if importlib.util.find_spec(name) is None]
        supported = sys.platform == "darwin" and platform.machine() == "arm64"
        return {"backend": "qwen3_mlx_robot", "default_voice": VOICE,
                "supported": supported, "installed": installed,
                "missing_dependencies": dependencies, "effect_installed": bool(ffmpeg_path()),
                "available": supported and installed and not dependencies and bool(ffmpeg_path()),
                "loaded": self.model is not None, "verified": self.verified,
                "model_id": MODEL_ID, "revision": REVISION}

    def _load(self):
        if self.model is None:
            if not self.status()["available"]:
                raise ServiceError("tts_robot_unavailable", "Install the local Qwen robot environment and model.")
            for name, expected in WEIGHTS.items():
                with (self.directory / name).open("rb") as source:
                    if hashlib.file_digest(source, "sha256").hexdigest() != expected:
                        raise ServiceError("tts_model_checksum", "The local Qwen model failed integrity verification.")
            self.verified = True
            from mlx_audio.tts.utils import load_model
            self.model = load_model(self.directory)
        return self.model

    @staticmethod
    def collect(chunks, evaluate, *, max_tokens=MAX_TOKENS) -> bytes:
        """Reject empty/invalid/over-budget generations; never return a partial prefix."""
        import numpy as np
        from worker import resample_to_16k
        audio, samples, tokens = [], 0, 0
        started = time.monotonic()
        for chunk in chunks:
            evaluate(chunk.audio)
            values = np.asarray(chunk.audio, dtype=np.float32).reshape(-1)
            if chunk.sample_rate != 24000 or not np.isfinite(values).all():
                raise ServiceError("tts_audio_invalid", "Qwen returned invalid audio.")
            tokens += chunk.token_count
            samples += values.size
            if samples > 24000 * MAX_SECONDS:
                raise ServiceError("tts_output_too_large", "Split this reply into shorter speech segments.")
            if tokens >= max_tokens:
                raise ServiceError("tts_generation_incomplete", "Qwen exhausted its generation budget.")
            if time.monotonic() - started > 90:
                raise ServiceError("tts_timeout", "Local speech synthesis exceeded its time budget.")
            audio.append(values)
        if not samples:
            raise ServiceError("tts_audio_empty", "Qwen produced no audio.")
        pcm = resample_to_16k(np.concatenate(audio), 24000)
        if not np.any(pcm):
            raise ServiceError("tts_audio_empty", "Qwen produced only silence.")
        buffer = io.BytesIO()
        with wave.open(buffer, "wb") as out:
            out.setnchannels(1); out.setsampwidth(2); out.setframerate(16000)
            out.writeframes(pcm.astype("<i2").tobytes())
        return buffer.getvalue()

    def synthesize(self, text: str, rate: int) -> bytes:
        model = self._load()
        import mlx.core as mx
        mx.random.seed(7)
        chunks = model.generate_custom_voice(
            text=text, speaker="Vivian", language="Chinese", instruct=INSTRUCTION,
            stream=True, streaming_interval=0.32, max_tokens=MAX_TOKENS,
            temperature=0.7, verbose=False,
        )
        try:
            wav = self.collect(chunks, mx.eval)
        finally:
            chunks.close()
        return robot_effect(wav, rate)

    def unload(self):
        self.model = None
        if "mlx.core" in sys.modules:
            import gc
            gc.collect()
            sys.modules["mlx.core"].clear_cache()
