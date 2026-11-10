"""ScanTab's markers: a stage's scan shows the statistics the stage moved
by, which are of its outlier-filtered points, not of the points drawn;
the lines carry no text, their names in the stats line are their key.
The curve plots leave room above and below their points. A finished
scan's CEN, COM and MAX are buttons that move its motor there, while
SPEC is free."""

import math
import unittest
from unittest import mock

from silx.gui import qt

from bl5a_gui import docs, peak, plots

X = [float(i) for i in range(11)]
# A peak at 5, and at 1 a point the raw maximum, CEN and COM follow.
Y = [1, 100, 4, 8, 16, 30, 16, 8, 4, 2, 1]
FILTERED = Y[:1] + [2] + Y[2:]


class ScanTabTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt.QApplication.instance() or qt.QApplication([])

    def setUp(self):
        self.tab = plots.ScanTab()
        self.scan = docs.Scan("u", "ascan", "th", "det",
                              {"plan_args": {"start": 0, "finish": 10, "intervals": 10, "time": 1}})
        for x, y in zip(X, Y):
            self.scan.add({"th": x, "det": y})
        self.tab.start(self.scan)
        self.tab.stop(self.scan)

    def tearDown(self):
        self.tab.deleteLater()

    def lines(self):
        return [m for m in self.tab.plot.getItems() if hasattr(m, "getXPosition") and not hasattr(m, "getXData")]

    def marks(self):
        return {m.getLegend(): m.getXPosition() for m in self.lines()}

    def shown(self):
        """The stats line as it reads on screen."""
        doc = qt.QTextDocument()
        doc.setHtml(self.tab.stats.text())
        return doc.toPlainText()

    def assertClear(self, plot):
        lo, hi = plot.getYAxis().getLimits()
        self.assertLess(lo, min(Y))
        self.assertGreater(hi, max(Y))
        self.assertEqual(plot.getXAxis().getLimits(), (X[0], X[-1]))

    def test_points_clear_the_top_and_bottom(self):
        self.assertClear(self.tab.plot)
        stack = plots.StackTab(mock.Mock())
        self.addCleanup(stack.deleteLater)
        stack.profiles.addCurve(X, Y)
        self.assertClear(stack.profiles)

    def test_scan_without_stage_shows_its_points(self):
        self.assertEqual(self.marks()["CEN"], peak.stats(X, Y).cen)

    def test_stage_shows_its_own_statistics(self):
        f = peak.stats(X, FILTERED)
        self.assertNotEqual(f.cen, peak.stats(X, Y).cen)
        stage = {"pick": "CEN", "target": f.cen, "cen": f.cen, "com": f.com, "xmax": f.xpk, "fwhm": f.fwhm,
                 "ymax": f.ymax, "rejected": 1}
        self.tab.stage(self.scan, stage)
        m = self.marks()
        self.assertEqual((m["CEN"], m["COM"], m["MAX"], m["target"]), (f.cen, f.com, f.xpk, f.cen))
        self.assertIn("(1 outlier replaced)", self.shown())

    def test_lines_carry_no_text_and_the_stats_line_is_their_key(self):
        f = peak.stats(X, FILTERED)
        stage = {"pick": "CEN", "target": f.cen, "cen": f.cen, "com": f.com, "xmax": f.xpk, "fwhm": f.fwhm,
                 "ymax": f.ymax, "rejected": 0}
        self.tab.stage(self.scan, stage)
        self.assertEqual(sorted(self.marks()), ["CEN", "COM", "MAX", "target"])
        self.assertEqual([m.getText() for m in self.lines() if m.getText()], [])
        for _, name, color in plots.MARKERS:
            self.assertIn('<span style="color:{}">{}</span>'.format(color, name), self.tab.stats.text())
        self.assertIn('<span style="color:{}">→ CEN</span>'.format(plots.TARGET_COLOR), self.tab.stats.text())

    def test_a_reason_reads_as_written(self):
        self.scan.exit_status, self.scan.reason = "fail", "th <limit> & stopped"
        self.tab.start(self.scan)
        self.tab.stop(self.scan)
        self.assertTrue(self.shown().endswith("(fail: th <limit> & stopped)"), self.shown())
        self.assertEqual(self.shown(), self.tab.stats.toolTip())

    def test_dip_stage_names_its_minimum(self):
        f = peak.stats(X, FILTERED)
        stage = {"pick": "pl_xMAX", "dip": True, "target": f.xpk, "cen": f.cen, "com": f.com, "xmax": f.xpk,
                 "fwhm": f.fwhm, "ymax": f.ymax, "rejected": 0}
        self.tab.stage(self.scan, stage)
        m = self.marks()
        self.assertEqual((m["MIN"], m["target"]), (f.xpk, f.xpk))
        self.assertNotIn("MAX", m)
        self.assertIn("DIP {:.5g} at".format(f.ymax), self.shown())
        self.assertIn("→ pl_xMAX dip", self.shown())

    def test_stage_with_nothing_replaced_or_a_null_statistic(self):
        f = peak.stats(X, FILTERED)
        stage = {"pick": "CEN", "target": f.cen, "cen": f.cen, "com": None, "xmax": f.xpk, "fwhm": f.fwhm,
                 "ymax": f.ymax, "rejected": 0}
        self.tab.stage(self.scan, stage)
        self.assertNotIn("COM", self.marks())
        self.assertNotIn(">COM</span>", self.tab.stats.text(), "no key to a line not drawn")
        self.assertNotIn("outlier", self.shown())
        self.assertTrue(math.isnan(float(self.shown().split("COM ")[1].split()[0])))

    def sent(self, k):
        """Where clicking the button to `k` moves the motor."""
        out = []
        self.tab.moveRequested.connect(lambda motor, value: out.append((motor, value)))
        self.tab.moves[k].click()
        return out

    def test_a_move_waits_for_spec(self):
        cen = peak.stats(X, Y).cen
        self.assertEqual(self.tab.moves["cen"].text(), "th → CEN")
        self.assertEqual(self.sent("cen"), [], "SPEC is not free")
        self.tab.setAllowed(True)
        self.assertEqual(self.sent("cen"), [("th", cen)])
        self.assertEqual(self.tab.moves["cen"].toolTip(), "Move th to {:.10g}".format(cen))

    def test_a_running_scan_has_nothing_to_move_to(self):
        self.tab.setAllowed(True)
        self.tab.start(docs.Scan("v", "lup", "sy", "det", {}))
        self.assertEqual([b.isEnabled() for b in self.tab.moves.values()], [False] * 3)
        self.assertEqual(self.tab.moves["cen"].text(), "sy → CEN")

    def test_a_stage_moves_by_its_statistics_a_dip_to_its_minimum(self):
        f = peak.stats(X, FILTERED)
        stage = {"pick": "CEN", "dip": True, "target": f.cen, "cen": f.cen, "com": None, "xmax": f.xpk,
                 "fwhm": f.fwhm, "ymax": f.ymax, "rejected": 1}
        self.tab.setAllowed(True)
        self.tab.stage(self.scan, stage)
        self.assertEqual(self.tab.moves["xpk"].text(), "th → MIN")
        self.assertEqual(self.sent("xpk"), [("th", f.xpk)])
        self.assertFalse(self.tab.moves["com"].isEnabled(), "no COM to move to")

    def test_an_earlier_scan_picked_moves_its_own_motor(self):
        later = docs.Scan("v", "ascan", "sy", "det", {})
        for x, y in zip(X, Y[::-1]):
            later.add({"sy": x, "det": y})
        self.tab.start(later)
        self.tab.stop(later)
        self.tab.setAllowed(True)
        self.assertEqual(self.sent("xpk"), [("sy", peak.stats(X, Y[::-1]).xpk)])
        self.tab._show(0)
        self.assertEqual(self.sent("xpk"), [("th", peak.stats(X, Y).xpk)])

    def test_a_stage_that_set_its_motor_moves_in_the_new_positions(self):
        f = peak.stats(X, FILTERED)
        stage = {"pick": "CEN", "target": f.cen, "cen": f.cen, "com": f.com, "xmax": f.xpk, "fwhm": f.fwhm,
                 "ymax": f.ymax, "rejected": 1, "set": 0}
        self.tab.setAllowed(True)
        self.tab.stage(self.scan, stage)
        self.assertEqual(self.sent("cen"), [("th", 0.0)])
        self.assertEqual(self.sent("com"), [("th", f.com - f.cen)])
        self.assertIn("a stage's set has moved th by since", self.tab.moves["com"].toolTip())

    def scanned(self, motor, y):
        s = docs.Scan(motor + str(len(y)), "lup", motor, "det", {})
        for x, v in zip(X, y):
            s.add({motor: x, "det": v})
        self.tab.start(s)
        self.tab.stop(s)
        return s

    def test_a_later_set_of_the_motor_moves_an_earlier_scans_targets(self):
        sy, th = self.scanned("sy", Y), self.scanned("th", Y[::-1])
        self.tab.setAllowed(True)
        self.tab.scans.setCurrentIndex(0)
        self.tab._show(0)
        xpk = peak.stats(X, Y).xpk
        self.tab.stage(sy, {"pick": "CEN", "target": 3.0, "set": 0})
        self.assertEqual(self.sent("xpk"), [("th", xpk)], "sy's set is not th's")
        self.assertEqual(self.tab.moves["xpk"].toolTip(), "Move th to {:.10g}".format(xpk))
        self.tab.stage(th, {"pick": "CEN", "target": 2.5, "set": 1})
        self.assertEqual(self.sent("xpk"), [("th", xpk - 1.5)])
        self.assertIn("less the 1.5", self.tab.moves["xpk"].toolTip())

    def test_a_motor_not_in_spec_ioc_is_not_moved(self):
        other = docs.Scan("w", "ascan", "chi", "det", {})
        for x, y in zip(X, Y):
            other.add({"chi": x, "det": y})
        self.tab.start(other)
        self.tab.stop(other)
        self.tab.setAllowed(True)
        self.assertEqual([b.isEnabled() for b in self.tab.moves.values()], [False] * 3)


if __name__ == "__main__":
    unittest.main()
