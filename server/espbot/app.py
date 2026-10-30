"""Threaded LAN Brain API, stdlib only. Never accept arbitrary code/tool URLs."""
from __future__ import annotations

from datetime import datetime, timezone
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import queue
import re
import threading
from urllib.parse import parse_qs, urlsplit

from . import __version__
from .config import Config
from .llm import LLMError, OpenAICompatibleLLM
from .tools import ToolError, ToolRegistry

EXPRESSIONS = (
    "idle", "listening", "happy", "curious", "thinking",
    "surprised", "sleepy", "shy", "blink",
)
DEVICE_ID_RE = re.compile(r"^[A-Za-z0-9_-]{1,48}$")
ROOT = Path(__file__).resolve().parents[2]
THEMES_FILE = ROOT / "shared" / "themes.json"
MAX_BODY = 8192
MAX_QUEUE = 128


class ApiError(Exception):
    def __init__(self, status: int, code: str, message: str):
        super().__init__(message)
        self.status = status
        self.code = code


def _device_id(value: object) -> str:
    if not isinstance(value, str) or not DEVICE_ID_RE.fullmatch(value):
        raise ApiError(400, "invalid_device_id", "Use 1..48 ASCII letters/digits/_/-")
    return value


class BrainState:
    def __init__(self) -> None:
        data = json.loads(THEMES_FILE.read_text(encoding="utf-8"))
        self.themes = data
        self.theme_ids = {item["id"] for item in data["themes"]}
        self.lock = threading.RLock()
        self.queues: dict[str, queue.Queue[dict]] = {}
        self.devices: dict[str, dict] = {}
        self.next_id = 1
        self.tools = ToolRegistry()

    def _visit(self, device_id: str) -> queue.Queue[dict]:
        if device_id not in self.queues:
            self.queues[device_id] = queue.Queue(maxsize=MAX_QUEUE)
        self.devices.setdefault(device_id, {})
        self.devices[device_id]["last_seen_utc"] = datetime.now(timezone.utc).isoformat()
        return self.queues[device_id]

    def enqueue(self, device_id: str, kind: str, payload: dict) -> int:
        _device_id(device_id)
        if kind != "face.set":
            raise ApiError(400, "invalid_command", "Only face.set is implemented")
        if not isinstance(payload, dict) or not payload:
            raise ApiError(400, "invalid_payload", "Nonempty face.set payload required")
        allowed = {"theme", "expression", "subtitle", "gaze_x", "gaze_y"}
        if set(payload) - allowed:
            raise ApiError(400, "invalid_payload", "Unknown face.set fields")
        if "theme" in payload and (
            not isinstance(payload["theme"], str) or payload["theme"] not in self.theme_ids
        ):
            raise ApiError(400, "invalid_theme", "Theme ID is not registered")
        if "expression" in payload and payload["expression"] not in EXPRESSIONS:
            raise ApiError(400, "invalid_expression", "Expression ID is not registered")
        if "subtitle" in payload and (
            not isinstance(payload["subtitle"], str) or len(payload["subtitle"]) > 180
        ):
            raise ApiError(400, "invalid_subtitle", "Subtitle must be <=180 characters")
        for axis in ("gaze_x", "gaze_y"):
            if axis in payload and (
                type(payload[axis]) is not int or not -100 <= payload[axis] <= 100
            ):
                raise ApiError(400, "invalid_gaze", "Gaze is integer -100..100")
        with self.lock:
            q = self._visit(device_id)
            if q.full():
                raise ApiError(429, "queue_full", "Device command queue full")
            cmd_id = self.next_id
            self.next_id += 1
            q.put_nowait({"id": cmd_id, "type": kind, "payload": payload.copy()})
            return cmd_id

    def pop(self, device_id: str) -> dict | None:
        _device_id(device_id)
        with self.lock:
            q = self._visit(device_id)
            try:
                return q.get_nowait()
            except queue.Empty:
                return None

    def devices_list(self) -> list[dict]:
        with self.lock:
            return [
                {
                    "device_id": device_id,
                    "last_seen_utc": info["last_seen_utc"],
                    "pending_commands": self.queues[device_id].qsize(),
                    "last_event": info.get("last_event"),
                }
                for device_id, info in sorted(self.devices.items())
            ]

    def note_event(self, device_id: str, event: str, payload: dict) -> None:
        _device_id(device_id)
        if not isinstance(event, str) or not re.fullmatch(r"[a-z][a-z0-9_.-]{0,47}", event):
            raise ApiError(400, "invalid_event", "Invalid event name")
        if not isinstance(payload, dict):
            raise ApiError(400, "invalid_payload", "Event payload must be an object")
        with self.lock:
            self._visit(device_id)
            self.devices[device_id]["last_event"] = event


