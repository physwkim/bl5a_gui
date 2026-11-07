"""The right column: the qs queue, the running alignment's stages and the
alignment history."""

import time

from silx.gui import qt

from . import docs, items
from .vendor.QueueView import QueueView


def _fmt(v, digits=5):
    if v is None:
        return "—"
    if isinstance(v, float):
        return "{:.{}g}".format(v, digits)
    return str(v)


class QueueTable(QueueView):
    """pal_tools' QueueView with this beamline's columns: the cells come
    from the item's kwargs, since bl5a-qs items carry their parameters
    there and not in md."""

    HEADERS = ["#", "Plan", "Name", "Parameters", "Folder", "User"]

    def __init__(self, parent=None):
        super().__init__(parent)
        self.table.setHorizontalHeaderLabels(self.HEADERS)

    @classmethod
    def _describe(cls, item):
        return items.describe(item)


class _Table(qt.QTableWidget):
    HEADERS = ()

    def __init__(self, parent=None):
        super().__init__(0, len(self.HEADERS), parent)
        self.setHorizontalHeaderLabels(self.HEADERS)
        self.setEditTriggers(qt.QAbstractItemView.NoEditTriggers)
        self.setSelectionBehavior(qt.QAbstractItemView.SelectRows)
        self.verticalHeader().setVisible(False)
        self.verticalHeader().setDefaultSectionSize(20)
        self.horizontalHeader().setStretchLastSection(True)

    def appendRow(self, cells, tips=None):
        row = self.rowCount()
        self.insertRow(row)
        for col, text in enumerate(cells):
            cell = qt.QTableWidgetItem(text)
            if tips and tips[col]:
                cell.setToolTip(tips[col])
            self.setItem(row, col, cell)
        self.resizeColumnsToContents()
        self.scrollToBottom()


class StagesTable(_Table):
    """Each stage of the alignment running now: the scan, which statistic
    it took, where the motor was and where the stage left it."""

    HEADERS = ("Stage", "Pick", "Before → after", "CEN", "FWHM", "Set")

    def __init__(self, parent=None):
        super().__init__(parent)
        self.caption = "Stages"

    def start(self, run):
        self.setRowCount(0)
        self.caption = "Stages — {} ({})".format(docs.what(run.plan, run.start.get("sequence")), run.series)

    def add(self, stage):
        moved = "{} → {}".format(_fmt(stage.get("before")), _fmt(stage.get("after")))
        self.appendRow(
            (str(stage.get("stage") or ""), str(stage.get("pick") or ""), moved, _fmt(stage.get("cen")),
             _fmt(stage.get("fwhm"), 3), "" if stage.get("set") is None else _fmt(stage.get("set"))),
            (None, "target {}".format(_fmt(stage.get("target"))), None,
             "COM {}, MAX at {}".format(_fmt(stage.get("com")), _fmt(stage.get("xmax"))), None,
             "set {} {} at {}: the user offset moved by {}".format(
                 stage.get("motor"), _fmt(stage.get("set")), _fmt(stage.get("target")),
                 _fmt(stage["target"] - stage["set"])) if stage.get("set") is not None else None))


class HistoryTable(_Table):
    """Where each finished alignment left the sample, per series. Δth0 is
    that run's correction to th's zero, not th0 itself: SPEC's dial is not
    published."""

    HEADERS = ("Time", "Series", "Plan", "T", "sy", "shold", "Δth0", "Exit")

    def add(self, row):
        when = time.strftime("%m-%d %H:%M", time.localtime(row["time"]))
        self.appendRow((when, row["series"], docs.what(row["plan"], row["sequence"]), _fmt(row["T"]),
                        _fmt(row["sy"]), _fmt(row["shold"]), _fmt(row["dth0"], 4), row["exit"]),
                       (None,) * 7 + (row["reason"] or None,))
