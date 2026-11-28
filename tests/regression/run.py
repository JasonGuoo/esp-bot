#!/usr/bin/env python3
"""Run real local regressions and retain per-scenario evidence, not just exit 0.

Only the fixed commands below are executable. The scenario catalogue is data.
Reports live outside the checkout and never include environment values.
"""
from __future__ import annotations

import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
CATALOGUE = Path(__file__).with_name("scenarios.json")
SERVER = "server/bot-server/src-tauri/Cargo.toml"
CORE = "server/xiaodai-core/Cargo.toml"


def steps(python):
    cargo = ["cargo", "test", "--offline", "--locked", "--manifest-path"]
    return [
        dict(id="suite", argv=[sys.executable, "-m", "unittest", "discover", "-s", "tests/regression", "-p", "test_runner.py", "-v"], timeout=60, kind="unittest"),
        dict(id="firmware", argv=[sys.executable, "tests/regression/firmware_host.py"], timeout=180, kind="cases"),
        dict(id="server", argv=[*cargo, SERVER, "--lib", "--", "--test-threads=4"], timeout=600, kind="rust", allowed_skips=[
            "harness::direct::tests::live_direct_endpoints_and_chat_paths", "harness::direct::tests::live_opencode_uses_private_provider_url", "memory::store::background_tests::context_read_cost",
            *["brain::gateway::tests::simulator::child_dialogues::live_" + scene for scene in
              ("moon_ferry", "quiet_radio", "sock_detective", "puddle_kitchen")]]),
        dict(id="core", argv=[*cargo, CORE, "--", "--test-threads=4"], timeout=600, kind="rust", allowed_skips=["src/lib.rs - (line 4)"]),
        dict(id="python", argv=[python, str(ROOT / "tests/regression/python_local.py")], cwd="server/bot-server/local-models", timeout=180, kind="cases"),
        dict(id="console", argv=["node", "--test", "--test-reporter=tap", "scripts/test-console.mjs"], cwd="server/bot-server", timeout=60, kind="node"),
        dict(id="tla", argv=["./tools/check-tla-twins.sh"], timeout=600, kind="tla"),
        dict(id="console-build", argv=["npm", "run", "build"], cwd="server/bot-server", timeout=180, kind="build"),
        dict(id="idf-build", argv=["./firmware/idf.sh", "build"], timeout=600, kind="build"),
    ]


def catalogue(path=CATALOGUE):
    rows = json.loads(path.read_text())
    seen = set()
    valid_steps = {s["id"] for s in steps(sys.executable)}
    for row in rows:
        if row["id"] in seen or row["mode"] not in ("automated", "acceptance", "deferred"):
            raise ValueError(f"Duplicate/invalid scenario: {row['id']}")
        seen.add(row["id"])
        for key in ("area", "scenario", "expected", "owner"):
            if not row.get(key):
                raise ValueError(f"Missing {key}: {row['id']}")
        if row["mode"] == "automated" and not row.get("evidence"):
            raise ValueError(f"No implementation evidence: {row['id']}")
        for step, case in row.get("evidence", []):
            if step not in valid_steps or not case:
                raise ValueError(f"Invalid evidence: {row['id']}")
        for twin in row.get("twins", []):
            folder = ROOT / "formal/tla" / twin
            for name in (folder.name + ".tla", folder.name + ".cfg", "checks.sh"):
                if not (folder / name).is_file():
                    raise ValueError(f"Missing twin {twin}/{name}")
    return rows


def parse_evidence(kind, output):
    if kind == "rust":
        cases = re.findall(r"^test (.+?) \.\.\. ok$", output, re.M)
        skipped = re.findall(r"^test (.+?) \.\.\. ignored[^\n]*", output, re.M)
    elif kind == "cases":
        cases = re.findall(r"^REGRESSION_CASE (\S+) passed$", output, re.M)
        skipped = re.findall(r"^REGRESSION_CASE (\S+) not_run.*$", output, re.M)
    elif kind == "node":
        cases = re.findall(r"^ok \d+ - ([^\n]+)$", output, re.M)
        skipped = [case for case in cases if "# SKIP" in case or "# TODO" in case]
        cases = [case for case in cases if case not in skipped]
    elif kind == "tla":
        cases = []
        for block in output.split("Checking formal/tla/")[1:]:
            if "Model checking completed. No error has been found." in block:
                cases.append(block.splitlines()[0].rstrip("/"))
        skipped = []
    elif kind == "unittest":
        cases = re.findall(r"^(test_\w+) \([^\n]+\) \.\.\. ok$", output, re.M)
        skipped = re.findall(r"^(test_\w+) \([^\n]+\) \.\.\. skipped.*$", output, re.M)
    else:
        cases, skipped = ["build"], []
    return sorted(set(cases)), sorted(set(skipped))


