#!/usr/bin/env python3
"""Explicit real-model acceptance; never downloads or changes App settings."""
import argparse
import base64
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import time
import wave

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--python", type=Path, required=True)
parser.add_argument("--model", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
os.umask(0o077)
args.output.mkdir(parents=True, exist_ok=False)
root = Path(__file__).resolve().parents[1]
worker = root / "server/bot-server/local-models/worker.py"
texts = ["我在呢。我们慢慢来，我陪你一起想办法。",
         "现在是下午三点半，十分钟后提醒你喝水。Apple 是苹果。"]
requests = [{"id": str(i), "op": "tts", "payload": {"text": text}} for i, text in enumerate(texts)]
requests.append({"id": "health", "op": "health", "payload": {}})
started = time.monotonic()
with (args.output / "worker.log").open("wb") as log:
    child = subprocess.Popen([str(args.python), "-B", "-u", str(worker),
        "--model-dir", str(args.model), "--tts-backend", "qwen3_robot",
        "--qwen3-model-dir", str(args.model)], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=log, start_new_session=True)
    try:
        stdout, _ = child.communicate(input=("\n".join(json.dumps(r, ensure_ascii=False) for r in requests)+"\n").encode(), timeout=180)
        assert child.returncode == 0, "worker failed; see worker.log"
    finally:
        # Includes ffmpeg: same ownership rule as the production Rust supervisor.
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
rows = [json.loads(line) for line in stdout.splitlines()]
assert len(rows) == len(requests)
evidence = []
for index, row in enumerate(rows[:-1]):
    assert row["id"] == str(index) and row["ok"], row.get("error")
    result = row["result"]
    assert result["backend"] == "qwen3_mlx_robot" and not result.get("fallback_reason"), "unexpected system-voice fallback"
    wav = base64.b64decode(result.pop("wav_base64"), validate=True)
    with wave.open(io.BytesIO(wav), "rb") as stream:
        assert (stream.getframerate(), stream.getnchannels(), stream.getsampwidth()) == (16000, 1, 2)
        assert 1600 < stream.getnframes() < 16000 * 35
        assert any(stream.readframes(stream.getnframes()))
    (args.output / f"robot-{index+1}.wav").write_bytes(wav)
    evidence.append({"text": texts[index], **result})
health = rows[-1]["result"]["models"]["tts"]
assert health["loaded"] and health["verified"] and health["backend"] == "qwen3_mlx_robot"
report = {"status": "passed", "elapsed_seconds": time.monotonic()-started,
          "health": health, "samples": evidence, "device_acceptance": "not_run"}
(args.output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2)+"\n")
print(f"Real Qwen robot worker passed; {len(evidence)} samples; {report['elapsed_seconds']:.2f}s total; S31 not_run")
