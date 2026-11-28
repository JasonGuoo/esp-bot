"""Repeatable numerical and actual Pillow regressions; no cloud or device calls.
Run with the configured model Python: python -m unittest test_teaching_media -v
"""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import media_prepare
import teaching_render
from PIL import Image


class TeachingMediaTests(unittest.TestCase):
    def setUp(self):
        self.raw = teaching_render.SCENE.read_bytes()
        self.scene = json.loads(self.raw)
        self.digest = hashlib.sha256(self.raw).hexdigest()

    def test_mapping_plane_kernel_and_classification_share_one_scene(self):
        proof = teaching_render.validate_scene(self.scene)
        self.assertEqual(proof, {"mapping": True, "classification": True, "kernel_pairs": 64})
        self.assertFalse(self.scene["trained"])
        self.assertEqual({p["id"] for p in self.scene["points"]}, set("ABCDEFGH"))
        # Opposite inner/outer points rule out linear separation in the input
        # plane: each class has the same mean at the origin.
        for label in (-1, 1):
            points = [p for p in self.scene["points"] if p["label"] == label]
            self.assertEqual(sum(p["x"] for p in points), 0)
            self.assertEqual(sum(p["y"] for p in points), 0)

    def test_wrong_mapping_labels_identity_and_nonfinite_coordinates_fail(self):
        for key, value in (("z", 2), ("label", 1), ("x", float("nan")), ("x", True), ("id", "B")):
            scene = copy.deepcopy(self.scene)
            scene["points"][0][key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                teaching_render.validate_scene(scene)
        for key, value in (("plane_z", 4), ("version", 2), ("trained", True), ("example_only", False)):
            scene = copy.deepcopy(self.scene)
            scene[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                teaching_render.validate_scene(scene)

    def test_actual_renderer_has_four_bounded_frames_and_frozen_semantics(self):
        with tempfile.TemporaryDirectory() as root:
            digests = set()
            for step in self.scene["steps"]:
                output = Path(root) / step["stage"]
                output.mkdir()
                visual = {"kind": "diagram", "title": step["title"], "body": "固定教学示例",
                          "diagram": {"type": "svm_radial", "stage": step["stage"], "scene_digest": self.digest}}
                files, metadata = media_prepare.render({"visual": visual, "options": {"print_pdf": False}}, output)
                preview = output / "preview.jpg"
                with Image.open(preview) as frame:
                    self.assertEqual(frame.size, (480, 272))
                    self.assertEqual(frame.format, "JPEG")
                self.assertLessEqual(preview.stat().st_size, 65536)
                self.assertEqual(metadata["frozen_scene"], self.scene)
                self.assertEqual(metadata["scene_digest"], self.digest)
                self.assertEqual(metadata["stage"], step["stage"])
                self.assertEqual(json.loads((output / "content.json").read_text())["text"], step["speech"])
                self.assertEqual(len(files), 2)
                digests.add(hashlib.sha256(preview.read_bytes()).hexdigest())
            self.assertEqual(len(digests), 4)

    def test_unknown_stage_or_wrong_scene_digest_cannot_render(self):
        for stage, digest in (("original", "0"*64), ("invented", self.digest)):
            with tempfile.TemporaryDirectory() as output, self.assertRaises(ValueError):
                media_prepare.render({"visual": {"kind": "diagram", "diagram": {
                    "type": "svm_radial", "stage": stage, "scene_digest": digest}}}, Path(output))


if __name__ == "__main__":
    unittest.main()