def _handler_factory(state: BrainState, cfg: Config):
    llm = OpenAICompatibleLLM(cfg)

    class BrainHandler(BaseHTTPRequestHandler):
        server_version = "ESPBotBrain/0.1"

        def _reply(self, status: int, body: dict) -> None:
            raw = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(raw)))
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(raw)

        def _authorize(self) -> None:
            if cfg.token:
                expected = "Bearer " + cfg.token
                received = self.headers.get("Authorization", "")
                if not hmac.compare_digest(received, expected):
                    raise ApiError(401, "unauthorized", "Bearer token required")

        def _body(self) -> dict:
            length_raw = self.headers.get("Content-Length", "")
            if not length_raw.isdigit():
                raise ApiError(411, "content_length_required", "Provide Content-Length")
            length = int(length_raw)
            if length < 1 or length > MAX_BODY:
                raise ApiError(413, "body_too_large", "Body must be 1..8192 bytes")
            try:
                body = json.loads(self.rfile.read(length))
            except (UnicodeDecodeError, ValueError) as exc:
                raise ApiError(400, "invalid_json", "Body is not valid JSON") from exc
            if not isinstance(body, dict):
                raise ApiError(400, "invalid_json", "Root must be a JSON object")
            return body

        def _run(self, method: str) -> None:
            try:
                parsed = urlsplit(self.path)
                path = parsed.path
                if method == "GET" and path == "/health":
                    self._reply(200, {"ok": True, "service": "esp-bot-brain", "version": __version__})
                    return
                self._authorize()
                if method == "GET":
                    if path == "/v1/themes":
                        self._reply(200, state.themes)
                    elif path == "/v1/expressions":
                        self._reply(200, {"expressions": EXPRESSIONS})
                    elif path == "/v1/devices":
                        self._reply(200, {"devices": state.devices_list()})
                    elif path == "/v1/tools":
                        self._reply(200, {"tools": state.tools.list_tools()})
                    elif path == "/v1/commands/next":
                        params = parse_qs(parsed.query, keep_blank_values=True)
                        ids = params.get("device_id", [])
                        if len(ids) != 1:
                            raise ApiError(400, "invalid_device_id", "One device_id required")
                        self._reply(200, {"command": state.pop(ids[0])})
                    else:
                        raise ApiError(404, "not_found", "Unknown endpoint")
                elif method == "POST":
                    body = self._body()
                    if path == "/v1/commands":
                        cmd_id = state.enqueue(
                            _device_id(body.get("device_id")),
                            body.get("type"),
                            body.get("payload"),
                        )
                        self._reply(200, {"ok": True, "command_id": cmd_id})
                    elif path == "/v1/devices/events":
                        state.note_event(
                            _device_id(body.get("device_id")),
                            body.get("event"),
                            body.get("payload", {}),
                        )
                        self._reply(200, {"ok": True})
                    elif path == "/v1/tools/call":
                        try:
                            result = state.tools.invoke(body.get("name"), body.get("arguments", {}))
                        except ToolError as exc:
                            raise ApiError(400, "tool_error", str(exc)) from exc
                        self._reply(200, {"ok": True, "result": result})
                    elif path == "/v1/chat":
                        device_id = _device_id(body.get("device_id"))
                        message = body.get("message")
                        if not isinstance(message, str) or not message.strip() or len(message) > 4000:
                            raise ApiError(400, "invalid_message", "Nonempty message <=4000 chars required")
                        if not llm.configured:
                            raise ApiError(503, "llm_not_configured", "Set Brain LLM model and base URL")
                        try:
                            reply = llm.complete(message)
                        except LLMError as exc:
                            raise ApiError(502, "llm_error", str(exc)) from exc
                        cmd_id = state.enqueue(
                            device_id, "face.set",
                            {"subtitle": reply[:180], "expression": "happy"},
                        )
                        self._reply(200, {"ok": True, "text": reply, "command_id": cmd_id})
                    else:
                        raise ApiError(404, "not_found", "Unknown endpoint")
                else:
                    raise ApiError(405, "method_not_allowed", "Unsupported method")
            except ApiError as exc:
                self._reply(exc.status, {"error": exc.code, "message": str(exc)})
            except (BrokenPipeError, ConnectionResetError):
                pass
            except Exception:
                # Never disclose stack traces or model provider configuration.
                self._reply(500, {"error": "internal_error", "message": "Unexpected server error"})

        def do_GET(self) -> None:
            self._run("GET")

        def do_POST(self) -> None:
            self._run("POST")

    return BrainHandler


def create_server(config: Config | None = None, state: BrainState | None = None) -> ThreadingHTTPServer:
    cfg = config or Config.from_env()
    cfg.validate()
    brain = state or BrainState()
    return ThreadingHTTPServer((cfg.bind, cfg.port), _handler_factory(brain, cfg))


def main() -> None:
    cfg = Config.from_env()
    with create_server(cfg) as server:
        host, port = server.server_address[:2]
        print(f"ESP Bot Brain listening on http://{host}:{port}", flush=True)
        if cfg.bind not in ("127.0.0.1", "localhost", "::1"):
            print("WARNING: plain HTTP, development on trusted LAN only", flush=True)
        server.serve_forever(poll_interval=0.5)
