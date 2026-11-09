"""The Calibration tab: the calibrants' d spacings are pyFAI's, Refine
sends what the tab shows, a result is taken only for the calibrate the
tab is waiting on (also when it is published before the reply names
it), a pick snaps to the brightest pixel, and Save and apply goes the
Integration box's way to a `set`."""

import math
import os
import tempfile
import unittest
from unittest import mock

import numpy as np
from silx.gui import qt
from silx.gui.utils.testutils import TestCaseQt

from bl5a_gui import bridge, calib, config, main, panels, plots
from bl5a_gui.calib import CalibrationTab

PONI = """poni_version: 2.1
Detector: Detector
Detector_config: {"pixel1": 0.001, "pixel2": 0.001, "max_shape": [345, 345], "orientation": 3}
Distance: 0.3
Poni1: 0.1725
Poni2: 0.1725
Rot1: 0.0
Rot2: 0.0
Rot3: 0.0
Wavelength: 7e-11
"""
REFINED = PONI.replace("Distance: 0.3", "Distance: 0.30123").replace("Rot1: 0.0", "Rot1: 0.001")
FRAME = "/d/lab6_0001.mar3450"


class Integrator(qt.QObject):
    reply = qt.Signal(str, object)
    published = qt.Signal(str, object)
    failed = qt.Signal(str, str)

    def __init__(self):
        super().__init__()
        self.requests = []

    def request(self, method, **params):
        self.requests.append((method, params))


def _settings(test):
    tmp = tempfile.TemporaryDirectory()
    test.addCleanup(tmp.cleanup)
    patch = mock.patch.object(config, "SETTINGS", (os.path.join(tmp.name, "bl5a-gui.conf"), qt.QSettings.IniFormat))
    patch.start()
    test.addCleanup(patch.stop)
    return tmp.name


class DSpacingTest(unittest.TestCase):
    # The first three d of pyFAI's LaB6.D, CeO2.D, Si.D and Al.D.
    PYFAI = {"LaB6": (4.15682600, 2.93931985, 2.39994461), "CeO2": (3.12441816, 2.70582550, 1.91330756),
             "Si": (3.13570166, 1.92021727, 1.63756679), "Al": (2.33797992, 2.02475000, 1.43171446)}

    def test_the_calibrants_are_pyfais(self):
        for name, first in self.PYFAI.items():
            ds = calib.d_spacings(*calib.CALIBRANTS[name])
            for got, want in zip(ds, first):
                self.assertAlmostEqual(got / want, 1.0, delta=1e-7, msg=name)

    def test_diamond_drops_the_even_hkl_not_a_multiple_of_4(self):
        a, lattice = calib.CALIBRANTS["Si"]
        ns = {round((a / d) ** 2) for d in calib.d_spacings(a, lattice)}
        self.assertTrue({3, 8, 11, 16, 19, 24} <= ns)
        self.assertFalse({4, 12} & ns, "200 and 222 are forbidden in diamond")

    def test_primitive_takes_every_n_but_7(self):
        ns = [round((2.0 / d) ** 2) for d in calib.d_spacings(2.0, "P")]
        self.assertEqual(ns[:8], [1, 2, 3, 4, 5, 6, 8, 9], "7 is no sum of three squares")
        self.assertEqual(ns, sorted(ns))

    def test_read_d(self):
        self.assertEqual(calib.read_d("# LaB6\n4.1568 100\n\n2.9393  # 110\n4.1568\n"), [4.1568, 2.9393])
        for text in ("", "# only a comment\n", "0.0\n", "-1.2\n", "hkl\n"):
            with self.assertRaises(ValueError, msg=repr(text)):
                calib.read_d(text)


