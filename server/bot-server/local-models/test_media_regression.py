"""Supplementary user scenarios through the actual local media renderer."""
import json
from pathlib import Path
import tempfile
import unittest

from PIL import Image
import media_prepare as media


class MediaRegression(unittest.TestCase):
    def render(self, visual, **kwargs):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files, meta = media.render({"visual": visual, "options": {"print_pdf": False}, **kwargs}, root)
            with Image.open(root / "preview.jpg") as frame:
                self.assertEqual(frame.size, (480, 272))
            self.assertLessEqual((root / "preview.jpg").stat().st_size, 65536)
            self.assertTrue(all((root / f["name"]).stat().st_mode & 0o077 == 0 for f in files))
            return json.loads((root / "content.json").read_text()), meta

    def test_long_chinese_keeps_full_semantics_when_screen_is_truncated(self):
        body = "小朋友问：为什么树叶是绿色的？\n" * 60
        content, meta = self.render({"kind": "text", "title": "一起讨论", "body": body})
        self.assertTrue(meta["preview_truncated"])
        self.assertEqual(content["text"], body)

    def test_fraction_zero_full_and_number_line_negative_bounds(self):
        for numerator in (0, 3, 5):
            content, meta = self.render({"kind": "diagram", "diagram": {"type": "fraction", "numerator": numerator, "denominator": 5}})
            self.assertIn(f"{numerator}/5", content["text"])
            self.assertTrue(meta["visual_available"])
        content, _ = self.render({"kind": "diagram", "diagram": {"type": "number_line", "start": -3, "end": 7, "ticks": 6}})
        self.assertIn("-3 到 7", content["text"])

    def test_invalid_math_and_executable_diagram_never_render(self):
        for diagram in [
            {"type": "fraction", "numerator": 1, "denominator": 0},
            {"type": "fraction", "numerator": True, "denominator": 5},
            {"type": "fraction", "numerator": 1.5, "denominator": 5},
            {"type": "fraction", "numerator": 6, "denominator": 5},
            {"type": "number_line", "start": 7, "end": -3},
            {"type": "number_line", "start": float("nan"), "end": 3},
            {"type": "number_line", "start": 0, "end": 10, "ticks": 2.5},
            {"type": "javascript", "code": "throw new Error('never execute')"},
            {"type": "concept", "nodes": ["a"], "links": [{"from": 0, "to": 2}]},
        ]:
            with self.subTest(diagram=diagram), self.assertRaises(ValueError):
                self.render({"kind": "diagram", "diagram": diagram})

    def test_missing_stroke_data_is_explicit_not_invented_stroke_order(self):
        content, meta = self.render({"kind": "stroke", "character": "永"})
        self.assertFalse(meta["visual_available"])
        self.assertEqual(meta["fallback"], "glyph_preview")
        self.assertIn("笔顺资料暂不可用", content["text"])
        self.assertNotIn("frames", meta)

    def test_spelling_and_untrusted_text_boundaries(self):
        content, _ = self.render({"kind": "spelling", "word": "apple", "gloss": "苹果"})
        self.assertIn("apple", content["text"])
        self.assertIn("苹果", content["text"])
        for visual in [{"kind": "spelling", "word": "<script>"}, {"kind": "spelling", "word": "苹果"},
                       {"kind": "text", "body": "a" * 12001}, {"kind": "text", "steps": ["a"] * 21}]:
            with self.subTest(visual=str(visual)[:60]), self.assertRaises(ValueError):
                self.render(visual)


if __name__ == "__main__":
    unittest.main()
