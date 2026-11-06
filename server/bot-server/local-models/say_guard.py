#!/usr/bin/env python3
"""Private child-process adapter for macOS `say`, not an RPC entry point.

Keep only the parent liveness pipe's read end. Parent exit (including SIGKILL)
closes the write end, allowing this process to stop and reap `say`. No PID-based
identity assumptions, text logs, shell interpolation or network audio output.
"""

from __future__ import annotations

import os
import select
import signal
import subprocess
import sys
import time
from pathlib import Path

MAX_SECONDS = 120
MAX_OUTPUT_BYTES = 16000 * 2 * MAX_SECONDS + 65536
interrupted = False


def stop(_signum, _frame):
    global interrupted
    interrupted = True


def main() -> int:
    if len(sys.argv) != 5:
        return 2
    alive_fd = int(sys.argv[1])
    output = Path(sys.argv[2])
    voice = sys.argv[3]
    rate = int(sys.argv[4])
    os.umask(0o077)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    # Parent sends <=1000 Unicode characters. A hard byte limit prevents this
    # private helper from becoming an unbounded pipe reader if misused.
    text = sys.stdin.buffer.read(12001)
    if not text or len(text) > 12000 or not 100 <= rate <= 260:
        return 2
    process = None
    completed = False
    try:
        process = subprocess.Popen(
            ["/usr/bin/say", "-v", voice, "-r", str(rate), "-o", str(output), "--file-format=WAVE", "--data-format=LEI16@16000", "--channels=1", "-f", "-"],
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        process.stdin.write(text)
        process.stdin.close()
        deadline = time.monotonic() + MAX_SECONDS
        while process.poll() is None:
            if interrupted or time.monotonic() >= deadline:
                return 3
            if output.exists() and output.stat().st_size > MAX_OUTPUT_BYTES:
                return 4
            readable, _, _ = select.select([alive_fd], [], [], 0.2)
            if readable and os.read(alive_fd, 1) == b"":
                return 5
        if process.returncode != 0:
            return 6
        completed = True
        return 0
    finally:
        if process is not None and process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=1)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
        os.close(alive_fd)
        if not completed:
            output.unlink(missing_ok=True)
            try:
                output.parent.rmdir()
            except OSError:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
