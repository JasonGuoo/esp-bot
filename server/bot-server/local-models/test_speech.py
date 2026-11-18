"""Local macOS speech regressions; no model inference or downloads."""
import base64
import io
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock, patch
import wave

from worker import ServiceError, Worker, resample_to_16k


@unittest.skipUnless(sys.platform == "darwin", "Requires the installed macOS say tool")
class SpeechTests(unittest.TestCase):
    def setUp(self):
        self.worker = Worker(Path("/tmp/espbot-unused-models"), "Tingting", 180)

    def test_inventory_keeps_qualified_names_and_deduplicates(self):
        process = Mock(returncode=0)
        process.communicate.return_value = (
            b"Tingting (Chinese (China mainland)) zh_CN # sample\n" * 2,
            None,
        )
        with patch("worker.subprocess.Popen", return_value=process):
            voices = self.worker.voices()["voices"]
        self.assertEqual(voices, [{"name": "Tingting (Chinese (China mainland))", "language": "zh_CN"}])

    def test_ambiguous_short_name_is_rejected_before_synthesis(self):
        self.worker.voice_inventory = [
            {"name": "Example (English)", "language": "en_US"},
            {"name": "Example (Chinese)", "language": "zh_CN"},
        ]
        with patch("worker.subprocess.Popen") as spawn:
            with self.assertRaises(ServiceError) as caught:
                self.worker.tts({"text": "测试", "voice": "Example"})
        self.assertEqual(caught.exception.code, "tts_voice_unavailable")
        spawn.assert_not_called()

    def test_installed_short_name_synthesizes_real_pcm(self):
        if not any(v["name"] == "Tingting" or v["name"].startswith("Tingting (")
                   for v in self.worker.voices()["voices"]):
            self.skipTest("Tingting is not installed; never download a voice")
        result = self.worker.tts({"text": "本机语音测试通过。"})
        self.assertTrue(result["voice"].startswith("Tingting"))
        with wave.open(io.BytesIO(base64.b64decode(result["wav_base64"])), "rb") as wav:
            self.assertEqual((wav.getnchannels(), wav.getsampwidth(), wav.getframerate()), (1, 2, 16000))
            self.assertGreater(wav.getnframes(), 1600)
            self.assertTrue(any(wav.readframes(wav.getnframes())))


class KokoroTests(unittest.TestCase):
    def test_resampling_keeps_speech_band_and_removes_aliases(self):
        try:
            import numpy as np
        except ImportError:
            self.skipTest("numpy is not installed")
        t = np.arange(24000) / 24000
        level = lambda f: float(np.sqrt(np.mean((resample_to_16k(0.5 * np.sin(2 * np.pi * f * t), 24000)[2000:-2000] / 32767.0) ** 2)))
        self.assertEqual(resample_to_16k(np.zeros(24000), 24000).size, 16000)
        self.assertAlmostEqual(level(440), 0.5 / np.sqrt(2), places=2)
        self.assertLess(level(10000), 0.01, "10 kHz would alias to 6 kHz")

    @unittest.skipUnless(sys.platform == "darwin", "Requires the installed macOS say tool")
    def test_a_broken_kokoro_model_falls_back_to_say(self):
        broken = Path("/tmp") / "espbot-broken-kokoro"
        broken.mkdir(exist_ok=True)
        (broken / "model.onnx").write_bytes(b"not a model")
        worker = Worker(Path("/tmp/espbot-unused-models"), "Tingting", 180, tts_backend="kokoro", kokoro_model_dir=str(broken))
        with patch("worker.installed", return_value=True), patch.object(Worker, "_kokoro_engine", side_effect=RuntimeError("load")):
            with patch.object(Worker, "voices", return_value={"voices": []}):
                with self.assertRaises(ServiceError) as caught:
                    worker.tts({"text": "测试"})
        self.assertEqual(caught.exception.code, "tts_voice_unavailable", "fell through to the say path")
        self.assertTrue(worker.kokoro_failed)
        self.assertFalse(worker._kokoro_wanted())


if __name__ == "__main__":
    unittest.main()