def run_step(step, report_dir, env, root=ROOT):
    started = time.monotonic()
    log = report_dir / (step["id"] + ".log")
    result = dict(id=step["id"], argv=step["argv"], status="failed", cases=[], skipped=[], log=str(log))
    prerequisites = {
        "firmware": ["firmware/build/config/sdkconfig.h", "firmware/.toolchains/esp-idf/export.sh", "firmware/managed_components/espressif__esp-sr/include/esp32s31/esp_mn_iface.h"],
        "idf-build": ["firmware/.toolchains/esp-idf/export.sh", "firmware/sdkconfig"],
        "console": ["server/bot-server/node_modules/typescript/package.json"],
        "console-build": ["server/bot-server/node_modules/typescript/package.json", "server/bot-server/node_modules/vite/package.json"],
        "tla": ["firmware/build/config/sdkconfig.h"],
    }
    missing = [p for p in prerequisites.get(step["id"], []) if not (root / p).exists()]
    commands = {"firmware": ["xcrun"], "tla": ["java", "cargo", "xcrun"], "idf-build": ["bash"]}
    missing += [name for name in commands.get(step["id"], []) if not shutil.which(name, path=env.get("PATH"))]
    if missing:
        reason = "missing prerequisites: " + ", ".join(missing)
        log.write_text(reason + "\n")
        result.update(status="not_run", reason=reason, seconds=0)
        return result
    with log.open("w") as output:
        try:
            process = subprocess.Popen(step["argv"], cwd=root / step.get("cwd", "."),
                                       env=env, stdin=subprocess.DEVNULL, stdout=output,
                                       stderr=subprocess.STDOUT, start_new_session=True)
            try:
                code = process.wait(timeout=step["timeout"])
            except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
                # A build/worker can spawn descendants. Retire the whole owned
                # group, including a child that ignores SIGTERM.
                for sig in (signal.SIGTERM, signal.SIGKILL):
                    try:
                        os.killpg(process.pid, sig)
                    except ProcessLookupError:
                        pass
                process.wait()
                result["reason"] = "timeout" if isinstance(error, subprocess.TimeoutExpired) else "interrupted"
                result["returncode"] = process.returncode
                return result
            result["returncode"] = code
        except (FileNotFoundError, PermissionError) as error:
            result.update(status="not_run", reason=str(error))
            return result
        finally:
            result["seconds"] = round(time.monotonic() - started, 3)
    result["cases"], result["skipped"] = parse_evidence(step["kind"], log.read_text(errors="replace"))
    if code:
        result["reason"] = f"exit {code}"
    elif not result["cases"]:
        result["reason"] = "zero executed cases (a stale filter is not a pass)"
    elif set(result["skipped"]) - set(step.get("allowed_skips", [])):
        result.update(status="not_run", reason="one or more required cases skipped")
    else:
        result["status"] = "passed"
    return result


def catalogue_markdown(rows):
    lines = ["# 小呆功能回归场景", "", "由 `scenarios.json` 生成：`./tools/check-functional-regression.sh --list`。automated 只指所列实现和孪生可自动执行，实际结果见每次 report.md；acceptance 是独立待验项，deferred 是用户暂缓。", ""]
    for area in dict.fromkeys(r["area"] for r in rows):
        lines += [f"## {area}", "", "| ID / 责任 | 操作与情境 | 可观察预期 | 验证层 |", "| --- | --- | --- | --- |"]
        for r in rows:
            if r["area"] == area:
                lines.append(f"| {r['id']} / {r['owner']} | {r['scenario']} | {r['expected']} | {r['mode']} |")
        lines.append("")
    lines += ["## 精确执行映射", "", "以下名称必须实际执行成功；用例被删除、改名、跳过或零匹配都会阻止自动场景通过。TLA 模型是额外证据，不能代替实现回归。", ""]
    for row in rows:
        lines.append(f"- **{row['id']}**：" + "；".join([f"`{step}:{case}`" for step, case in row.get("evidence", [])] + [f"`tla:{twin}`" for twin in row.get("twins", [])] + ([row["reason"]] if "reason" in row else [])))
    return "\n".join(lines) + "\n"


def scenario_results(rows, results):
    by_step = {r["id"]: r for r in results}
    assessed = []
    for row in rows:
        entry = dict(row, status="not_run")
        if row["mode"] == "deferred":
            entry["reason"] = row.get("reason", "explicitly deferred")
        elif row["mode"] == "acceptance":
            entry["reason"] = row["reason"]
        else:
            missing, failures = [], []
            evidence = row["evidence"] + [["tla", twin] for twin in row.get("twins", [])]
            for step, case in evidence:
                run = by_step.get(step)
                if run is None or run["status"] == "not_run":
                    missing.append(f"{step}:{case}")
                elif run["status"] != "passed":
                    failures.append(f"{step}:{case}")
                elif case not in run["cases"]:
                    # Exact matching makes deleted/renamed cases break the gate.
                    failures.append(f"missing executed case {step}:{case}")
            entry["status"] = "failed" if failures else "not_run" if missing else "passed"
            if failures or missing:
                entry["reason"] = "; ".join(failures + missing)
        assessed.append(entry)
    return assessed


def exit_code(results, scenarios):
    required = results + [r for r in scenarios if r["mode"] == "automated"]
    return 1 if any(r["status"] == "failed" for r in required) else 2 if any(r["status"] != "passed" for r in required) else 0