class CalibrationTabTest(TestCaseQt):
    def setUp(self):
        super().setUp()
        self.dir = _settings(self)
        self.integrator = Integrator()
        self.tab = CalibrationTab(self.integrator)
        self.addCleanup(self.tab.deleteLater)
        self.image = np.zeros((345, 345), dtype=np.int32)

    def ready(self):
        """A frame and the integrator's PONI as the start."""
        self.tab._loaded(FRAME, self.image, 1e-3)
        self.integrator.published.emit("config", {"poni": PONI})

    def refine(self):
        self.tab.refineButton.click()
        return self.integrator.requests[-1]

    def calibration(self, id, poni=REFINED, **body):
        body = dict({"id": id, "poni": poni, "chi2": 1e-9, "points": [[10, 20, 0], [30, 40, 1]],
                     "path": FRAME}, **body)
        self.integrator.published.emit("calibration", body)

    def test_refine_sends_what_the_tab_shows(self):
        self.ready()
        self.tab.picks = [(1, 2, 0)]
        self.tab.upper.setValue(55000)
        self.tab.fixed["dist"].setChecked(True)
        method, params = self.refine()
        self.assertEqual(method, "calibrate")
        self.assertEqual(params["path"], FRAME)
        self.assertEqual(params["poni"], PONI)
        ds = [float(line) for line in params["calibrant"].splitlines()]
        self.assertEqual(ds, calib.d_spacings(*calib.CALIBRANTS["LaB6"]), "repr round-trips")
        self.assertEqual((params["rings"], params["delta_tth"], params["factor"], params["upper"]),
                         (15, 0.1, 1.0, 55000.0))
        self.assertEqual(params["fixed"], ["dist", "rot3"])
        self.assertEqual(params["points"], [[1, 2, 0]])

    def test_no_upper_limit_sends_one_above_any_count(self):
        self.ready()
        self.assertEqual(self.refine()[1]["upper"], calib.NO_LIMIT)

    def test_refine_without_a_frame_or_start_sends_nothing(self):
        self.tab.refineButton.click()
        self.tab._loaded(FRAME, self.image, 1e-3)
        self.tab.refineButton.click()
        self.assertEqual(self.integrator.requests, [])
        self.integrator.published.emit("config", {"poni": PONI})
        self.tab.calibrant.setCurrentText(calib.D_FILE)
        self.tab.refineButton.click()
        self.assertEqual(self.integrator.requests, [])
        self.assertIn("no .D file", self.tab.state.text())

    def test_a_result_is_taken_for_its_own_id_only(self):
        self.ready()
        self.refine()
        self.integrator.reply.emit("calibrate", {"calibrating": 7})
        self.calibration(6)
        self.assertIsNone(self.tab.result)
        self.calibration(7)
        self.assertEqual(self.tab.result["text"], REFINED)
        self.assertIsNone(self.tab.current)
        self.assertTrue(self.tab.saveButton.isEnabled())
        self.assertEqual(self.tab.ringsOf.currentText(), calib.REFINED)
        self.assertEqual(self.tab.table.item(0, 1).text(), "301.2300")
        self.assertEqual(self.tab.table.item(0, 2).text(), "+1.2300")
        self.assertEqual(self.tab.table.item(3, 2).text(), "+{:.5f}".format(math.degrees(0.001)))
        self.assertIn("2 peaks (ring 1: 1; ring 2: 1)", self.tab.state.text())
        peaks = self.tab.plot.getCurve("peaks")
        self.assertEqual(list(peaks.getXData()), [20.5, 40.5])
        self.assertEqual(list(peaks.getYData()), [10.5, 30.5])

    def test_a_result_published_before_its_reply_is_kept(self):
        self.ready()
        self.refine()
        self.calibration(7)
        self.assertIsNone(self.tab.result)
        self.integrator.reply.emit("calibrate", {"calibrating": 7})
        self.assertEqual(self.tab.result["text"], REFINED)

    def test_a_second_refine_supersedes_the_first(self):
        self.ready()
        self.refine()
        self.refine()
        self.integrator.reply.emit("calibrate", {"calibrating": 1})
        self.integrator.reply.emit("calibrate", {"calibrating": 2})
        self.calibration(1)
        self.assertIsNone(self.tab.result)
        self.calibration(2)
        self.assertEqual(self.tab.result["text"], REFINED)

    def test_a_refused_refine_ends_the_wait(self):
        self.ready()
        self.refine()
        self.integrator.failed.emit("calibrate", "busy")
        self.assertIsNone(self.tab.current)
        self.assertEqual(self.tab.state.text(), "Refused: busy")
        self.refine()
        self.integrator.reply.emit("calibrate", {"calibrating": 3})
        self.calibration(3)
        self.assertEqual(self.tab.result["text"], REFINED, "the refusal took the first run's place only")

    def test_an_error_is_said(self):
        self.ready()
        self.refine()
        self.integrator.reply.emit("calibrate", {"calibrating": 4})
        self.integrator.published.emit("calibration", {"id": 4, "error": "ring 1: no peaks"})
        self.assertIsNone(self.tab.result)
        self.assertIsNone(self.tab.current)
        self.assertEqual(self.tab.state.text(), "Failed: ring 1: no peaks")
        self.assertFalse(self.tab.saveButton.isEnabled())

    def test_the_start_follows_the_integrator_only_while_it_is_the_integrators(self):
        self.ready()
        self.assertEqual(self.tab.start[:2], (calib.INTEGRATOR, PONI))
        self.integrator.published.emit("config", {"poni": REFINED})
        self.assertEqual(self.tab.start[1], REFINED)
        path = os.path.join(self.dir, "start.poni")
        with open(path, "w") as f:
            f.write(PONI)
        with mock.patch.object(qt.QFileDialog, "getOpenFileName", return_value=(path, "")):
            self.tab.loadPoni.click()
        self.assertEqual((self.tab.startLabel.text(), self.tab.startLabel.toolTip()), ("start.poni", path))
        self.integrator.reply.emit("config", {"poni": REFINED})
        self.assertEqual(self.tab.start[:2], (path, PONI))
        self.tab.fromIntegrator.click()
        self.assertEqual(self.tab.start[:2], (calib.INTEGRATOR, REFINED))

    def test_rings_are_drawn_up_to_the_number_searched(self):
        self.ready()
        self.tab.rings.setValue(3)
        labels = sorted(i.getName() for i in self.tab.plot.getItems() if i.getName().endswith(" label"))
        self.assertEqual(labels, ["ring 1 label", "ring 2 label", "ring 3 label"])
        self.tab.ringsOf.setCurrentText(calib.NO_RINGS)
        self.tab.ringsOf.activated.emit(self.tab.ringsOf.currentIndex())
        self.assertFalse([i for i in self.tab.plot.getItems() if i.getName().startswith("ring ")])

    def test_a_click_picks_the_brightest_pixel_near_it(self):
        self.image[100, 103] = 50
        self.image[100, 110] = 90  # out of SNAP of the click
        self.ready()
        self.tab.pickRing.setValue(3)
        click = {"event": "mouseClicked", "button": "left", "x": 101.2, "y": 98.7, "xpixel": 0, "ypixel": 0}
        self.tab.plot.sigPlotSignal.emit(click)
        self.assertEqual(self.tab.picks, [], "Pick is up")
        self.tab.pick.setChecked(True)
        self.tab.plot.sigPlotSignal.emit(dict(click, button="right"))
        self.tab.plot.sigPlotSignal.emit(click)
        self.assertEqual(self.tab.picks, [(100, 103, 2)])
        self.assertEqual(self.tab.pickCount.text(), "ring 3: 1")
        self.assertEqual(list(self.tab.plot.getCurve("picks").getXData()), [103.5])
        self.tab._loaded(FRAME, self.image, 1e-3)
        self.assertEqual(self.tab.picks, [], "a new frame drops the picks")
        self.assertIsNone(self.tab.plot.getCurve("picks"))

    def test_a_real_click_in_zoom_mode_picks(self):
        self.image[200, 150] = 50
        self.ready()
        self.tab.resize(1200, 800)
        self.tab.show()
        self.qWaitForWindowExposed(self.tab)
        self.tab.pick.setChecked(True)
        widget = self.tab.plot.getWidgetHandle()
        x, y = self.tab.plot.dataToPixel(150.5, 200.5)
        self.mouseClick(widget, qt.Qt.LeftButton, pos=(x, y))
        self.qapp.processEvents()
        self.assertEqual(self.tab.picks, [(200, 150, 0)])

    def test_the_side_shows_every_cell_whole_however_far_it_is_squeezed(self):
        self.ready()
        self.tab.resize(600, 900)
        self.tab.show()
        self.tab.plot.parentWidget().setSizes([150, 450])
        self.qWaitForWindowExposed(self.tab)
        # The result widens the panel after the side has its width.
        self.refine()
        self.integrator.reply.emit("calibrate", {"calibrating": 5})
        self.calibration(5, poni=PONI.replace("Rot1: 0.0", "Rot1: -0.0039"))
        for _ in range(4):  # the widening passes up table, panel, side and splitter
            self.qapp.processEvents()
        side = self.tab.plot.parentWidget().widget(0)
        self.assertGreaterEqual(side.viewport().width(), side.widget().minimumSizeHint().width())
        table, header = self.tab.table, self.tab.table.horizontalHeader()
        for j in range(table.columnCount()):
            self.assertGreaterEqual(table.columnWidth(j), max(table.sizeHintForColumn(j), header.sectionSizeHint(j)),
                                    table.horizontalHeaderItem(j).text())
        self.assertGreaterEqual(table.viewport().width(), header.length())

    def test_the_side_widens_with_its_panel_alone(self):
        # The tab's plot redraws, and so stirs the splitter, whenever the
        # table changes; beside a still widget only the side can tell it.
        label = qt.QLabel("short")
        panel = qt.QWidget()
        qt.QVBoxLayout(panel).addWidget(label)
        split = qt.QSplitter(qt.Qt.Horizontal)
        self.addCleanup(split.deleteLater)
        side = calib._Side(panel)
        split.addWidget(side)
        split.addWidget(qt.QWidget())
        split.resize(800, 300)
        split.show()
        split.setSizes([10, 790])
        self.qWaitForWindowExposed(split)
        label.setText("a label as wide as a table full of numbers")
        for _ in range(4):
            self.qapp.processEvents()
        self.assertGreaterEqual(side.viewport().width(), panel.minimumSizeHint().width())

    def test_save_writes_the_refined_poni_and_asks_for_it_applied(self):
        self.ready()
        self.refine()
        self.integrator.reply.emit("calibrate", {"calibrating": 5})
        self.calibration(5)
        path = os.path.join(self.dir, "refined.poni")
        applied = []
        self.tab.applyRequested.connect(applied.append)
        with mock.patch.object(qt.QFileDialog, "getSaveFileName", return_value=(path, "")) as ask:
            self.tab.saveButton.click()
        self.assertEqual(ask.call_args[0][2], "/d/lab6_0001.poni")
        with open(path) as f:
            self.assertEqual(f.read(), REFINED)
        self.assertEqual(applied, [path])


