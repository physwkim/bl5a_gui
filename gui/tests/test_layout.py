"""The panes around the plots: each can shrink below its contents, the
layout outlives the window, Reset layout brings back the first one, and
no menu hides the run toolbar. The alignment's panes are on the
Alignment tab only."""

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
        queue = w.panes["queue"]
        self.assertGreaterEqual(queue.widget().viewport().width(), w.queueView.minimumSizeHint().width(),
                                "the first layout shows the whole queue")
        w.resizeDocks([queue], [200], qt.Qt.Horizontal)
        self.app.processEvents()
        self.assertLess(queue.width(), w.queueView.minimumSizeHint().width())
        w.resizeDocks([queue], [1300], qt.Qt.Horizontal)
        self.app.processEvents()
        self.assertLess(w.centralWidget().width(), w.stackTab.minimumSizeHint().width())

    def test_the_first_layout_shows_every_tab_whole(self):
        w = self.window()
        self.assertGreaterEqual(w.centralWidget().viewport().width(), w.tabs.minimumSizeHint().width())
        self.assertEqual(w.centralWidget().horizontalScrollBar().maximum(), 0)

    def test_the_first_layout_at_the_gui_font_shows_the_left_panes_whole(self):
        font = self.app.font()
        self.addCleanup(self.app.setFont, font)
        self.app.setFont(qt.QFont(config.FONT, config.FONT_PT))
        w = self.window()
        for name in ("setup", "motors"):
            w.panes[name].raise_()
            self.app.processEvents()
            area = w.panes[name].widget()
            self.assertGreaterEqual(area.viewport().width(), area.widget().minimumSizeHint().width(), name)
        self.assertGreaterEqual(w.centralWidget().viewport().width(), w.tabs.minimumSizeHint().width())

    def test_layout_outlives_the_window_until_reset(self):
        w = self.window()
        w.panes["queue"].setFloating(True)
        w.panes["history"].close()
        w.close()
        w = self.window()
        self.assertTrue(w.panes["queue"].isFloating())
        self.assertFalse(w.panes["history"].isVisible())
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

    def test_a_layout_saved_before_the_tabs_is_not_restored(self):
        w = self.window()
        w.panes["queue"].setFloating(True)
        qt.QSettings(*config.SETTINGS).setValue("window/state", w.saveState())  # as saved before LAYOUT
        w = self.window()
        self.assertFalse(w.panes["queue"].isFloating())

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
                             [p.toggleViewAction() for p in w.panes.values()] + [w.resetLayoutAct])


if __name__ == "__main__":
    unittest.main()
