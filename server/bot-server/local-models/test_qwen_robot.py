"""Repeatable local adapter/effect/fallback tests; real model smoke is separate."""
import base64
import io
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import wave

import numpy as np
from model_store import ServiceError
from qwen_robot import QwenRobot, robot_effect
from worker import Worker, pcm_from_wav


def chunk(audio=None, rate=24000, tokens=5):
    if audio is None:
        audio = 0.3 * np.sin(2 * np.pi * 440 * np.arange(12000) / 24000)
    return SimpleNamespace(audio=audio, sample_rate=rate, token_count=tokens)


class RobotTests(unittest.TestCase):
    def test_corrupt_weights_are_rejected_before_upstream_model_load(self):
        with tempfile.TemporaryDirectory() as root:
            (Path(root) / "model.safetensors").write_bytes(b"corrupt model")
            robot = QwenRobot(root)
            with patch.object(robot, "status", return_value={"available": True}), self.assertRaises(ServiceError) as caught:
                robot._load()
            self.assertEqual(caught.exception.code, "tts_model_checksum")
            self.assertIsNone(robot.model)
            self.assertFalse(robot.verified)

    def test_real_effect_keeps_bounded_pcm_and_changes_pitch_without_clipping(self):
        original = QwenRobot.collect([chunk()], lambda _: None)
        changed = robot_effect(original)
        pcm, frames = pcm_from_wav(changed, 120)
        values = np.frombuffer(pcm, dtype="<i2") / 32768
        self.assertGreater(frames, 7000)
        self.assertLess(frames, 8300)
        self.assertLess(np.max(np.abs(values)), 0.93)
        frequency = np.argmax(np.abs(np.fft.rfft(values[1000:-1000]))) * 16000 / len(values[1000:-1000])
        self.assertAlmostEqual(frequency, 440 * 1.122462, delta=6)
        self.assertNotEqual(changed, original)
        _, faster = pcm_from_wav(robot_effect(original, 240), 120)
        self.assertLess(faster, frames)

    def test_invalid_chunks_and_token_exhaustion_never_return_partial_audio(self):
        for bad, code in [([], "tts_audio_empty"), ([chunk(np.zeros(10))], "tts_audio_empty"),
                          ([chunk(np.array([np.nan]))], "tts_audio_invalid"),
                          ([chunk(rate=16000)], "tts_audio_invalid"),
                          ([chunk(), chunk(tokens=1395)], "tts_generation_incomplete"),
                          ([chunk(np.ones(24000 * 120 + 1))], "tts_output_too_large")]:
            with self.subTest(code=code), self.assertRaises(ServiceError) as caught:
                QwenRobot.collect(bad, lambda _: None)
            self.assertEqual(caught.exception.code, code)

    def test_effect_missing_and_failed_are_explicit(self):
        wav = QwenRobot.collect([chunk()], lambda _: None)
        for path, expected in [(None, "tts_effect_unavailable"), ("/usr/bin/false", "tts_effect_failed")]:
            with patch("qwen_robot.ffmpeg_path", return_value=path), self.assertRaises(ServiceError) as caught:
                robot_effect(wav)
            self.assertEqual(caught.exception.code, expected)

    def test_worker_rejects_invalid_input_before_model_loading(self):
        worker = Worker(Path("/unused"), "Tingting", 180, tts_backend="qwen3_robot")
        with patch.object(worker.robot, "synthesize") as generate:
            for payload in [{"text": ""}, {"text": "x" * 1001}, {"text": "\ud800"},
                            {"text": "\x00"}, {"text": "你好", "rate": True},
                            {"text": "你好", "rate": 99}, {"text": "你好", "unexpected": 1}]:
                with self.subTest(payload=repr(payload)), self.assertRaises(ServiceError):
                    worker.tts(payload)
            generate.assert_not_called()

    def test_missing_model_uses_real_say_and_reports_fallback_without_retry_storm(self):
        with tempfile.TemporaryDirectory() as root:
            worker = Worker(Path(root), "Tingting", 180, tts_backend="qwen3_robot", qwen3_model_dir=root)
            self.assertFalse(worker.health()["models"]["tts"]["available"])
            result = worker.tts({"text": "本机回退测试。"})
            self.assertEqual(result["backend"], "macos_say")
            self.assertEqual(result["fallback_reason"], "tts_robot_unavailable")
            pcm, _ = pcm_from_wav(base64.b64decode(result["wav_base64"]), 120)
            self.assertTrue(any(pcm))
            with patch.object(worker.robot, "synthesize") as generate:
                worker.tts({"text": "继续测试。"})
                generate.assert_not_called()
            self.assertEqual(worker.health()["models"]["tts"]["backend"], "macos_say")

    def test_generation_failure_falls_back_with_the_complete_original_text(self):
        worker = Worker(Path("/unused"), "Tingting", 180, tts_backend="qwen3_robot")
        text = "第一句。第二句不能丢。"
        with patch.object(worker.robot, "synthesize", side_effect=ServiceError("tts_generation_incomplete", "limit")), \
             patch.object(worker, "_say_tts", return_value={"backend": "macos_say"}) as fallback:
            result = worker.tts({"text": text})
            fallback.assert_called_once_with(text, None, 180)
            self.assertEqual(result["fallback_reason"], "tts_generation_incomplete")
        worker.robot_failure = None
        with patch.object(worker.robot, "synthesize", side_effect=ServiceError("tts_output_too_large", "limit")), \
             patch.object(worker, "_say_tts") as fallback, self.assertRaises(ServiceError):
            worker.tts({"text": text})
        fallback.assert_not_called()


if __name__ == "__main__":
    unittest.main()
