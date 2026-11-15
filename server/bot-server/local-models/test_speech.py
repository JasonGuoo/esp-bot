"""Local macOS speech regressions; no model inference or downloads."""
import base64
import io
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock, patch
import wave

from worker import ServiceError, Worker


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


if __name__ == "__main__":
    unittest.main()
