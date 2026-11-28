"""Explicit local-only modules: never discover opt-in Graphiti/model tests."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, ".")
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "server/bot-server/scripts"))


class Evidence(unittest.TextTestResult):
    def addSuccess(self, test):
        super().addSuccess(test)
        print(f"REGRESSION_CASE {test.id()} passed", flush=True)

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        print(f"REGRESSION_CASE {test.id()} not_run {reason}", flush=True)


suite = unittest.defaultTestLoader.loadTestsFromNames(["test_speech", "test_qwen_robot", "test_relocate_data_root", "test_teaching_media", "test_media_regression", "test_worker_contracts"])
result = unittest.TextTestRunner(verbosity=2, resultclass=Evidence).run(suite)
raise SystemExit(0 if result.wasSuccessful() else 1)
