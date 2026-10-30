import json
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from generate_face_themes import generate, SOURCE, TARGET


class PaletteSourceTest(unittest.TestCase):
    def test_generated_header_is_fresh(self):
        data = json.loads(SOURCE.read_text(encoding="utf-8"))
        self.assertEqual(generate(data), TARGET.read_text(encoding="utf-8"))

    def test_palettes_share_eye_colour(self):
        data = json.loads(SOURCE.read_text(encoding="utf-8"))
        for theme in data["themes"]:
            self.assertEqual(theme["eye"], theme["pupil"])
            self.assertNotEqual(theme["background"], theme["eye"])

    def test_duplicate_ids_rejected(self):
        data = json.loads(SOURCE.read_text(encoding="utf-8"))
        data["themes"].append(dict(data["themes"][0]))
        with self.assertRaises(ValueError):
            generate(data)


if __name__ == "__main__":
    unittest.main()
