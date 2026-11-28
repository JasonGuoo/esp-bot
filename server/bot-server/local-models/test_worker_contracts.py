"""Actual media admission and worker process boundary; no model inference."""
import base64
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import wave

from worker import ServiceError, decode_base64, pcm_from_wav


def wav_bytes(frames=160, channels=1, rate=16000, width=2):
    buffer = io.BytesIO()
    with wave.open(buffer, "wb") as wav:
        wav.setnchannels(channels)
        wav.setsampwidth(width)
        wav.setframerate(rate)
        wav.writeframes(b"\x01" * frames * channels * width)
    return buffer.getvalue()


class WorkerContracts(unittest.TestCase):
    def test_complete_pcm_and_exact_duration_boundary(self):
        raw = wav_bytes(frames=16000)
        pcm, frames = pcm_from_wav(raw, maximum_seconds=1)
        self.assertEqual((len(pcm), frames), (32000, 16000))
        with self.assertRaises(ServiceError) as caught:
            pcm_from_wav(wav_bytes(frames=16001), maximum_seconds=1)
        self.assertEqual(caught.exception.code, "audio_duration_invalid")

    def test_wrong_format_and_truncated_audio_fail_before_inference(self):
        for raw, code in [(b"not wav", "unsupported_audio"), (wav_bytes(channels=2), "unsupported_audio"),
                          (wav_bytes(rate=24000), "unsupported_audio"), (wav_bytes(width=1), "unsupported_audio"),
                          (wav_bytes(frames=0), "audio_duration_invalid"), (wav_bytes()[:-2], "audio_truncated")]:
            with self.subTest(code=code), self.assertRaises(ServiceError) as caught:
                pcm_from_wav(raw)
            self.assertEqual(caught.exception.code, code)

    def test_base64_is_bounded_strict_and_not_a_data_url(self):
        self.assertEqual(decode_base64(base64.b64encode(b"abc").decode(), 3), b"abc")
        for value in [None, "", "data:audio/wav;base64,YWJj", "!!!!", base64.b64encode(b"abcd").decode()]:
            with self.subTest(value=value), self.assertRaises(ServiceError):
                decode_base64(value, 3)

    def test_actual_worker_recovers_after_bad_requests_without_echoing_private_text(self):
        with tempfile.TemporaryDirectory() as folder:
            secret = "PRIVATE_FIXTURE_CHILD_TEXT_不应进入诊断"
            lines = [
                json.dumps({"id": secret, "op": "health", "payload": {}}),
                '{"id":"bad-json","op":"health","payload":{"x":NaN}}',
                json.dumps({"id": "bad-payload", "op": "health", "payload": []}),
                json.dumps({"id": "unsupported", "op": "unknown", "payload": {"text": secret}}),
                json.dumps({"id": "healthy", "op": "health", "payload": {}}),
            ]
            process = subprocess.run([sys.executable, str(Path(__file__).with_name("worker.py")), "--model-dir", folder],
                                     input="\n".join(lines) + "\n", text=True, capture_output=True, timeout=20)
            self.assertEqual(process.returncode, 0, process.stderr)
            responses = [json.loads(line) for line in process.stdout.splitlines()]
            self.assertEqual(len(responses), 5)
            self.assertTrue(all(not response["ok"] for response in responses[:4]))
            self.assertTrue(responses[-1]["ok"])
            self.assertEqual(responses[-1]["id"], "healthy")
            self.assertNotIn(secret, process.stdout + process.stderr)


if __name__ == "__main__":
    unittest.main()