class DialogTest(TestCaseQt):
    def setUp(self):
        super().setUp()
        self.dir = _settings(self)
        qt.QSettings(*config.SETTINGS).setValue("data_root", self.dir)
        self.tab = CalibrationTab(Integrator())
        self.image = plots.ImageTab()
        self.box = panels.IntegrationBox(Integrator())
        for w in (self.tab, self.image, self.box):
            self.addCleanup(w.deleteLater)
        self.dialogs = {"frame": self.tab._open, "start PONI": self.tab._loadPoni, "d spacings": self.tab._browseD,
                        "image": self.image._open, "integration PONI": self.box._browse}

    def opened(self):
        where = {}
        for name, browse in self.dialogs.items():
            with mock.patch.object(qt.QFileDialog, "getOpenFileName", return_value=("", "")) as ask:
                browse()
            where[name] = ask.call_args[0][2]
        return where

    def test_a_dialog_opens_in_the_data_folder_until_a_file_is_named(self):
        self.assertEqual(self.opened(), dict.fromkeys(self.dialogs, self.dir))
        image = np.zeros((345, 345), dtype=np.int32)
        self.tab._loaded(FRAME, image, 1e-3)
        self.image._loaded(FRAME, image, 1e-3)
        self.tab.dPath.setText("/c/x.D")
        self.box.poni.setText("/p/x.poni")
        self.assertEqual(self.opened(), {"frame": "/d", "start PONI": self.dir, "d spacings": "/c/x.D", "image": "/d",
                                         "integration PONI": "/p/x.poni"})


