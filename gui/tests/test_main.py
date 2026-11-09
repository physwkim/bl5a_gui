"""The GUI's command line."""

import unittest

from bl5a_gui import config, main


class ParseTest(unittest.TestCase):
    def test_font_pt_defaults_to_config(self):
        self.assertEqual(main.parse([]).font_pt, config.FONT_PT)
        self.assertEqual(config.FONT_PT, 12)

    def test_font_pt_option(self):
        self.assertEqual(main.parse(["--font-pt", "9"]).font_pt, 9)


if __name__ == "__main__":
    unittest.main()
