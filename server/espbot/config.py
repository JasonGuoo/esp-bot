"""Process configuration; never put model provider credentials on ESP32."""
from __future__ import annotations
import ipaddress
import os
from dataclasses import dataclass


@dataclass(frozen=True)
class Config:
    bind: str = "127.0.0.1"
    port: int = 8766
    token: str = ""
    llm_base_url: str = ""
    llm_model: str = ""
    llm_api_key: str = ""

    @classmethod
    def from_env(cls) -> "Config":
        return cls(
            bind=os.getenv("ESPBOT_BIND", "127.0.0.1"),
            port=int(os.getenv("ESPBOT_PORT", "8766")),
            token=os.getenv("ESPBOT_TOKEN", ""),
            llm_base_url=os.getenv("ESPBOT_LLM_BASE_URL", "").rstrip("/"),
            llm_model=os.getenv("ESPBOT_LLM_MODEL", ""),
            llm_api_key=os.getenv("ESPBOT_LLM_API_KEY", ""),
        )

    def validate(self) -> None:
        if not 0 <= self.port <= 65535:
            raise ValueError("ESPBOT_PORT must be 0..65535")
        try:
            ip = ipaddress.ip_address(self.bind)
            local = ip.is_loopback
        except ValueError:
            local = self.bind == "localhost"
        if not local and not self.token:
            raise ValueError(
                "ESPBOT_TOKEN required for any non-loopback bind. "
                "Never expose this HTTP prototype to the public Internet."
            )
        if bool(self.llm_base_url) != bool(self.llm_model):
            raise ValueError("Set ESPBOT_LLM_BASE_URL and ESPBOT_LLM_MODEL together")
        if self.llm_base_url and not self.llm_base_url.startswith(("http://", "https://")):
            raise ValueError("ESPBOT_LLM_BASE_URL must start with http:// or https://")
