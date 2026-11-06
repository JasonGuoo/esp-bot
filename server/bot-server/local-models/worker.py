#!/usr/bin/env python3
"""Offline local model adapter. The Rust supervisor owns admission and cancellation.

One bounded JSON object per line in each direction:
  {"id":"r1","op":"health","payload":{}}
  {"id":"r1","ok":true,"result":{...}}

No microphones/cameras are opened here. No models or packages are downloaded.
Raw input media stay in RAM. Model implementation comes from the upstream
libraries in requirements.txt, not a project-owned inference framework.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import gc
import importlib.metadata
import importlib.util
import io
import json
import math
import os
import platform
import re
import signal
import subprocess
import sys
import tempfile
import time
import wave
from pathlib import Path
from typing import Any

from model_store import ModelStore, ServiceError

PROTOCOL_VERSION = 1
MAX_LINE_BYTES = 8 * 1024 * 1024
MAX_AUDIO_SECONDS = 30
SAMPLE_RATE = 16000
MAX_WAV_BYTES = SAMPLE_RATE * 2 * MAX_AUDIO_SECONDS + 65536
MAX_IMAGE_BYTES = 3 * 1024 * 1024
MAX_IMAGE_PIXELS = 4 * 1024 * 1024
MAX_TTS_CHARACTERS = 1000
MAX_TTS_SECONDS = 120
MAX_TTS_WAV_BYTES = SAMPLE_RATE * 2 * MAX_TTS_SECONDS + 65536


def fail(code: str, message: str) -> None:
    raise ServiceError(code, message)


def fields(payload: dict, allowed: set[str]) -> None:
    if set(payload) - allowed:
        fail("invalid_payload", "Unsupported payload field for this operation.")


def decode_base64(value: Any, maximum: int) -> bytes:
    if not isinstance(value, str) or not value:
        fail("invalid_media", "Supply a nonempty base64 media value, without a data URL prefix.")
    if len(value) > 4 * ((maximum + 2) // 3):
        fail("media_too_large", "Encoded media exceeds the operation limit.")
    try:
        raw = base64.b64decode(value, validate=True)
    except (ValueError, binascii.Error) as exc:
        raise ServiceError("invalid_base64", "Media is not valid base64.") from exc
    if not raw or len(raw) > maximum:
        fail("media_too_large", "Decoded media exceeds the operation limit.")
    return raw


def pcm_from_wav(raw: bytes, maximum_seconds: int = MAX_AUDIO_SECONDS) -> tuple[bytes, int]:
    """Accept a bounded, complete WAV container; never invoke ffmpeg on input."""
    if raw[:4] != b"RIFF" or raw[8:12] != b"WAVE":
        fail("unsupported_audio", "Expected a PCM WAV container.")
    try:
        with wave.open(io.BytesIO(raw), "rb") as wav:
            if (
                wav.getnchannels() != 1
                or wav.getsampwidth() != 2
                or wav.getframerate() != SAMPLE_RATE
                or wav.getcomptype() != "NONE"
            ):
                fail("unsupported_audio", "Audio must be 16kHz, mono, signed PCM16 little-endian WAV.")
            count = wav.getnframes()
            if count <= 0 or count > SAMPLE_RATE * maximum_seconds:
                fail("audio_duration_invalid", f"Audio must be no longer than {maximum_seconds} seconds.")
            pcm = wav.readframes(count + 1)
            if len(pcm) != count * 2:
                fail("audio_truncated", "WAV samples do not match the declared frame count.")
    except (wave.Error, EOFError, OSError) as exc:
        raise ServiceError("invalid_audio", "Cannot decode the PCM WAV container.") from exc
    return pcm, count


def installed(module: str) -> bool:
    try:
        return importlib.util.find_spec(module) is not None
    except (ImportError, ValueError):
        return False


def package_version(distribution: str) -> str | None:
    try:
        return importlib.metadata.version(distribution)
    except importlib.metadata.PackageNotFoundError:
        return None


def finite_number(value: Any) -> float | None:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return None
    number = float(value)
    return number if math.isfinite(number) else None


class Worker:
    def __init__(self, model_dir: Path, default_voice: str, tts_rate: int):
        self.store = ModelStore(model_dir)
        self.default_voice = default_voice
        self.tts_rate = tts_rate
        self.speaker = None
        self.detector = None
        self.recognizer = None
        self.asr_loaded = False
        self.voice_inventory: list[dict] | None = None

    def health(self) -> dict:
        apple_silicon = sys.platform == "darwin" and platform.machine() == "arm64"
        descriptions = {
            "asr": ("mlx_whisper", apple_silicon, ["mlx_whisper", "numpy"], self.asr_loaded),
            "speaker": ("speechbrain_ecapa_cpu", True, ["speechbrain", "torch", "torchaudio", "numpy"], self.speaker is not None),
            "face": ("opencv_yunet_sface_cpu", True, ["cv2", "PIL", "numpy"], self.recognizer is not None),
        }
        models = {}
        for kind, (backend, supported, dependencies, loaded) in descriptions.items():
            status = self.store.status(kind)
            missing = [name for name in dependencies if not installed(name)]
            status.update({
                "backend": backend,
                "supported": supported,
                "missing_dependencies": missing,
                "loaded": loaded,
                # This means configured for an attempted call, not hardware validated.
                "available": supported and not missing and status["installed"],
            })
            models[kind] = status
        tts_supported = sys.platform == "darwin" and Path("/usr/bin/say").is_file()
        models["tts"] = {
            "backend": "macos_say",
            "supported": tts_supported,
            "available": tts_supported,
            "default_voice": self.default_voice,
            "voice_inventory_checked": self.voice_inventory is not None,
            "voice_count": len(self.voice_inventory) if self.voice_inventory is not None else None,
            "loaded": False,
        }
        return {
            "protocol_version": PROTOCOL_VERSION,
            "worker_version": "0.1.0",
            "offline": True,
            "capture_owned_by_worker": False,
            "platform": sys.platform,
            "architecture": platform.machine(),
            "models": models,
            "packages": {name: package_version(name) for name in (
                "mlx-whisper", "speechbrain", "opencv-python-headless", "torch", "torchaudio", "numpy", "Pillow"
            )},
            "limits": {
                "json_line_bytes": MAX_LINE_BYTES,
                "audio_seconds": MAX_AUDIO_SECONDS,
                "audio_sample_rate": SAMPLE_RATE,
                "image_bytes": MAX_IMAGE_BYTES,
                "image_pixels": MAX_IMAGE_PIXELS,
                "speaker_min_seconds": 3,
                "tts_characters": MAX_TTS_CHARACTERS,
                "tts_seconds": MAX_TTS_SECONDS,
            },
        }

    def _audio(self, payload: dict, *, speaker: bool = False):
        import numpy as np

        raw = decode_base64(payload.get("wav_base64"), MAX_WAV_BYTES)
        pcm, count = pcm_from_wav(raw)
        seconds = count / SAMPLE_RATE
        if seconds < (3.0 if speaker else 0.25):
            fail("audio_too_short", "Use at least 3 seconds for speaker samples, or 250ms for transcription.")
        audio = np.frombuffer(pcm, dtype="<i2").astype(np.float32) / 32768.0
        clipped = float(np.mean(np.abs(audio) >= 0.999))
        dc_offset = float(np.mean(audio, dtype=np.float64))
        audio -= dc_offset
        rms = float(np.sqrt(np.mean(audio * audio, dtype=np.float64)))
        dbfs = 20.0 * math.log10(max(rms, 1e-12))
        frames = audio[: (count // 320) * 320].reshape(-1, 320)
        frame_rms = np.sqrt(np.mean(frames * frames, axis=1, dtype=np.float64))
        activity = float(np.mean(frame_rms >= 10 ** (-42.0 / 20.0)))
        if dbfs < (-42.0 if speaker else -55.0) or activity < (0.15 if speaker else 0.02):
            fail("audio_too_quiet", "The audio did not pass the minimum signal-energy gate.")
        if clipped > (0.02 if speaker else 0.10):
            fail("audio_clipped", "The audio has too many clipped samples.")
        quality = {
            "duration_ms": round(seconds * 1000),
            "rms_dbfs": round(dbfs, 3),
            "clipped_fraction": round(clipped, 6),
            "removed_dc_offset": round(dc_offset, 6),
            "active_energy_fraction": round(activity, 6),
            "screening": "energy_gate_only",
            "single_speaker_verified": False,
        }
        return audio, quality

    @staticmethod
    def _embedding(values, expected_dimension: int) -> list[float]:
        import numpy as np

        vector = np.asarray(values, dtype=np.float32).reshape(-1)
        if vector.size != expected_dimension or not np.isfinite(vector).all():
            fail("invalid_embedding", "Model returned an unexpected embedding shape or nonfinite values.")
        length = float(np.linalg.norm(vector))
        if not math.isfinite(length) or length < 1e-8:
            fail("invalid_embedding", "Model returned an unusable embedding.")
        return (vector / length).tolist()

    def transcribe(self, payload: dict) -> dict:
        fields(payload, {"wav_base64", "language"})
        if sys.platform != "darwin" or platform.machine() != "arm64":
            fail("unsupported_platform", "The selected ASR adapter requires native Apple Silicon Python.")
        directory = self.store.require("asr")
        audio, quality = self._audio(payload)
        import mlx_whisper
        from mlx_whisper.tokenizer import LANGUAGES
        from mlx_whisper.transcribe import ModelHolder

        language = payload.get("language", "zh")
        if language == "auto":
            language = None
        if language is not None and (not isinstance(language, str) or language not in LANGUAGES):
            fail("unsupported_language", "Use a Whisper language code, 'auto', or null.")
        # Official Apple example API. Passing a verified LOCAL directory prevents
        # load_model from taking its Hugging Face snapshot_download branch.
        try:
            result = mlx_whisper.transcribe(
                audio,
                path_or_hf_repo=str(directory),
                language=language,
                task="transcribe",
                temperature=0.0,
                condition_on_previous_text=False,
                no_speech_threshold=0.6,
                logprob_threshold=-1.0,
                word_timestamps=False,
                verbose=None,
            )
        finally:
            # Failed decoding can still leave the upstream model cache loaded.
            self.asr_loaded = ModelHolder.model is not None
        text = str(result.get("text", "")).strip()
        if not text:
            fail("no_speech", "The ASR model returned no speech text.")
        if len(text) > 4000:
            fail("asr_output_too_long", "The ASR result exceeds the conversation text limit.")
        segments = []
        for segment in result.get("segments", [])[:128]:
            start = finite_number(segment.get("start"))
            end = finite_number(segment.get("end"))
            segments.append({
                "start_ms": round(start * 1000) if start is not None else None,
                "end_ms": round(end * 1000) if end is not None else None,
                "text": str(segment.get("text", ""))[:4000],
                "avg_logprob": finite_number(segment.get("avg_logprob")),
                "no_speech_prob": finite_number(segment.get("no_speech_prob")),
            })
        return {
            "text": text,
            "language": result.get("language", language),
            "model_id": self.store.spec("asr")["model_id"],
            "segments": segments,
            "quality": quality,
            "streaming": False,
        }

    def speaker_embed(self, payload: dict) -> dict:
        fields(payload, {"wav_base64"})
        directory = self.store.require("speaker")
        audio, quality = self._audio(payload, speaker=True)
        import torch

        if self.speaker is None:
            from speechbrain.inference.classifiers import EncoderClassifier
            from speechbrain.utils.fetching import FetchConfig, LocalStrategy

            torch.set_num_threads(4)
            # Official SpeechBrain example with an explicitly offline local source.
            # The YAML's pretrained_path also becomes local, so every checkpoint
            # resolves to the pinned bundle instead of a remote repository name.
            self.speaker = EncoderClassifier.from_hparams(
                source=str(directory),
                savedir=str(directory),
                overrides={"pretrained_path": str(directory)},
                run_opts={"device": "cpu"},
                local_strategy=LocalStrategy.NO_LINK,
                fetch_config=FetchConfig(allow_network=False, allow_updates=False),
            )
        with torch.inference_mode():
            value = self.speaker.encode_batch(torch.from_numpy(audio).unsqueeze(0), normalize=False)
            embedding = self._embedding(value.detach().cpu().numpy(), 192)
        return {
            "embedding": embedding,
            "model_id": self.store.spec("speaker")["model_id"],
            "dimension": len(embedding),
            "quality_ok": True,
            "quality": quality,
            "modality": "speaker",
            "identity_claim": False,
        }

    def face_embed(self, payload: dict) -> dict:
        fields(payload, {"image_base64"})
        directory = self.store.require("face")
        raw = decode_base64(payload.get("image_base64"), MAX_IMAGE_BYTES)
        import cv2 as cv
        import numpy as np
        from PIL import Image, ImageOps

        Image.MAX_IMAGE_PIXELS = MAX_IMAGE_PIXELS
        try:
            with Image.open(io.BytesIO(raw)) as opened:
                if opened.format not in {"JPEG", "PNG"} or getattr(opened, "n_frames", 1) != 1:
                    fail("unsupported_image", "Use a single JPEG or PNG frame.")
                width, height = opened.size
                if min(width, height) < 96 or max(width, height) > 4096 or width * height > MAX_IMAGE_PIXELS:
                    fail("image_dimensions_invalid", "Image dimensions exceed the permitted range.")
                # Header dimensions are checked BEFORE a full image is decompressed.
                oriented = ImageOps.exif_transpose(opened)
                image = np.asarray(oriented.convert("RGB"))[:, :, ::-1].copy()
        except ServiceError:
            raise
        except (OSError, ValueError, Image.DecompressionBombError) as exc:
            raise ServiceError("invalid_image", "Cannot decode this bounded JPEG/PNG image.") from exc
        original_height, original_width = image.shape[:2]
        scale = min(1.0, 960.0 / max(original_width, original_height))
        if scale < 1.0:
            image = cv.resize(image, (round(original_width * scale), round(original_height * scale)), interpolation=cv.INTER_AREA)
        height, width = image.shape[:2]
        scale_x, scale_y = width / original_width, height / original_height
        if self.detector is None:
            cv.setNumThreads(2)
            # These calls follow OpenCV Zoo's YuNet/SFace examples. Alignment and
            # neural network inference remain owned by OpenCV.
            detector = cv.FaceDetectorYN.create(
                model=str(directory / "face_detection_yunet_2023mar.onnx"),
                config="",
                input_size=(width, height),
                score_threshold=0.8,
                nms_threshold=0.3,
                top_k=5000,
                backend_id=cv.dnn.DNN_BACKEND_OPENCV,
                target_id=cv.dnn.DNN_TARGET_CPU,
            )
            recognizer = cv.FaceRecognizerSF.create(
                model=str(directory / "face_recognition_sface_2021dec.onnx"),
                config="",
                backend_id=cv.dnn.DNN_BACKEND_OPENCV,
                target_id=cv.dnn.DNN_TARGET_CPU,
            )
            self.detector, self.recognizer = detector, recognizer
        self.detector.setInputSize((width, height))
        _, faces = self.detector.detect(image)
        count = 0 if faces is None else len(faces)
        if count == 0:
            fail("no_face", "No usable face was detected.")
        if count != 1:
            fail("multiple_faces", "Use a frame containing exactly one person for identity comparison.")
        face = faces[0]
        if len(face) != 15 or not np.isfinite(face).all():
            fail("face_geometry_invalid", "The detector returned invalid face geometry.")
        x, y, box_width, box_height = map(float, face[:4])
        score = float(face[14])
        if score < 0.90 or min(box_width, box_height) < 80:
            fail("face_quality_low", "Move closer and use a clear, well-lit face image.")
        if x < 0 or y < 0 or x + box_width > width or y + box_height > height:
            fail("face_partial", "The whole face must be inside the image.")
        points = np.asarray(face[4:14]).reshape(5, 2)
        if (points[:, 0] < 0).any() or (points[:, 0] >= width).any() or (points[:, 1] < 0).any() or (points[:, 1] >= height).any():
            fail("face_geometry_invalid", "Facial landmarks are outside the image.")
        if float(np.linalg.norm(points[0] - points[1])) < 16:
            fail("face_quality_low", "The eye landmarks are too close for a useful sample.")
        crop = image[int(y): math.ceil(y + box_height), int(x): math.ceil(x + box_width)]
        gray = cv.cvtColor(crop, cv.COLOR_BGR2GRAY)
        brightness = float(np.mean(gray))
        contrast = float(np.std(gray))
        sharpness = float(cv.Laplacian(gray, cv.CV_64F).var())
        if brightness < 30 or brightness > 225 or contrast < 15 or sharpness < 50:
            fail("face_quality_low", "Use a sharper image with even lighting.")
        aligned = self.recognizer.alignCrop(image, face)
        embedding = self._embedding(self.recognizer.feature(aligned), 128)
        return {
            "embedding": embedding,
            "model_id": self.store.spec("face")["model_id"],
            "dimension": len(embedding),
            "quality_ok": True,
            "modality": "face",
            "identity_claim": False,
            "face_count": 1,
            "bbox": {"x": x / scale_x, "y": y / scale_y, "width": box_width / scale_x, "height": box_height / scale_y},
            "image_size": {"width": original_width, "height": original_height},
            "quality": {
                "detector_score": score,
                "brightness_mean": brightness,
                "contrast_stddev": contrast,
                "laplacian_variance": sharpness,
                "screening": "single_detected_face_and_image_quality",
                "liveness_verified": False,
            },
        }

    def voices(self) -> dict:
        if sys.platform != "darwin" or not Path("/usr/bin/say").is_file():
            fail("unsupported_platform", "macOS speech synthesis is unavailable.")
        if self.voice_inventory is None:
            # `say -v ?` lists INSTALLED voices. Never initiate a voice download.
            process = subprocess.Popen(["/usr/bin/say", "-v", "?"], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            try:
                output, _ = process.communicate(timeout=10)
            except subprocess.TimeoutExpired as exc:
                process.kill()
                process.communicate()
                raise ServiceError("tts_voice_timeout", "Installed voice enumeration timed out.") from exc
            if process.returncode != 0 or len(output) > 262144:
                fail("tts_voice_unavailable", "Cannot enumerate installed macOS voices.")
            voices = []
            for line in output.decode("utf-8", errors="replace").splitlines():
                match = re.match(r"^(.*?)\s+([A-Za-z]{2,3}[_-][A-Za-z]{2,4})\s+#", line)
                if match:
                    voices.append({"name": match[1].strip(), "language": match[2]})
            self.voice_inventory = voices[:500]
        return {"voices": self.voice_inventory, "installed_only": True}

    def tts(self, payload: dict) -> dict:
        fields(payload, {"text", "voice", "rate"})
        text = payload.get("text")
        if not isinstance(text, str) or not text.strip() or len(text) > MAX_TTS_CHARACTERS:
            fail("tts_text_invalid", f"Supply between 1 and {MAX_TTS_CHARACTERS} text characters.")
        if any(ord(char) < 32 and char not in "\n\t\r" for char in text):
            fail("tts_text_invalid", "Speech text contains unsupported control characters.")
        # `say` has [[...]] control directives. They must be spoken literally,
        # never interpreted from a model response or an external web page.
        text = text.replace("[", "［").replace("]", "］")
        try:
            encoded = text.encode("utf-8")
        except UnicodeEncodeError as exc:
            raise ServiceError("tts_text_invalid", "Speech text contains invalid Unicode.") from exc
        rate = payload.get("rate", self.tts_rate)
        if type(rate) is not int or rate < 100 or rate > 260:
            fail("tts_rate_invalid", "Speech rate must be an integer between 100 and 260.")
        available = self.voices()["voices"]
        voice = payload.get("voice")
        if voice is None:
            voice = self.default_voice
        if not isinstance(voice, str) or len(voice) > 128:
            fail("tts_voice_unavailable", "Select an installed macOS voice.")
        if voice not in {item["name"] for item in available}:
            # A missing configured Mandarin voice is surfaced, not replaced with
            # a different language or a silently downloaded voice.
            fail("tts_voice_unavailable", "The selected voice is not installed; choose one from voices.")
        with tempfile.TemporaryDirectory(prefix="xiaodai-speech-") as directory:
            output = Path(directory) / "speech.wav"
            # A pipe rather than PID polling detects parent death without PID
            # reuse races. The guard also kills `say` when the worker is SIGKILLed.
            alive_read, alive_write = os.pipe()
            process = None
            try:
                process = subprocess.Popen(
                    [sys.executable, "-u", str(Path(__file__).with_name("say_guard.py")), str(alive_read), str(output), voice, str(rate)],
                    stdin=subprocess.PIPE,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    pass_fds=(alive_read,),
                    start_new_session=True,
                )
                os.close(alive_read)
                alive_read = -1
                try:
                    process.communicate(input=encoded, timeout=MAX_TTS_SECONDS + 10)
                except subprocess.TimeoutExpired as exc:
                    os.close(alive_write)
                    alive_write = -1
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    raise ServiceError("tts_timeout", "Local speech synthesis exceeded its time budget.") from exc
                if process.returncode == 3:
                    fail("tts_timeout", "Local speech synthesis exceeded its time budget.")
                if process.returncode == 4:
                    fail("tts_output_too_large", "Split this reply into shorter speech segments.")
                if process.returncode != 0 or not output.is_file():
                    fail("tts_failed", "The installed macOS voice could not synthesize a bounded WAV file.")
                if output.stat().st_size > MAX_TTS_WAV_BYTES:
                    fail("tts_output_too_large", "Split this reply into shorter speech segments.")
                wav = output.read_bytes()
                _, frames = pcm_from_wav(wav, MAX_TTS_SECONDS)
            finally:
                if alive_read >= 0:
                    os.close(alive_read)
                if alive_write >= 0:
                    os.close(alive_write)
                if process is not None and process.poll() is None:
                    # Closing the liveness pipe asks the guard to stop and reap
                    # its speech child even while Python exits on SIGTERM.
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
        return {
            "wav_base64": base64.b64encode(wav).decode("ascii"),
            "sample_rate": SAMPLE_RATE,
            "channels": 1,
            "format": "pcm_s16le",
            "container": "wav",
            "duration_ms": round(frames * 1000 / SAMPLE_RATE),
            "voice": voice,
            "backend": "macos_say",
            "streaming": False,
        }

    def unload(self) -> dict:
        self.speaker = None
        self.detector = None
        self.recognizer = None
        if self.asr_loaded:
            import mlx.core as mx
            from mlx_whisper.transcribe import ModelHolder

            ModelHolder.model = None
            ModelHolder.model_path = None
            gc.collect()
            if hasattr(mx, "clear_cache"):
                mx.clear_cache()
            elif hasattr(mx, "metal") and hasattr(mx.metal, "clear_cache"):
                mx.metal.clear_cache()
        self.asr_loaded = False
        self.store.verified.clear()
        gc.collect()
        return {"unloaded": True}

    def dispatch(self, op: str, payload: dict) -> dict:
        if op in {"health", "unload", "voices"}:
            fields(payload, set())
        handlers = {
            "health": lambda _: self.health(),
            "transcribe": self.transcribe,
            "speaker_embed": self.speaker_embed,
            "face_embed": self.face_embed,
            "tts": self.tts,
            "voices": lambda _: self.voices(),
            "unload": lambda _: self.unload(),
        }
        handler = handlers.get(op)
        if handler is None:
            fail("unsupported_operation", "Unknown local-model operation.")
        return handler(payload)


def reject_constant(_: str):
    fail("invalid_json", "JSON nonfinite constants are not accepted.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--tts-voice", default="Tingting")
    parser.add_argument("--tts-rate", type=int, default=180)
    args = parser.parse_args()
    if not 100 <= args.tts_rate <= 260:
        parser.error("--tts-rate must be between 100 and 260")
    os.umask(0o077)
    # These are defense in depth; all adapters ALSO use verified local paths.
    for key in ("HF_HUB_OFFLINE", "HF_DATASETS_OFFLINE", "TRANSFORMERS_OFFLINE", "HF_HUB_DISABLE_TELEMETRY", "DO_NOT_TRACK"):
        os.environ[key] = "1"
    os.environ["TOKENIZERS_PARALLELISM"] = "false"
    for key in ("OMP_NUM_THREADS", "MKL_NUM_THREADS", "OPENBLAS_NUM_THREADS"):
        os.environ.setdefault(key, "4")

    # Third-party Python/native output is suppressed, so even a library that
    # prints recognized text cannot leak media/transcripts into logs or JSONL.
    protocol = os.fdopen(os.dup(sys.stdout.fileno()), "w", encoding="utf-8", buffering=1)
    diagnostics = os.fdopen(os.dup(sys.stderr.fileno()), "w", encoding="utf-8", buffering=1)
    null_fd = os.open(os.devnull, os.O_WRONLY)
    os.dup2(null_fd, sys.stdout.fileno())
    os.dup2(null_fd, sys.stderr.fileno())
    os.close(null_fd)

    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        worker = Worker(args.model_dir, args.tts_voice, args.tts_rate)
    except Exception as exc:
        diagnostics.write(f"local-models startup_failed type={type(exc).__name__}\n")
        return 1

    while True:
        line = sys.stdin.buffer.readline(MAX_LINE_BYTES + 1)
        if not line:
            return 0
        request_id = None
        op = "invalid"
        fatal = len(line) > MAX_LINE_BYTES
        started = time.monotonic()
        try:
            if fatal:
                fail("request_too_large", "JSONL request exceeds the 8MiB limit; restart the worker.")
            request = json.loads(line.decode("utf-8"), parse_constant=reject_constant)
            if not isinstance(request, dict) or set(request) - {"id", "op", "payload"}:
                fail("invalid_request", "Expected an object containing id, op and payload.")
            candidate_id = request.get("id")
            if not isinstance(candidate_id, str) or not re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", candidate_id):
                fail("invalid_request_id", "Request id must be 1-128 safe ASCII characters.")
            request_id = candidate_id
            candidate_op = request.get("op")
            if not isinstance(candidate_op, str) or not re.fullmatch(r"[a-z_]{1,32}", candidate_op):
                fail("invalid_operation", "Invalid operation name.")
            op = candidate_op
            payload = request.get("payload", {})
            if not isinstance(payload, dict):
                fail("invalid_payload", "Payload must be a JSON object.")
            result = worker.dispatch(op, payload)
            response = {"id": request_id, "ok": True, "result": result}
        except ServiceError as exc:
            response = {"id": request_id, "ok": False, "error": {"code": exc.code, "message": exc.message}}
        except (UnicodeError, json.JSONDecodeError, RecursionError):
            response = {"id": request_id, "ok": False, "error": {"code": "invalid_json", "message": "Request is not a bounded UTF-8 JSON object."}}
        except (ImportError, ModuleNotFoundError) as exc:
            diagnostics.write(f"local-models id={request_id or '-'} op={op} dependency_import_failed type={type(exc).__name__}\n")
            response = {"id": request_id, "ok": False, "error": {"code": "dependency_unavailable", "message": "A local inference dependency is missing or incompatible; install requirements.txt in this Python environment."}}
        except Exception as exc:
            # Never stringify arbitrary library exceptions: they can contain
            # input text, tensors, environment variables or private paths.
            diagnostics.write(f"local-models id={request_id or '-'} op={op} inference_failed type={type(exc).__name__}\n")
            response = {"id": request_id, "ok": False, "error": {"code": "inference_failed", "message": "Local inference failed; inspect dependency versions and the model installation status."}}
        encoded = json.dumps(response, ensure_ascii=False, allow_nan=False, separators=(",", ":"))
        if len(encoded.encode("utf-8")) + 1 > MAX_LINE_BYTES:
            encoded = json.dumps({"id": request_id, "ok": False, "error": {"code": "response_too_large", "message": "Local-model response exceeded the JSONL limit."}})
        try:
            protocol.write(encoded + "\n")
            protocol.flush()
        except BrokenPipeError:
            return 0
        code = "ok" if response["ok"] else response["error"]["code"]
        diagnostics.write(f"local-models id={request_id or '-'} op={op} result={code} elapsed_ms={round((time.monotonic() - started) * 1000)}\n")
        # Input/response references are not retained as conversation history.
        line = b""
        response = None
        request = None
        result = None
        payload = None
        encoded = ""
        if fatal:
            return 2


if __name__ == "__main__":
    raise SystemExit(main())
