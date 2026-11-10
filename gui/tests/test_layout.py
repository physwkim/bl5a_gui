"""The panes around the queue and the plots window beside it: each can
shrink below its contents, the layout outlives the window, Reset layout
brings back the first one, and no menu hides the run toolbar. The
alignment's panes are on the Alignment tab only."""

import os
import tempfile
import unittest
from unittest import mock

from silx.gui import qt

from bl5a_gui import bridge, config, main

SERVICES = dict.fromkeys(("PVs", "Documents", "Commands", "QsPoller", "Integrator"), mock.DEFAULT)


class LayoutTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt.QApplication.instance() or qt.QApplication([])

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        ini = os.path.join(tmp.name, "bl5a-gui.conf")
        for patch in (mock.patch.object(config, "SETTINGS", (ini, qt.QSettings.IniFormat)),
                      mock.patch.multiple(bridge, **SERVICES)):
            patch.start()
            self.addCleanup(patch.stop)

    def window(self):
        w = main.MainWindow(main.parse([]))
        self.addCleanup(w.deleteLater)
        w.show()
        self.app.processEvents()
        return w

    def test_queue_and_plots_shrink_below_their_contents(self):
        w = self.window()
        queue = w.centralWidget()
        self.assertGreaterEqual(queue.viewport().width(), w.queueView.minimumSizeHint().width(),
                                "the first layout shows the whole queue")
        w.resizeDocks([w.panes["setup"]], [1300], qt.Qt.Horizontal)
        self.app.processEvents()
        self.assertLess(queue.width(), w.queueView.minimumSizeHint().width())
        w.plots.resize(300, 300)
        self.app.processEvents()
        self.assertLess(w.plots.centralWidget().width(), w.stackTab.minimumSizeHint().width())

    def test_the_first_layout_shows_every_tab_whole(self):
        w = self.window()
        self.assertGreaterEqual(w.plots.centralWidget().viewport().width(), w.tabs.minimumSizeHint().width())
        self.assertEqual(w.plots.centralWidget().horizontalScrollBar().maximum(), 0)

    def test_the_first_layout_at_the_gui_font_shows_the_panes_queue_tabs_and_status_whole(self):
        font = self.app.font()
        self.addCleanup(self.app.setFont, font)
        self.app.setFont(qt.QFont(config.FONT, config.FONT_PT))
        w = self.window()
        for name in ("setup", "motors"):
            w.panes[name].raise_()
            self.app.processEvents()
            area = w.panes[name].widget()
            self.assertGreaterEqual(area.viewport().width(), area.widget().minimumSizeHint().width(), name)
        self.assertGreaterEqual(w.centralWidget().viewport().width(), w.queueView.minimumSizeHint().width())
        self.assertGreaterEqual(w.plots.centralWidget().viewport().width(), w.tabs.minimumSizeHint().width())
        # A plan stopping after its frame, a temperature ramp and a frame's path.
        w.status.setQs({"re_state": "running", "running_item_name": "battery_cycle", "items_in_queue": 12,
                        "worker_environment_exists": True}, stopping=True)
        w.status.setTemperature(30.0, 25.12, 1.0)
        w.status.set("file", "/home/bl5a/data/2026-11-10/insitu_cell3/insitu_cell3_00012.mar3450")
        self.app.processEvents()
        for key, (_, label) in w.status.labels.items():
            self.assertGreaterEqual(label.width(), label.sizeHint().width(), key)

    def test_the_motors_are_over_the_queue_with_every_row_at_the_gui_font(self):
        font = self.app.font()
        self.addCleanup(self.app.setFont, font)
        self.app.setFont(qt.QFont(config.FONT, config.FONT_PT))
        w = self.window()
        m, s, q = (qt.QRect(x.mapTo(w, qt.QPoint(0, 0)), x.size())
                   for x in (w.panes["motors"], w.panes["setup"], w.centralWidget()))
        self.assertEqual((m.left(), m.width()), (q.left(), q.width()), "over the queue only")
        self.assertLess(m.bottom(), q.top())
        self.assertEqual(s.top(), m.top(), "Setup up to the toolbars beside them")
        area = w.panes["motors"].widget()
        self.assertGreaterEqual(area.viewport().height(), w.motors.minimumSizeHint().height())

    def test_the_tabs_are_in_a_window_of_their_own(self):
        w = self.window()
        self.assertIs(w.tabs.window(), w.plots)
        self.assertTrue(w.plots.isVisible())
        self.assertLessEqual(w.plots.frameGeometry().right(), w.screen().availableGeometry().right(),
                             "placed again with the frame the window got when shown")
        w.plots.close()
        self.assertTrue(w.isVisible())
        w.plotsAct.trigger()
        self.assertTrue(w.plots.isVisible())
        w.close()
        self.assertFalse(w.plots.isVisible(), "it closes with the window")

    def test_the_plots_window_is_beside_the_window_to_the_edge_of_the_screen(self):
        w = self.window()
        tabs = w.tabs.minimumSizeHint().width()
        for screen in (qt.QRect(0, 0, 3840, 2100), qt.QRect(0, 0, w.frameGeometry().width() + tabs // 2, 2100)):
            with mock.patch.object(w, "screen") as s:
                s.return_value.availableGeometry.return_value = screen
                w.defaultLayout()
            self.app.processEvents()
            plots, frame = w.plots.frameGeometry(), w.frameGeometry()
            room = screen.right() - frame.right() >= tabs
            self.assertEqual(plots.left() == frame.right() + 1, room, screen)
            self.assertEqual(plots.right(), screen.right(), screen)
            self.assertEqual((plots.top(), w.plots.height()), (frame.top(), w.height()), screen)
            self.assertGreaterEqual(w.plots.width(), tabs, screen)

    def test_layout_outlives_the_window_until_reset(self):
        w = self.window()
        w.panes["spec"].setFloating(True)
        w.panes["history"].close()
        w.plots.setGeometry(100, 120, 700, 500)
        self.app.processEvents()
        plots = w.plots.geometry()
        w.close()
        w = self.window()
        self.assertTrue(w.panes["spec"].isFloating())
        self.assertFalse(w.panes["history"].isVisible())
        self.assertEqual(w.plots.geometry(), plots)
        w.plots.close()
        w.resetLayoutAct.trigger()
        self.app.processEvents()
        self.app.sendPostedEvents(None, qt.QEvent.DeferredDelete)  # the tab bars the reset retired
        for host, area, tabbed, names in main.PANES:
            host = w.hosts[host]
            fronts = [b.tabText(b.currentIndex()) for b in host.findChildren(qt.QTabBar) if b.parent() is host]
            panes = [w.panes[name] for name in names]
            for pane in panes:
                self.assertFalse(pane.isFloating(), pane.objectName())
                self.assertEqual(host.dockWidgetArea(pane), area, pane.objectName())
                self.assertFalse(pane.isHidden(), pane.objectName())
            self.assertEqual(host.tabifiedDockWidgets(panes[0]), panes[1:] if tabbed else [], names)
            if tabbed:
                self.assertIn(panes[0].windowTitle(), fronts, "the first pane is the tab in front")
        self.assertTrue(w.plots.isVisible())

    def test_a_layout_saved_before_the_tabs_is_not_restored(self):
        w = self.window()
        w.panes["spec"].setFloating(True)
        qt.QSettings(*config.SETTINGS).setValue("window/state", w.saveState())  # as saved before LAYOUT
        w = self.window()
        self.assertFalse(w.panes["spec"].isFloating())

    def test_the_alignment_panes_show_on_its_tab_only(self):
        w = self.window()
        alignment = [w.panes["stages"], w.panes["history"]]
        for tab in (w.alignment, w.imageTab, w.stackTab, w.alignment):
            w.tabs.setCurrentWidget(tab)
            self.app.processEvents()
            name = w.tabs.tabText(w.tabs.currentIndex())
            self.assertEqual([p.isVisible() for p in alignment], [tab is w.alignment] * 2, name)

    def test_menus_hold_the_panes_not_the_toolbars(self):
        w = self.window()
        for host in w.hosts.values():
            actions = host.createPopupMenu().actions()
            for bar in w.findChildren(qt.QToolBar):
                self.assertNotIn(bar.toggleViewAction(), actions, bar.windowTitle())
            self.assertEqual([a for a in actions if not a.isSeparator()],
                             [p.toggleViewAction() for p in w.panes.values()] + [w.plotsAct, w.resetLayoutAct])


if __name__ == "__main__":
    unittest.main()
