"""Optional OpenAI-compatible text-model client (server side only)."""
from __future__ import annotations
import json
import urllib.error
import urllib.request
from .config import Config


class LLMError(RuntimeError):
    pass


class OpenAICompatibleLLM:
    def __init__(self, config: Config):
        self.config = config

    @property
    def configured(self) -> bool:
        return bool(self.config.llm_base_url and self.config.llm_model)

    def complete(self, message: str) -> str:
        if not self.configured:
            raise LLMError("llm_not_configured")
        payload = {
            "model": self.config.llm_model,
            "messages": [
                {"role": "system", "content": (
                    "You are the speech brain of a small friendly robot. "
                    "Be concise, clear, and truthful. Prefer short spoken replies."
                )},
                {"role": "user", "content": message},
            ],
            "stream": False,
        }
        headers = {"Content-Type": "application/json", "Accept": "application/json"}
        if self.config.llm_api_key:
            headers["Authorization"] = f"Bearer {self.config.llm_api_key}"
        req = urllib.request.Request(
            f"{self.config.llm_base_url}/chat/completions",
            data=json.dumps(payload).encode("utf-8"),
            headers=headers,
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as response:
                raw = response.read(512_001)
            if len(raw) > 512_000:
                raise LLMError("llm_response_too_large")
            data = json.loads(raw)
            content = data["choices"][0]["message"]["content"]
            if not isinstance(content, str) or not content.strip():
                raise LLMError("llm_empty_response")
            return content.strip()
        except (urllib.error.URLError, ValueError, KeyError, IndexError, TypeError) as exc:
            raise LLMError(f"llm_provider_error: {type(exc).__name__}") from exc
