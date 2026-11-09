"""The battery, in-situ and scan forms. Each builds its items with `items`
and emits them; nothing reaches the queue that `items` refused."""

from silx.gui import qt

from . import config, items
from .editor import SequenceCombo, resolve
from .items import Invalid


def _line(text="", tip="", width=None):
    w = qt.QLineEdit(text)
    if tip:
        w.setToolTip(tip)
    if width:
        w.setFixedWidth(width)
    return w


class BatteryForm(qt.QWidget):
    """mymar1 (one frame) or mymar (N frames, one every Ti minutes) as a
    mar_series item."""

    queueRequested = qt.Signal(object)
    stopAfterFrameRequested = qt.Signal()

    def __init__(self, parent=None):
        super().__init__(parent)
        form = qt.QFormLayout(self)
        self.single = qt.QRadioButton("Single shot (mymar1)")
        self.repeat = qt.QRadioButton("Repeat (mymar)")
        self.single.setChecked(True)
        modes = qt.QHBoxLayout()
        modes.addWidget(self.single)
        modes.addWidget(self.repeat)
        form.addRow(modes)
        self.name = _line(tip="mar345 file name: fewer than 28 characters, no spaces or quotes")
        self.name.setMaxLength(27)
        self.texpo = _line("2", "Seconds 1 to 20, or negative: monitor counts")
        self.n = qt.QSpinBox()
        self.n.setRange(1, 100000)
        self.n.setValue(1)
        self.ti = _line("3", "Minutes from one frame's start to the next, more than 2.5")
        self.geometry = qt.QComboBox()
        self.geometry.addItems(items.GEOMETRIES)
        self.geometry.setToolTip("mymar's mar345 positions: martran, marsdd, scan size")
        self.series = _line(tip="The data folder and stack the frames go to (no spaces); empty: the file name")
        form.addRow("File name", self.name)
        form.addRow("Texpo", self.texpo)
        form.addRow("N", self.n)
        form.addRow("Ti [min]", self.ti)
        form.addRow("Geometry", self.geometry)
        form.addRow("Series", self.series)
        self.add = qt.QPushButton("Add to queue")
        self.stopAfterFrame = qt.QPushButton("Stop after frame")
        self.stopAfterFrame.setToolTip("Let the frame being taken finish, then stop the series:\n"
                                       "a deferred pause, and a stop once paused.")
        self.stopAfterFrame.setEnabled(False)
        buttons = qt.QHBoxLayout()
        buttons.addWidget(self.add)
        buttons.addWidget(self.stopAfterFrame)
        form.addRow(buttons)
        self.single.toggled.connect(self._mode)
        self.add.clicked.connect(self._add)
        self.stopAfterFrame.clicked.connect(self.stopAfterFrameRequested)
        self._mode()

    def _mode(self):
        repeat = self.repeat.isChecked()
        self.n.setEnabled(repeat)
        self.ti.setEnabled(repeat)

    def item(self):
        repeat = self.repeat.isChecked()
        return items.mar_series(self.name.text().strip(), self.texpo.text().strip(), self.geometry.currentText(),
                                n=self.n.value() if repeat else 1,
                                ti=self.ti.text().strip() if repeat else None,
                                series=self.series.text().strip() or None)

    def _add(self):
        try:
            item = self.item()
        except Invalid as e:
            qt.QMessageBox.warning(self, "Battery", str(e))
            return
        answer = qt.QMessageBox.question(
            self, "Beamstop", "Is the beamstop in place?\n\nThe frames are taken with the shutter open.",
            qt.QMessageBox.Yes | qt.QMessageBox.No, qt.QMessageBox.No)
        if answer == qt.QMessageBox.Yes:
            self.queueRequested.emit(item)


class ScanForm(qt.QWidget):
    """One ascan or lup of any motor as a step_scan item: drawn in the Scan
    tab and written to the series' SPEC file, as an alignment's scans."""

    queueRequested = qt.Signal(object)

    def __init__(self, parent=None):
        super().__init__(parent)
        form = qt.QFormLayout(self)
        self.lup = qt.QRadioButton("lup (relative)")
        self.ascan = qt.QRadioButton("ascan (absolute)")
        self.lup.setChecked(True)
        self.lup.setToolTip("start and finish are offsets from where the motor is; it goes back there after")
        self.ascan.setToolTip("start and finish are positions; the motor stays at finish")
        modes = qt.QHBoxLayout()
        modes.addWidget(self.lup)
        modes.addWidget(self.ascan)
        form.addRow(modes)
        self.motor = qt.QComboBox()
        self.motor.addItems(config.MOTORS)
        self.start = _line("-0.1")
        self.finish = _line("0.1")
        self.intervals = qt.QSpinBox()
        self.intervals.setRange(1, 10000)
        self.intervals.setValue(10)
        self.intervals.setToolTip("Steps; the scan takes intervals + 1 points")
        self.time = _line("1", "Seconds counted at each point")
        self.counter = qt.QComboBox()
        self.counter.addItems(items.COUNTERS)
        self.counter.setCurrentText("det")
        self.counter.setToolTip("The counter the scan's CEN, COM and peak are of")
        self.series = _line(tip="The data folder whose SPEC file gets the scan (no spaces); empty: step_scan")
        form.addRow("Motor", self.motor)
        form.addRow("Start", self.start)
        form.addRow("Finish", self.finish)
        form.addRow("Intervals", self.intervals)
        form.addRow("Time [s]", self.time)
        form.addRow("Counter", self.counter)
        form.addRow("Series", self.series)
        self.add = qt.QPushButton("Add to queue")
        form.addRow(self.add)
        self.add.clicked.connect(self._add)

    def item(self):
        return items.step_scan(self.motor.currentText(), self.start.text().strip(), self.finish.text().strip(),
                               self.intervals.value(), self.time.text().strip(), relative=self.lup.isChecked(),
                               counter=self.counter.currentText(), series=self.series.text().strip() or None)

    def _add(self):
        try:
            item = self.item()
        except Invalid as e:
            qt.QMessageBox.warning(self, "Scan", str(e))
            return
        self.queueRequested.emit(item)


