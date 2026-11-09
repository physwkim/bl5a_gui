"""StackTab follows the rows as they are integrated, as ImageTab follows
the frames: a row of another stack, listed or new, brings that stack up,
unless Follow is off. A row that lands while its stack's rows are on
their way is not lost."""

import unittest

from silx.gui import qt

from bl5a_gui import plots


class Integrator(qt.QObject):
    reply = qt.Signal(str, object)
    published = qt.Signal(str, object)

    def __init__(self, stacks):
        super().__init__()
        self.stacks = stacks  # path -> rows
        self.held = None  # replies made but not delivered, while holding

    def request(self, method, **params):
        if method == "stacks":
            body = {"stacks": [{"path": p, "series": p, "rows": len(r)} for p, r in self.stacks.items()]}
        elif method == "history":
            path, start = params["path"], params.get("from", 0)
            rows = self.stacks[path][start:]
            body = {"path": path, "from": start, "q": [1.0, 2.0], "unit": "q_A^-1",
                    "frames": [{"th": 0.1}] * len(rows), "intensity": rows}
        else:
            return
        if self.held is not None:
            self.held.append((method, body))
        else:
            self.reply.emit(method, body)

    def release(self):
        held, self.held = self.held, None
        for method, body in held:
            self.reply.emit(method, body)

    def row(self, path):
        self.stacks.setdefault(path, []).append([1.0, 1.0])
        self.published.emit("row", {"path": path, "index": len(self.stacks[path]) - 1})


class StackTabTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = qt.QApplication.instance() or qt.QApplication([])

    def setUp(self):
        self.integrator = Integrator({"/d/a.h5": [[1.0, 1.0]], "/d/b.h5": [[1.0, 1.0]]})
        self.tab = plots.StackTab(self.integrator)
        self.addCleanup(self.tab.deleteLater)
        self.tab.reload()
        self.tab.series.setCurrentIndex(self.tab.series.findData("/d/a.h5"))
        self.tab.series.activated.emit(self.tab.series.currentIndex())

    def shown(self):
        path = self.tab.current()
        return path, len(self.tab.data[path]["rows"])

    def test_a_new_stack_comes_up(self):
        self.integrator.row("/d/c.h5")
        self.assertEqual(self.shown(), ("/d/c.h5", 1))
        self.integrator.row("/d/c.h5")
        self.assertEqual(self.shown(), ("/d/c.h5", 2))

    def test_a_listed_stack_comes_up(self):
        self.integrator.row("/d/b.h5")
        self.assertEqual(self.shown(), ("/d/b.h5", 2))

    def test_without_follow_the_stack_shown_stays(self):
        self.tab.follow.setChecked(False)
        self.integrator.row("/d/b.h5")
        self.integrator.row("/d/c.h5")
        self.assertEqual(self.shown(), ("/d/a.h5", 1))
        self.assertGreaterEqual(self.tab.series.findData("/d/c.h5"), 0, "the new stack is listed")
        self.integrator.row("/d/a.h5")
        self.assertEqual(self.shown(), ("/d/a.h5", 2))

    def test_a_row_while_the_rows_are_on_their_way(self):
        self.integrator.held = []
        self.tab.series.activated.emit(self.tab.series.currentIndex())
        self.integrator.row("/d/a.h5")
        self.integrator.release()
        self.assertEqual(self.shown(), ("/d/a.h5", 2))

    def test_a_long_stack_name_does_not_widen_the_tab(self):
        self.tab.show()
        self.app.processEvents()
        width = self.tab.sizeHint().width()
        self.integrator.row("/data/261031_lamino_electrode/sample_lamino_electrode_operando_cycle3_q.h5")
        self.app.processEvents()
        self.assertEqual(self.tab.sizeHint().width(), width)


if __name__ == "__main__":
    unittest.main()
