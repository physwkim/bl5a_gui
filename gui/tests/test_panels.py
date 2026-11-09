"""panels: the status line's qs label."""

import unittest

from silx.gui import qt

from bl5a_gui import panels

app = qt.QApplication.instance() or qt.QApplication([])


class QsLabelTest(unittest.TestCase):
    def test_environment_closed_shows_the_manager_state(self):
        bar = panels.StatusBar()
        bar.setQs({"re_state": "null", "manager_state": "environment_closed", "items_in_queue": 2,
                   "worker_environment_exists": False})
        text = bar.labels["qs"][1].text()
        self.assertIn("environment_closed, 2 queued, environment closed", text)
        self.assertNotIn("null", text)


if __name__ == "__main__":
    unittest.main()
