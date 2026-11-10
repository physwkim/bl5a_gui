"""The GUI's command line, and the scan plot's moves: through the motor
record, while SPEC is free; one that fails says why in Messages."""

import os
import tempfile
import unittest
from unittest import mock

from silx.gui import qt

from bl5a_gui import bridge, config, docs, main, peak


class ParseTest(unittest.TestCase):
    def test_font_pt_defaults_to_config(self):
        self.assertEqual(main.parse([]).font_pt, config.FONT_PT)
        self.assertEqual(config.FONT_PT, 12)

    def test_font_pt_option(self):
        self.assertEqual(main.parse(["--font-pt", "9"]).font_pt, 9)


class ScanMoveTest(unittest.TestCase):
    def setUp(self):
        self.app = qt.QApplication.instance() or qt.QApplication([])
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        ini = os.path.join(tmp.name, "gui.conf")
        for patch in (mock.patch.object(config, "SETTINGS", (ini, qt.QSettings.IniFormat)),
                      mock.patch.multiple(bridge, **dict.fromkeys(("PVs", "Documents", "Commands", "QsPoller",
                                                                   "Integrator"), mock.DEFAULT))):
            patch.start()
            self.addCleanup(patch.stop)
        self.w = main.MainWindow(main.parse([]))
        self.addCleanup(self.w.deleteLater)

    def moved(self, msta, error=None):
        """A th → CEN click whose put ended with `error`, then MSTA read `msta`."""
        w = self.w
        w.pvs.get_text.side_effect = lambda name: {config.SPEC + "th.MSTA": msta}[name]
        w.pvs.put.side_effect = lambda name, value, callback=None: callback(error)
        w.scanTab.setAllowed(True)
        w.scanTab.at = (docs.Scan("u", "lup", "th", "det", {}),
                        peak.Stats(xpk=0.1, ymax=5.0, com=0.2, cen=0.25, fwhm=0.05), False)
        w.scanTab._buttons()
        w.scanTab.moves["cen"].click()
        w.pvs.put.assert_called_once_with(config.SPEC + "th.VAL", 0.25, callback=mock.ANY)
        return w.messages.out.toPlainText()

    def test_a_move_goes_to_the_motor_record(self):
        self.assertEqual(self.moved("2"), "")

    def test_a_refused_move_says_so(self):
        self.assertTrue(self.moved(str(0x2 | config.MSTA_PROBLEM)).endswith(
            "th move: SPEC refused it; the SPEC pane says why"))

    def test_a_failed_put_says_why(self):
        self.assertTrue(self.moved("", "Put operation failed").endswith("th move: Put operation failed"))

    def test_the_gate_holds_the_moves(self):
        with mock.patch.object(main.gate, "allowed", return_value=dict.fromkeys(
                ("spec", "pause", "resume", "stop", "abort", "stop_after_frame"), False)):
            self.w.scanTab.setAllowed(True)
            self.w._gate()
        self.assertFalse(self.w.scanTab.allowed)


if __name__ == "__main__":
    unittest.main()
