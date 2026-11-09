"""The panes around the plots: each can shrink below its contents, the
layout outlives the window, Reset layout brings back the first one, and
no menu hides the run toolbar."""

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
        for area, tabbed, names in main.PANES:
            panes = [w.panes[name] for name in names]
            for pane in panes:
                self.assertFalse(pane.isFloating(), pane.objectName())
                self.assertEqual(w.dockWidgetArea(pane), area, pane.objectName())
                self.assertFalse(pane.isHidden(), pane.objectName())
            self.assertEqual(w.tabifiedDockWidgets(panes[0]), panes[1:] if tabbed else [], names)

    def test_menus_hold_the_panes_not_the_toolbars(self):
        w = self.window()
        actions = w.createPopupMenu().actions()
        for bar in w.findChildren(qt.QToolBar):
            self.assertNotIn(bar.toggleViewAction(), actions, bar.windowTitle())
        self.assertEqual([a for a in actions if not a.isSeparator()],
                         [p.toggleViewAction() for p in w.panes.values()] + [w.resetLayoutAct])


if __name__ == "__main__":
    unittest.main()