def write_report(path, report):
    temporary = path / "report.json.tmp"
    temporary.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    temporary.replace(path / "report.json")
    lines = ["# 小呆本机功能回归", "", f"本机自动门禁：**{report['local_status']}**。实板/真实模型验收独立记录，不能由本机通过推导。", "",
             f"源码：`{report['revision']}`；工作区摘要：`{report['worktree_digest']}`；开始：{report['started_utc']}。", "",
             "| 检查 | 状态 | 秒 | 成功案例 / 跳过 | 日志 |", "| --- | --- | ---: | ---: | --- |"]
    for r in report["steps"]:
        lines.append(f"| {r['id']} | {r['status']} | {r.get('seconds', 0)} | {len(r['cases'])} / {len(r['skipped'])} | [{r['id']}]({r['log']}) |")
    lines += ["", "## 场景结果", "", "| ID | 场景 | 状态 | 限制 / 原因 |", "| --- | --- | --- | --- |"]
    for r in report["scenarios"]:
        lines.append(f"| {r['id']} | {r['scenario']} | {r['status']} | {r.get('reason', '仅所列本机实现及模型范围')} |")
    ignored = [(r["id"], case) for r in report["steps"] for case in r["skipped"]]
    lines += ["", "## 未运行的 Rust 用例", "", *[f"- `{step}:{case}`" for step, case in ignored], ""]
    (path / "report.md").write_text("\n".join(lines))


def provenance(env):
    def git(*args):
        return subprocess.check_output(["git", *args], cwd=ROOT, env=env, stderr=subprocess.DEVNULL)
    revision = git("rev-parse", "HEAD").decode().strip()
    digest = hashlib.sha256(git("diff", "--binary", "HEAD"))
    for name in sorted(git("ls-files", "--others", "--exclude-standard", "-z").split(b"\0")):
        if name:
            path = ROOT / os.fsdecode(name)
            digest.update(name + b"\0")
            if path.is_file():
                digest.update(path.read_bytes())
    return revision, digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list", action="store_true", help="list scenarios without running commands")
    parser.add_argument("--only", help="comma-separated steps for iteration; a subset is never a full pass")
    parser.add_argument("--report-dir", type=Path, help="new directory for private logs and JSON/Markdown report")
    parser.add_argument("--model-python", default=os.environ.get("ESPBOT_MODEL_PYTHON", str(Path.home() / "Xiaodai_Server/engines/model-python/bin/python")))
    args = parser.parse_args()
    rows = catalogue()
    if args.list:
        print(catalogue_markdown(rows), end="")
        return 0
    os.umask(0o077)
    env = dict(os.environ)
    env.setdefault("DEVELOPER_DIR", "/Library/Developer/CommandLineTools")
    # Always keep generated core files out of the vendored tree, even if a
    # caller's CARGO_TARGET_DIR points there.
    env["CARGO_TARGET_DIR"] = str(ROOT / "server/bot-server/src-tauri/target")
    env["RUST_BACKTRACE"] = "1"
    all_steps = steps(args.model_python)
    selected = set(args.only.split(",")) if args.only else {s["id"] for s in all_steps}
    if selected - {s["id"] for s in all_steps}:
        parser.error("unknown step in --only")
    path = args.report_dir
    if path:
        path = path.expanduser().resolve()
        if path.is_relative_to(ROOT):
            parser.error("report directory must be outside the checkout")
        path.mkdir(parents=True, exist_ok=False, mode=0o700)
    else:
        cache = Path.home() / "Library/Caches/esp-bot/regression"
        cache.mkdir(parents=True, exist_ok=True)
        path = Path(tempfile.mkdtemp(prefix=datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ-"), dir=cache))
    revision, digest = provenance(env)
    report = dict(started_utc=datetime.now(timezone.utc).isoformat(), revision=revision,
                  worktree_digest=digest, local_status="not_run", steps=[], scenarios=[])
    print(f"Report: {path}", flush=True)
    for step in all_steps:
        if step["id"] not in selected:
            continue
        print(f"Running {step['id']} …", flush=True)
        result = run_step(step, path, env)
        report["steps"].append(result)
        report["scenarios"] = scenario_results(rows, report["steps"])
        write_report(path, report)
        print(f"{step['id']}: {result['status']} ({result['seconds']:.1f}s, {len(result['cases'])} cases) {result.get('reason', '')}", flush=True)
        if result.get("reason") == "interrupted":
            break
    code = exit_code(report["steps"], report["scenarios"])
    if len(report["steps"]) != len(all_steps) and code == 0:
        code = 2
    report["local_status"] = {0: "passed", 1: "failed", 2: "not_run"}[code]
    report["finished_utc"] = datetime.now(timezone.utc).isoformat()
    report["counts"] = dict(Counter(r["status"] for r in report["scenarios"]))
    write_report(path, report)
    print(f"Local: {report['local_status']}; scenarios: {report['counts']}; {path / 'report.md'}", flush=True)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
