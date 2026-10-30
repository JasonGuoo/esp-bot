"""Stdlib endpoint and security checks; no external LLM account required."""
import json
from pathlib import Path
import sys
import threading
import unittest
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "server"))

from espbot.app import create_server
from espbot.config import Config


class ServerTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.httpd = create_server(Config(bind="127.0.0.1", port=0))
        cls.port = cls.httpd.server_address[1]
        cls.worker = threading.Thread(target=cls.httpd.serve_forever, daemon=True)
        cls.worker.start()

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        cls.httpd.server_close()
        cls.worker.join(timeout=3)

    def request(self, path, data=None):
        body = json.dumps(data).encode() if data is not None else None
        req = urllib.request.Request(
            f"http://127.0.0.1:{self.port}{path}",
            data=body,
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=3) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as exc:
            return exc.code, json.load(exc)

    def test_health(self):
        code, data = self.request("/health")
        self.assertEqual(code, 200)
        self.assertTrue(data["ok"])

    def test_theme_palette_complete(self):
        code, data = self.request("/v1/themes")
        self.assertEqual(code, 200)
        self.assertEqual(len(data["themes"]), 5)
        self.assertEqual(set(d["id"] for d in data["themes"]),
                         {"mint", "sakura", "lavender", "butter", "sky"})
        self.assertTrue(all(d["eye"] == d["pupil"] for d in data["themes"]))

    def test_enqueue_pop(self):
        message = {
            "device_id": "test-queue", "type": "face.set",
            "payload": {
                "theme": "lavender", "expression": "happy",
                "subtitle": "你好", "gaze_x": -55, "gaze_y": 40,
            },
        }
        code, enq = self.request("/v1/commands", message)
        self.assertEqual(code, 200)
        code, polled = self.request("/v1/commands/next?device_id=test-queue")
        self.assertEqual(code, 200)
        self.assertEqual(polled["command"]["id"], enq["command_id"])
        self.assertEqual(polled["command"]["payload"], message["payload"])
        self.assertIsNone(self.request("/v1/commands/next?device_id=test-queue")[1]["command"])

    def test_invalid_theme_rejected(self):
        code, body = self.request("/v1/commands", {
            "device_id": "validation", "type": "face.set",
            "payload": {"theme": "made-up"},
        })
        self.assertEqual(code, 400)
        self.assertEqual(body["error"], "invalid_theme")

    def test_reject_unknown_commands_and_gaze(self):
        for command in (
            {"type": "shell.exec", "payload": {"command": "id"}},
            {"type": "face.set", "payload": {"gaze_x": 9999}},
            {"type": "face.set", "payload": {"gaze_y": True}},
        ):
            code, _ = self.request("/v1/commands", {"device_id": "validation", **command})
            self.assertEqual(code, 400)

    def test_llm_requires_configuration(self):
        code, data = self.request("/v1/chat", {
            "device_id": "korvo-01", "message": "Hello"
        })
        self.assertEqual(code, 503)
        self.assertEqual(data["error"], "llm_not_configured")

    def test_tools_allowlist(self):
        code, data = self.request("/v1/tools/call", {
            "name": "math.add", "arguments": {"a": 7, "b": 11}
        })
        self.assertEqual(code, 200)
        self.assertEqual(data["result"]["value"], 18)
        code, data = self.request("/v1/tools/call", {
            "name": "shell.exec", "arguments": {"command": "rm -rf /"}
        })
        self.assertEqual(code, 400)

    def test_events_and_devices(self):
        code, _ = self.request("/v1/devices/events", {
            "device_id": "telemetry", "event": "face.ready", "payload": {}
        })
        self.assertEqual(code, 200)
        code, data = self.request("/v1/devices")
        self.assertEqual(code, 200)
        dev = next(d for d in data["devices"] if d["device_id"] == "telemetry")
        self.assertEqual(dev["last_event"], "face.ready")


class SecurityTest(unittest.TestCase):
    def test_public_bind_needs_token(self):
        with self.assertRaises(ValueError):
            Config(bind="0.0.0.0", port=8766, token="").validate()

    def test_token_protects_commands(self):
        httpd = create_server(Config(bind="127.0.0.1", port=0, token="secret"))
        worker = threading.Thread(target=httpd.serve_forever, daemon=True)
        worker.start()
        try:
            url = f"http://127.0.0.1:{httpd.server_address[1]}/v1/themes"
            with self.assertRaises(urllib.error.HTTPError) as cm:
                urllib.request.urlopen(url, timeout=3)
            self.assertEqual(cm.exception.code, 401)
            req = urllib.request.Request(url, headers={"Authorization": "Bearer secret"})
            with urllib.request.urlopen(req, timeout=3) as response:
                self.assertEqual(response.status, 200)
        finally:
            httpd.shutdown()
            httpd.server_close()
            worker.join(timeout=3)


if __name__ == "__main__":
    unittest.main()
