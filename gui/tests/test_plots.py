"""ScanTab's markers: a stage's scan shows the statistics the stage moved
by, which are of its outlier-filtered points, not of the points drawn.
The curve plots leave room above and below their points."""

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

    def marks(self):
        return {m.getLegend(): m.getXPosition() for m in self.tab.plot.getItems()
                if hasattr(m, "getXPosition") and not hasattr(m, "getXData")}

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
        self.assertIn("(1 outlier replaced)", self.tab.stats.text())

    def test_dip_stage_names_its_minimum(self):
        f = peak.stats(X, FILTERED)
        stage = {"pick": "pl_xMAX", "dip": True, "target": f.xpk, "cen": f.cen, "com": f.com, "xmax": f.xpk,
                 "fwhm": f.fwhm, "ymax": f.ymax, "rejected": 0}
        self.tab.stage(self.scan, stage)
        m = self.marks()
        self.assertEqual((m["MIN"], m["target"]), (f.xpk, f.xpk))
        self.assertNotIn("MAX", m)
        self.assertIn("DIP {:.5g} at".format(f.ymax), self.tab.stats.text())
        self.assertIn("→ pl_xMAX dip", self.tab.stats.text())

    def test_stage_with_nothing_replaced_or_a_null_statistic(self):
        f = peak.stats(X, FILTERED)
        stage = {"pick": "CEN", "target": f.cen, "cen": f.cen, "com": None, "xmax": f.xpk, "fwhm": f.fwhm,
                 "ymax": f.ymax, "rejected": 0}
        self.tab.stage(self.scan, stage)
        self.assertNotIn("COM", self.marks())
        self.assertNotIn("outlier", self.tab.stats.text())
        self.assertTrue(math.isnan(float(self.tab.stats.text().split("COM ")[1].split()[0])))


if __name__ == "__main__":
    unittest.main()
