import json
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from generate_face_themes import (generate, generate_expressions, SOURCE, TARGET,
                                  EXPRESSIONS_SOURCE, EXPRESSIONS_TARGET, PREVIEW_TARGET)


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

    def test_default_is_selected_by_id_not_array_position(self):
        data = json.loads(SOURCE.read_text(encoding="utf-8"))
        data["themes"].reverse()
        generated = generate(data)
        self.assertIn("#define FACE_DEFAULT_THEME_INDEX 5", generated)
        data["default_theme"] = "missing"
        with self.assertRaises(ValueError):
            generate(data)

    def test_eve_catalog_matches_native_and_browser(self):
        data = json.loads(EXPRESSIONS_SOURCE.read_text(encoding="utf-8"))
        native, browser = generate_expressions(data)
        self.assertEqual(native, EXPRESSIONS_TARGET.read_text(encoding="utf-8"))
        self.assertEqual(browser, PREVIEW_TARGET.read_text(encoding="utf-8"))
        runtime = (ROOT / "firmware/components/bot_runtime/bot_runtime.c").read_text()
        for expression in data["expressions"]:
            self.assertIn('"' + expression["id"] + '"', runtime)
        data["shapes"]["idle"][-1] += 1
        with self.assertRaises(ValueError):
            generate_expressions(data)


if __name__ == "__main__":
    unittest.main()