class ApplyTest(TestCaseQt):
    def setUp(self):
        super().setUp()
        self.dir = _settings(self)
        self.path = os.path.join(self.dir, "refined.poni")
        with open(self.path, "w") as f:
            f.write(REFINED)

    def test_apply_file_sends_set(self):
        integrator = Integrator()
        box = panels.IntegrationBox(integrator)
        self.addCleanup(box.deleteLater)
        box.npt.setValue(1500)
        box.applyFile(" {} ".format(self.path))
        self.assertEqual(integrator.requests, [("set", {"poni": REFINED, "npt": 1500})])
        self.assertEqual(box.poni.text(), self.path)
        self.assertEqual(qt.QSettings(*config.SETTINGS).value("poni"), self.path)

    def test_the_window_applies_what_the_tab_saved(self):
        patch = mock.patch.multiple(bridge, **dict.fromkeys(("PVs", "Documents", "Commands", "QsPoller", "Integrator"),
                                                            mock.DEFAULT))
        patch.start()
        self.addCleanup(patch.stop)
        w = main.MainWindow(main.parse([]))
        self.addCleanup(w.deleteLater)
        w.calibTab.applyRequested.emit(self.path)
        w.integrator.request.assert_called_with("set", poni=REFINED, npt=w.integration.npt.value())


if __name__ == "__main__":
    unittest.main()
