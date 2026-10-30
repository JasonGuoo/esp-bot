"""Small allowlisted tool registry. No arbitrary shell, file or URL access."""
from __future__ import annotations
from datetime import datetime, timezone
import math


class ToolError(ValueError):
    pass


def _clock_utc(args: dict) -> dict:
    if args:
        raise ToolError("clock.utc takes no arguments")
    return {"iso_8601": datetime.now(timezone.utc).isoformat()}


def _math_add(args: dict) -> dict:
    if set(args) != {"a", "b"}:
        raise ToolError("math.add requires exactly numeric a and b")
    a, b = args["a"], args["b"]
    if any(isinstance(v, bool) or not isinstance(v, (float, int)) or not math.isfinite(v) for v in (a, b)):
        raise ToolError("a and b must be finite numbers")
    result = a + b
    if not math.isfinite(result):
        raise ToolError("result is not finite")
    return {"value": result}


_TOOLS = {"clock.utc": _clock_utc, "math.add": _math_add}


class ToolRegistry:
    def list_tools(self) -> list[dict]:
        return [
            {"name": "clock.utc", "description": "Current UTC time", "parameters": {}},
            {"name": "math.add", "description": "Add two finite numbers", "parameters": {"a": "number", "b": "number"}},
        ]

    def invoke(self, name: str, args: dict) -> dict:
        action = _TOOLS.get(name)
        if action is None:
            raise ToolError("Unknown or disallowed tool")
        if not isinstance(args, dict):
            raise ToolError("Tool arguments must be an object")
        return action(args)
