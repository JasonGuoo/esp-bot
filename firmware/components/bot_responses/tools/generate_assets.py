#!/usr/bin/env python3
"""Generate real local Chinese speech and an embedded IMA ADPCM catalogue.

The model runs on the development computer only. Firmware needs neither a TTS
model nor a synthesis service. No model downloads or API requests occur here.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import struct
import subprocess
import sys
import wave
from pathlib import Path

COMPONENT = Path(__file__).resolve().parents[1]
REPOSITORY = COMPONENT.parents[2]
MODEL_URL = "https://huggingface.co/hexgrad/Kokoro-82M-v1.1-zh"
PACKAGE_URL = ("https://github.com/k2-fsa/sherpa-onnx/releases/download/"
               "tts-models/kokoro-int8-multi-lang-v1_1.tar.bz2")
BLOCK_BYTES = 256
SAMPLE_RATE = 16000
SCENES = {"general", "morning", "day", "evening", "night", "waking", "dark", "offline", "repeat", "no_face"}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def run(arguments: list[str]) -> None:
    subprocess.run(arguments, check=True, stdout=subprocess.DEVNULL)


def read_ima_wav(path: Path) -> bytes:
    raw = path.read_bytes()
    if raw[:4] != b"RIFF" or raw[8:12] != b"WAVE":
        raise ValueError(f"Not RIFF WAVE: {path}")
    offset, fmt, payload = 12, None, None
    while offset + 8 <= len(raw):
        kind, length = struct.unpack_from("<4sI", raw, offset)
        offset += 8
        if offset + length > len(raw):
            raise ValueError(f"Truncated RIFF chunk: {path}")
        chunk = raw[offset:offset + length]
        if kind == b"fmt ":
            fmt = chunk
        elif kind == b"data":
            payload = chunk
        offset += length + (length & 1)
    if fmt is None or len(fmt) < 20 or payload is None:
        raise ValueError(f"Missing ADPCM WAVE data: {path}")
    encoding, channels, rate, _, alignment, bits = struct.unpack_from("<HHIIHH", fmt)
    if (encoding, channels, rate, alignment, bits) != (0x11, 1, SAMPLE_RATE, BLOCK_BYTES, 4):
        raise ValueError(f"Unexpected IMA format: {path}")
    if struct.unpack_from("<H", fmt, 18)[0] != 505 or len(payload) % BLOCK_BYTES:
        raise ValueError(f"Unexpected IMA block layout: {path}")
    for position in range(0, len(payload), BLOCK_BYTES):
        if payload[position + 2] > 88 or payload[position + 3] != 0:
            raise ValueError(f"Invalid IMA predictor header: {path}")
    return payload


def validate_catalog(catalog: dict) -> list[dict]:
    if catalog.get("schema_version") != 1:
        raise ValueError("Unsupported catalogue schema")
    expressions = json.loads((REPOSITORY / "shared/eve-expressions.json").read_text())
    # Expression file is a structured source document; collect IDs without
    # requiring LVGL, firmware imports or a synthesis engine to validate text.
    def ids(value):
        if isinstance(value, dict):
            if isinstance(value.get("id"), str):
                yield value["id"]
            for item in value.values():
                yield from ids(item)
        elif isinstance(value, list):
            for item in value:
                yield from ids(item)
    known_expressions = set(ids(expressions))
    entries = catalog["entries"]
    seen = set()
    for entry in entries:
        if entry["id"] in seen or not entry["id"].replace("_", "").isalnum():
            raise ValueError(f"Duplicate/invalid response ID: {entry['id']}")
        seen.add(entry["id"])
        if entry["scene"] not in SCENES or entry["expression"] not in known_expressions:
            raise ValueError(f"Unknown scene/expression: {entry['id']}")
        if not entry["text"] or len(entry["text"].encode("utf-8")) > 180:
            raise ValueError(f"Empty/oversized response: {entry['id']}")
        if not 0 <= entry["gain_percent"] <= 100 or not 0.8 <= entry["tts_speed"] <= 1.3:
            raise ValueError(f"Invalid gain/speech rate: {entry['id']}")
        if entry["scene"] == "night" and entry["gain_percent"] > 35:
            raise ValueError(f"Night reply must remain quiet: {entry['id']}")
    if not entries or len(entries) > 65535:
        raise ValueError("Catalogue must contain 1..65535 entries")
    return entries


def synthesize(entries: list[dict], args, work: Path) -> dict:
    import numpy as np
    import sherpa_onnx

    model = args.model_dir.resolve()
    weights = model / "model.int8.onnx"
    if not weights.exists():
        weights = model / "model.onnx"
    provenance = {
        "engine": "sherpa-onnx",
        "engine_version": sherpa_onnx.__version__,
        "engine_license": "Apache-2.0 (upstream project; see bundled dependencies separately)",
        "engine_url": "https://github.com/k2-fsa/sherpa-onnx",
        "model": "Kokoro-82M-v1.1-zh",
        "model_url": MODEL_URL,
        "model_license": "Apache-2.0",
        "model_package_url": PACKAGE_URL,
        "weights_filename": weights.name,
        "weights_sha256": sha256(weights),
        "voices_sha256": sha256(model / "voices.bin"),
        "speaker_id": args.speaker_id,
        "speaker_name": args.speaker_name,
        "inference_location": "development computer CPU, offline after model download",
        "cloud_inference_requests": 0,
        "pretrained_model_shipped_in_firmware": False,
        "raw_training_recordings_shipped": False,
    }
    fingerprint = hashlib.sha256(json.dumps(provenance, sort_keys=True).encode()).hexdigest()
    config = sherpa_onnx.OfflineTtsConfig(
        model=sherpa_onnx.OfflineTtsModelConfig(
            kokoro=sherpa_onnx.OfflineTtsKokoroModelConfig(
                model=str(weights), voices=str(model / "voices.bin"),
                tokens=str(model / "tokens.txt"),
                lexicon=f"{model / 'lexicon-us-en.txt'},{model / 'lexicon-zh.txt'}",
            ), num_threads=args.threads, provider="cpu",
        ), max_num_sentences=1,
    )
    if not config.validate():
        raise ValueError("Invalid Kokoro model configuration")
    tts = sherpa_onnx.OfflineTts(config)
    print(f"Loaded {weights.name}; voice {args.speaker_name}; offline CPU synthesis", flush=True)
    for number, entry in enumerate(entries, 1):
        target = work / f"{entry['id']}.source.wav"
        cache = target.with_suffix(".json")
        key = {"text": entry["text"], "speed": entry["tts_speed"], "engine": fingerprint}
        if target.exists() and cache.exists() and json.loads(cache.read_text()) == key:
            continue
        generated = tts.generate(text=entry["text"], sid=args.speaker_id, speed=entry["tts_speed"])
        samples = np.asarray(generated.samples, dtype=np.float32)
        if not len(samples) or not np.all(np.isfinite(samples)) or np.max(np.abs(samples)) < 0.001:
            raise ValueError(f"Empty/non-finite/silent TTS output: {entry['id']}")
        pcm = np.clip(samples * 32767.0, -32768, 32767).astype("<i2")
        with wave.open(str(target), "wb") as output:
            output.setnchannels(1)
            output.setsampwidth(2)
            output.setframerate(generated.sample_rate)
            output.writeframes(pcm.tobytes())
        cache.write_text(json.dumps(key, ensure_ascii=False))
        print(f"[{number:03d}/{len(entries)}] {entry['id']}: {len(samples) / generated.sample_rate:.2f}s", flush=True)
    license_file = model / "LICENSE"
    if license_file.exists():
        (args.output_dir / "licenses").mkdir(parents=True, exist_ok=True)
        shutil.copyfile(license_file, args.output_dir / "licenses/Kokoro-Apache-2.0.txt")
    return provenance


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--model-dir", type=Path, help="Already downloaded official Kokoro v1.1-zh package")
    source.add_argument("--import-wavs", type=Path, help="Use <response-id>.wav from an authorized recorded voice pack")
    parser.add_argument("--provenance-json", type=Path, help="Required with --import-wavs; records source, permission and voice license")
    parser.add_argument("--catalog", type=Path, default=REPOSITORY / "shared/wake-replies.json")
    parser.add_argument("--output-dir", type=Path, default=COMPONENT)
    parser.add_argument("--work-dir", required=True, type=Path, help="Temporary WAV/cache directory outside the repository")
    parser.add_argument("--speaker-id", type=int, default=3)
    parser.add_argument("--speaker-name", default="zf_001")
    parser.add_argument("--threads", type=int, default=2)
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    args.work_dir.mkdir(parents=True, exist_ok=True)
    if shutil.which("ffmpeg") is None:
        raise RuntimeError("ffmpeg is required to resample and encode standard IMA ADPCM")
    catalog = json.loads(args.catalog.read_text())
    entries = validate_catalog(catalog)
    if args.model_dir:
        provenance = synthesize(entries, args, args.work_dir)
    else:
        if not args.provenance_json:
            raise ValueError("--import-wavs requires --provenance-json; never silently attribute third-party recordings")
        provenance = json.loads(args.provenance_json.read_text())
        for entry in entries:
            shutil.copyfile(args.import_wavs / f"{entry['id']}.wav",
                            args.work_dir / f"{entry['id']}.source.wav")

    packed = bytearray()
    index = []
    c_lines = ["/* Generated by tools/generate_assets.py. Do not edit. */",
               "static const bot_response_entry_t k_bot_responses[] = {"]
    for entry in entries:
        source_wav = args.work_dir / f"{entry['id']}.source.wav"
        pcm_wav = args.work_dir / f"{entry['id']}.pcm16.wav"
        ima_wav = args.work_dir / f"{entry['id']}.ima.wav"
        # Trimming removes empty leading/trailing TTS latency but
        # leaves 60/100 ms breath space. Per-scene/user gain remains runtime-owned.
        run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(source_wav),
             "-af", "silenceremove=start_periods=1:start_duration=0.02:start_threshold=-48dB:start_silence=0.06,areverse,silenceremove=start_periods=1:start_duration=0.02:start_threshold=-48dB:start_silence=0.10,areverse",
             "-ac", "1", "-ar", str(SAMPLE_RATE), "-c:a", "pcm_s16le", str(pcm_wav)])
        with wave.open(str(pcm_wav), "rb") as input_wave:
            pcm_samples = input_wave.getnframes()
            if not 0 < pcm_samples <= SAMPLE_RATE * 8:
                raise ValueError(f"Response must contain 0..8 seconds of actual speech: {entry['id']}")
        run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(pcm_wav),
             "-c:a", "adpcm_ima_wav", "-block_size", str(BLOCK_BYTES), str(ima_wav)])
        payload = read_ima_wav(ima_wav)
        offset = len(packed)
        packed.extend(payload)
        duration_ms = (pcm_samples * 1000 + SAMPLE_RATE - 1) // SAMPLE_RATE
        caption_ms = min(8500, max(entry["suggested_caption_ms"], duration_ms + 250))
        audio = {"id": entry["id"], "text": entry["text"], "offset": offset,
                 "bytes": len(payload), "pcm_samples": pcm_samples, "duration_ms": duration_ms,
                 "pcm_wav_sha256": sha256(pcm_wav), "adpcm_sha256": hashlib.sha256(payload).hexdigest()}
        index.append(audio)
        c_strings = [json.dumps(entry[key], ensure_ascii=False) for key in ["id", "text", "expression", "scene"]]
        c_lines.append("    { " + ", ".join(c_strings) +
                       f", {entry['gain_percent']}, {caption_ms}, {offset}U, {len(payload)}U, {pcm_samples}U }},")
    c_lines.append("};")
    ffmpeg_version = subprocess.check_output(["ffmpeg", "-version"], text=True).splitlines()[0]
    manifest = {"schema_version": 1, "catalog_sha256": sha256(args.catalog),
                "audio_file": "wake_replies.adpcm", "codec": "Microsoft IMA ADPCM WAV mono data blocks (RIFF headers removed)",
                "sample_rate": SAMPLE_RATE, "channels": 1, "block_bytes": BLOCK_BYTES,
                "samples_per_full_block": 505, "nibble_order": "low then high",
                "gain_baked_in": False, "ffmpeg_version": ffmpeg_version,
                "reply_count": len(index), "total_bytes": len(packed),
                "total_pcm_samples": sum(item["pcm_samples"] for item in index),
                "audio_sha256": hashlib.sha256(packed).hexdigest(),
                "provenance": provenance, "entries": index}
    # Publish a complete matching set only after all synthesis/encoding succeeds.
    (args.output_dir / "wake_replies.adpcm").write_bytes(packed)
    (args.output_dir / "generated_catalog.inc").write_text("\n".join(c_lines) + "\n")
    (args.output_dir / "assets-manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    print(f"Wrote {len(index)} real speech assets, {len(packed):,} ADPCM bytes, "
          f"{manifest['total_pcm_samples'] / SAMPLE_RATE:.2f} seconds", flush=True)


if __name__ == "__main__":
    main()