COOLING_TIP = "myinsitu's last step: ramp 25 °C/min to 25 °C, soak 3600 points,\nno alignment or frames"

# The in-situ table's columns: (key, header, tooltip).
COLUMNS = (
    ("T", "T [°C]", "settemp's set point"),
    ("ramp", "ramp", "set_ramp, °C/min"),
    ("wait", "wait", "wait until T is reached"),
    ("soak_n", "soak n", "loopscan points while soaking"),
    ("soak_t", "soak t", "seconds per soak point"),
    ("tolerance", "tol", "with wait: how close to T counts as reached, °C (changec's DELTA_K)"),
    ("align", "align", "run the Alignment sequence before the frames"),
    ("th_list", "th list", "mymar_in's th values, comma separated"),
    ("texpo", "texpo", "seconds (> 0) or monitor counts (< 0)"),
    ("name", "name", "mar345 file name"),
)
KEYS = [c[0] for c in COLUMNS]
CHECKS = ("wait", "align")


def _cell_text(v):
    """An items.NEW_ROW value as its cell's text."""
    if v is None:
        return ""
    if isinstance(v, (list, tuple)):
        return ", ".join(_cell_text(x) for x in v)
    return "{:g}".format(v) if isinstance(v, float) else str(v)


class InsituForm(qt.QWidget):
    """myinsitu as a table: each row is one insitu_step (settemp →
    the alignment sequence → mymar_in), all in one series."""

    queueRequested = qt.Signal(object)

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = qt.QVBoxLayout(self)
        top = qt.QFormLayout()
        self.series = _line(tip="The data folder and stack every row's frames go to (no spaces);"
                                " empty: the first row's name")
        top.addRow("Series", self.series)
        self.sequence = SequenceCombo()
        self.sequence.setToolTip("The sequence each row with align runs: Thin film is align_in.mac's;\n"
                                 "the user's own are the Alignment panel's user's")
        top.addRow("Alignment", self.sequence)
        layout.addLayout(top)
        self.table = qt.QTableWidget(0, len(COLUMNS))
        self.table.setHorizontalHeaderLabels([c[1] for c in COLUMNS])
        for k, (_, head, tip) in enumerate(COLUMNS):
            self.table.horizontalHeaderItem(k).setToolTip(tip)
        self.table.verticalHeader().setDefaultSectionSize(22)
        layout.addWidget(self.table)
        buttons = qt.QHBoxLayout()
        self.addRow = qt.QPushButton("Add row")
        self.coolingRow = qt.QPushButton("Cooling row")
        self.coolingRow.setToolTip(COOLING_TIP)
        self.removeRow = qt.QPushButton("Remove row")
        self.queueRows = qt.QPushButton("Queue rows")
        for b in (self.addRow, self.coolingRow, self.removeRow):
            buttons.addWidget(b)
        buttons.addStretch()
        buttons.addWidget(self.queueRows)
        layout.addLayout(buttons)
        self.addRow.clicked.connect(lambda: self.append({}))
        self.coolingRow.clicked.connect(lambda: self.append(items.cooling_row()))
        self.removeRow.clicked.connect(self._remove)
        self.queueRows.clicked.connect(self._queue)
        self.append({})

    def append(self, values):
        """A row showing `values`, an items.NEW_ROW whose left-out keys
        keep NEW_ROW's values."""
        values = dict(items.NEW_ROW, **values)
        cells = {key: _cell_text(values[key]) for key in KEYS if key not in CHECKS}
        row = self.table.rowCount()
        self.table.insertRow(row)
        for k, key in enumerate(KEYS):
            cell = qt.QTableWidgetItem()
            if key in CHECKS:
                cell.setFlags(qt.Qt.ItemIsEnabled | qt.Qt.ItemIsSelectable | qt.Qt.ItemIsUserCheckable)
                cell.setCheckState(qt.Qt.Checked if values[key] else qt.Qt.Unchecked)
            else:
                cell.setText(cells[key])
            self.table.setItem(row, k, cell)
        self.table.resizeColumnsToContents()

    def _remove(self):
        for row in sorted({i.row() for i in self.table.selectedIndexes()}, reverse=True):
            self.table.removeRow(row)

    def _cell(self, row, key):
        cell = self.table.item(row, KEYS.index(key))
        if key in CHECKS:
            return cell is not None and cell.checkState() == qt.Qt.Checked
        return "" if cell is None else cell.text().strip()

    def rows(self):
        """The table as items.insitu_items reads it."""
        return [{key: self._cell(r, key) for key in KEYS} for r in range(self.table.rowCount())]

    def queue_items(self):
        """Every row's item, or Invalid naming the first bad row."""
        rows = self.rows()
        sequence = resolve(self.sequence.currentText()) if any(r["align"] for r in rows) else None
        return items.insitu_items(rows, self.series.text(), sequence)

    def _queue(self):
        try:
            rows = self.queue_items()
        except Invalid as e:
            qt.QMessageBox.warning(self, "In-situ", str(e))
            return
        self.queueRequested.emit(rows)
