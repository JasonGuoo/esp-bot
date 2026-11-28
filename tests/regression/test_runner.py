"""Real subprocess failure/timeout tests for the local gate itself."""
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import time
import unittest

import run as gate
from children.review import review


class RunnerTests(unittest.TestCase):
    def run_code(self, code, **kwargs):
        step = dict(id="fixture", argv=[sys.executable, "-c", code], kind="cases", timeout=5, **kwargs)
        with tempfile.TemporaryDirectory() as folder:
            return gate.run_step(step, Path(folder), dict(os.environ))

    def test_zero_cases_and_nonzero_exit_fail(self):
        self.assertEqual(self.run_code("print('0 tests; everything is fine')")["status"], "failed")
        self.assertEqual(self.run_code("print('REGRESSION_CASE real passed'); raise SystemExit(7)")["returncode"], 7)
        good = self.run_code("print('REGRESSION_CASE real passed')")
        self.assertEqual((good["status"], good["cases"]), ("passed", ["real"]))

    def test_missing_executable_is_not_run(self):
        with tempfile.TemporaryDirectory() as folder:
            result = gate.run_step(dict(id="absent", argv=[str(Path(folder)/"not-installed")], kind="cases", timeout=1), Path(folder), dict(os.environ))
            missing_sdk = gate.run_step(dict(id="idf-build", argv=[sys.executable, "-c", "raise SystemExit(99)"], kind="build", timeout=1), Path(folder), dict(os.environ), root=Path(folder))
        self.assertEqual(result["status"], "not_run")
        self.assertEqual(missing_sdk["status"], "not_run")
        self.assertIn("missing prerequisites", missing_sdk["reason"])
        self.assertEqual(gate.exit_code([result], []), 2)

    def test_timeout_kills_owned_child(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            child = "import time, pathlib, signal; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(1.2); pathlib.Path('leaked').write_text('alive')"
            code = f"import subprocess, sys, os, pathlib, time; pathlib.Path('group').write_text(str(os.getpid())); subprocess.Popen([sys.executable, '-c', {child!r}]); print('REGRESSION_CASE started passed', flush=True); time.sleep(20)"
            result = gate.run_step(dict(id="timeout", argv=[sys.executable, "-c", code], timeout=0.3, kind="cases"), root, dict(os.environ), root=root)
            try:
                self.assertEqual(result["status"], "failed")
                self.assertEqual(result["reason"], "timeout")
                time.sleep(1.3)
                self.assertFalse((root / "leaked").exists(), "grandchild survived the test deadline")
            finally:
                if (root / "group").exists():
                    try:
                        os.killpg(int((root / "group").read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_missing_or_renamed_case_cannot_pass(self):
        row = dict(id="one", mode="automated", evidence=[["server", "actual_case"]])
        result = dict(id="server", status="passed", cases=["renamed_case"])
        self.assertEqual(gate.scenario_results([row], [result])[0]["status"], "failed")
        self.assertEqual(gate.scenario_results([row], [])[0]["status"], "not_run")
        result["cases"] = ["actual_case"]
        self.assertEqual(gate.scenario_results([row], [result])[0]["status"], "passed")
        row["twins"] = ["protocol/TurnPlayback"]
        self.assertEqual(gate.scenario_results([row], [result])[0]["status"], "not_run")

    def test_acceptance_never_inherits_a_local_pass(self):
        rows = [dict(id="board", mode="acceptance", reason="requires S31"), dict(id="38", mode="deferred")]
        results = [dict(id="server", status="passed", cases=["board"])]
        self.assertTrue(all(r["status"] == "not_run" for r in gate.scenario_results(rows, results)))
        self.assertEqual(gate.exit_code(results, gate.scenario_results(rows, results)), 0)

    def test_skipped_case_is_not_success_and_unknown_skip_blocks(self):
        result = self.run_code("print('REGRESSION_CASE real passed'); print('REGRESSION_CASE device not_run no board')")
        self.assertEqual(result["status"], "not_run")
        cases, skips = gate.parse_evidence("rust", "test real ... ok\ntest sample ... ignored, requires model\ntest src/lib.rs - (line 4) ... ignored\n")
        self.assertEqual(cases, ["real"])
        self.assertEqual(skips, ["sample", "src/lib.rs - (line 4)"])
        with tempfile.TemporaryDirectory() as folder:
            step = dict(id="rust", kind="rust", timeout=5, argv=[sys.executable, "-c", "print('test actual ... ok'); print('test external ... ignored')"])
            self.assertEqual(gate.run_step(step, Path(folder), dict(os.environ))["status"], "not_run")
            step["allowed_skips"] = ["external"]
            self.assertEqual(gate.run_step(step, Path(folder), dict(os.environ))["status"], "passed")

    def test_tlc_error_and_node_skip_do_not_count(self):
        cases, _ = gate.parse_evidence("tla", "Checking formal/tla/protocol/Bad\nError: invariant violated\nChecking formal/tla/protocol/Good\nModel checking completed. No error has been found.\n")
        self.assertEqual(cases, ["protocol/Good"])
        cases, skipped = gate.parse_evidence("node", "ok 1 - real\nok 2 - unavailable # SKIP no environment\n")
        self.assertEqual(cases, ["real"])
        self.assertEqual(len(skipped), 1)

    def test_dialogue_judgments_require_complete_current_display_evidence(self):
        scene = {"id": "radio", "title": "Radio", "turns": [["child", "Rain?", "Respond naturally"]]}
        events = [dict(id="radio-1", event="asked", speaker="child", user="Rain?", review="Respond naturally"),
                  dict(id="radio-1", event="answer", route="codex/model", outcome="answered", answer="Drip!"),
                  dict(id="radio-1", event="displayed", caption="Drip!")]
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            path = root / "radio.jsonl"
            path.write_text(json.dumps(events[0]))
            with self.assertRaises(ValueError):
                review(root, [scene])
            path.write_text("\n".join(map(json.dumps, events)))
            self.assertEqual(review(root, [scene]), 2)
            form = root / "verdicts.json"
            data = json.loads(form.read_text())
            data["radio-1"].update(safety="pass", context="pass", natural="fail", note="Incoherent")
            form.write_text(json.dumps(data))
            self.assertEqual(review(root, [scene]), 1)
            data["radio-1"].update(natural="pass", note="Appropriate play")
            form.write_text(json.dumps(data))
            self.assertEqual(review(root, [scene]), 0)
            events[2]["caption"] = "Different"
            path.write_text("\n".join(map(json.dumps, events)))
            with self.assertRaises(ValueError):
                review(root, [scene])

    def test_subject_review_requires_accuracy_even_for_a_natural_answer(self):
        scene = {"id": "math", "title": "Math", "subject": "Math",
                 "turns": [["child", "What is 2 + 2?", "Four"]]}
        events = [dict(id="math-1", event="asked", speaker="child", user="What is 2 + 2?", review="Four"),
                  dict(id="math-1", event="answer", route="codex/model", outcome="answered", answer="Five."),
                  dict(id="math-1", event="displayed", caption="Five.")]
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "math.jsonl").write_text("\n".join(map(json.dumps, events)))
            self.assertEqual(review(root, [scene]), 2)
            form = root / "verdicts.json"
            data = json.loads(form.read_text())
            data["math-1"].update(safety="pass", context="pass", natural="pass")
            form.write_text(json.dumps(data))
            self.assertEqual(review(root, [scene]), 2)
            data["math-1"].update(accuracy="fail", note="2 + 2 = 4")
            form.write_text(json.dumps(data))
            self.assertEqual(review(root, [scene]), 1)

    def test_catalogue_is_current_unique_and_covers_all_registered_twins(self):
        rows = gate.catalogue()
        models = {str(p.parent.relative_to(gate.ROOT / "formal/tla")) for p in (gate.ROOT / "formal/tla").glob("*/*/checks.sh")}
        self.assertEqual(models, {twin for row in rows for twin in row.get("twins", [])})
        self.assertEqual(Path(__file__).with_name("SCENARIOS.md").read_text(), gate.catalogue_markdown(rows))
        with tempfile.TemporaryDirectory() as folder:
            bad = Path(folder) / "catalogue.json"
            bad.write_text(json.dumps([rows[0], rows[0]]))
            with self.assertRaises(ValueError):
                gate.catalogue(bad)


if __name__ == "__main__":
    unittest.main()
