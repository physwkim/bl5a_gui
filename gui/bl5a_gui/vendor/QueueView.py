__author__ = "Sang-Woo Kim, Pohang Accelerator Laboratory"
__contact__ = "physwkim@postech.ac.kr"
__license__ = "MIT"

__doc__ = """
    The daemon's queue, as a list the operator can edit.

    Run starts a scan at once and does not touch the queue, so this is
    not a picture of what the control panel is about to do -- it is what
    the daemon is holding: what "Add to queue" put there, in the order it
    will run, plus anything queued from a console.

    Every edit here is a queue operation and none of them is a scan: the
    buttons emit the intent and the main window makes the call, so a
    refusal from the daemon is reported where every other daemon error is.
    Nothing is written from the table either -- it renders what setQueue()
    is handed.

    The running item is shown because a list that hid it would disagree
    with the daemon.  It arrives as it was queued, so its row reads like
    any other, but it has left the queue and no queue edit reaches it --
    it is drawn with no uid, which is what keeps it out of the selection.
    Abort is what ends a running scan.
"""

import os

from silx.gui import qt

COL_POS, COL_PLAN, COL_MODE, COL_FILE, COL_E0, COL_POINTS = range(6)
COLUMNS = ['#', 'Plan', 'Mode', 'File', 'E0 [eV]', 'Points']

_RUNNING_BG = qt.QColor(180, 210, 110)
_RUNNING_MARK = '▶'

_MIN_COLUMN_WIDTH = 50
_POS_COLUMN_WIDTH = 34


class QueueView(qt.QWidget):
    """
    The uid signals carry item_uids and not row numbers: the queue can
    have changed between the poll that drew the table and the click, and
    an item_uid still names the same item when it has.
    """

    sigStartRequested = qt.Signal()
    sigStopRequested = qt.Signal()
    sigEnvOpenRequested = qt.Signal()
    sigEnvCloseRequested = qt.Signal()
    sigRemoveRequested = qt.Signal(object)
    sigMoveRequested = qt.Signal(object, int)
    sigClearRequested = qt.Signal()
    sigAutostartRequested = qt.Signal(bool)

    def __init__(self, parent=None):
        super(QueueView, self).__init__(parent)

        # What the last setQueue() rendered, so an unchanged queue does
        # not rebuild the table under the operator's selection.
        self._signature = None

        self.caption = qt.QLabel("Queue", self)
        self.caption.setStyleSheet("font-weight: bold;")

        self.startBtn = self._button("Start", "Run the queue from its first item.")
        self.stopBtn = self._button(
            "Stop", "Stop the queue after the running scan.\n"
                    "The running scan is not interrupted; Abort does that.")
        self.upBtn = self._button("Up", "Move the selected item one place earlier.")
        self.downBtn = self._button("Down", "Move the selected item one place later.")
        self.removeBtn = self._button(
            "Remove", "Take the selected items out of the queue.\n"
                      "The running scan cannot be removed; Abort ends that one.")
        self.clearBtn = self._button("Clear", "Empty the queue. The running scan keeps going.")

        self.envLabel = qt.QLabel(self)
        self.openBtn = self._button(
            "Open", "Open the daemon's RunEngine environment: the worker\n"
                    "process the plans run in. Nothing runs without it.")
        self.closeBtn = self._button(
            "Close", "Close the RunEngine environment. The queue keeps its\n"
                     "items and nothing runs until it is open again.\n"
                     "Not while a scan is running; Abort ends that one.")

        self.autostartBox = qt.QCheckBox("Autostart", self)
        self.autostartBox.setEnabled(False)
        self.autostartBox.setToolTip(
            "Run what is added to the queue without a Start.\n"
            "The daemon disarms this on every queue exit that is not a\n"
            "normal drain -- a failed or aborted scan, a Stop -- and this\n"
            "is where it goes back on.")

        self.startBtn.clicked.connect(self.sigStartRequested)
        self.stopBtn.clicked.connect(self.sigStopRequested)
        self.upBtn.clicked.connect(lambda: self._requestMove(-1))
        self.downBtn.clicked.connect(lambda: self._requestMove(1))
        self.removeBtn.clicked.connect(self._requestRemove)
        self.clearBtn.clicked.connect(self.sigClearRequested)
        self.openBtn.clicked.connect(self.sigEnvOpenRequested)
        self.closeBtn.clicked.connect(self.sigEnvCloseRequested)
        # `clicked` and not `toggled`: the box is redrawn from the status
        # poll twice a second, and only an operator's click may become a
        # call -- a signal the redraw also emits would arm the queue back
        # the moment the daemon disarmed it.
        self.autostartBox.clicked.connect(self.sigAutostartRequested)

        head = qt.QWidget(self)
        headLayout = qt.QHBoxLayout(head)
        headLayout.setContentsMargins(0, 0, 0, 0)
        headLayout.addWidget(self.caption)
        headLayout.addStretch()
        for button in (self.startBtn, self.stopBtn, self.upBtn,
                       self.downBtn, self.removeBtn, self.clearBtn):
            headLayout.addWidget(button)

        self.table = qt.QTableWidget(self)
        self.table.setColumnCount(len(COLUMNS))
        self.table.setHorizontalHeaderLabels(COLUMNS)
        self.table.setSelectionBehavior(qt.QAbstractItemView.SelectRows)
        self.table.setEditTriggers(qt.QAbstractItemView.NoEditTriggers)
        self.table.setAlternatingRowColors(True)
        self.table.verticalHeader().setVisible(False)
        self.table.horizontalHeader().setSectionResizeMode(qt.QHeaderView.Interactive)
        # Off: the spare width goes to File, not to Points -- the
        # numbers are short and fixed, file names are neither.
        self.table.horizontalHeader().setStretchLastSection(False)
        self.table.itemSelectionChanged.connect(self._selectionChanged)

        # Delete on the list does what Remove does, which is what a list
        # of things you can take out of is expected to do.
        self.removeShortcut = qt.QShortcut(qt.QKeySequence.Delete, self.table)
        self.removeShortcut.setContext(qt.Qt.WidgetShortcut)
        self.removeShortcut.activated.connect(self._requestRemove)

        # The environment and the arming are the daemon's, not the list's,
        # so they get their own row rather than three more controls among
        # the queue's.
        foot = qt.QWidget(self)
        footLayout = qt.QHBoxLayout(foot)
        footLayout.setContentsMargins(0, 0, 0, 0)
        footLayout.addWidget(self.envLabel)
        footLayout.addSpacing(12)
        footLayout.addWidget(self.autostartBox)
        footLayout.addStretch()
        footLayout.addWidget(self.openBtn)
        footLayout.addWidget(self.closeBtn)

        layout = qt.QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(3)
        layout.addWidget(head)
        layout.addWidget(self.table)
        layout.addWidget(foot)

        self.setQueue({}, [])
        self.setEnvironment(None)
        self.setAutostart(None)

    def _button(self, text, tooltip):
        button = qt.QPushButton(text, self)
        button.setFixedHeight(30)
        button.setToolTip(tooltip)
        button.setEnabled(False)
        return button

    # ------------------------------------------------------------------
    # Rendering
    # ------------------------------------------------------------------

    @staticmethod
    def _md(item):
        """The metadata the item was queued with; the scan's own keys live
        there and nowhere else in the item."""
        kwargs = item.get('kwargs') or {}
        md = kwargs.get('md') if isinstance(kwargs, dict) else None
        return md if isinstance(md, dict) else {}

    @classmethod
    def _describe(cls, item):
        """(plan, mode, file, E0, points) as the five cells read them.

        Read out of the metadata and not computed from the arguments: a
        cell is blank when the item did not declare the value, which is
        what an item queued from a console looks like.  The file is the
        name part of `write_path`, what the control panel's filename box
        held when the scan was queued."""
        md = cls._md(item)
        mode = md.get('scan_mode') or ''
        path = md.get('write_path') or ''
        E0 = md.get('E0')
        points = md.get('scan_points')
        return (str(item.get('name') or ''),
                str(mode),
                os.path.basename(str(path)),
                '' if E0 is None else '{:.1f}'.format(float(E0)),
                '' if points is None else str(points))

    def setQueue(self, running, items):
        """
        Render the running scan and the queue behind it.

        Called from the status poll, so it rebuilds only when something
        changed -- a rebuild drops the selection, and a selection dropped
        twice a second cannot be clicked in.
        """
        running = running or {}
        items = items or []
        rows = []
        if running:
            rows.append((None, self._describe(running)))
        for item in items:
            rows.append((item.get('item_uid'), self._describe(item)))

        signature = [(uid, cells) for uid, cells in rows]
        if signature == self._signature:
            return

        selected = self.selectedUids()
        self._signature = signature

        self.table.setRowCount(len(rows))
        for row, (uid, cells) in enumerate(rows):
            position = _RUNNING_MARK if uid is None else str(row if running else row + 1)
            for col, text in enumerate((position,) + cells):
                cell = qt.QTableWidgetItem(text)
                cell.setTextAlignment(qt.Qt.AlignCenter | qt.Qt.AlignVCenter)
                if col == COL_POS:
                    # The uid rides on the row's first cell, so the click
                    # that selects a row is what names the item to edit.
                    cell.setData(qt.Qt.UserRole, uid)
                if uid is None:
                    cell.setBackground(_RUNNING_BG)
                self.table.setItem(row, col, cell)

        self.caption.setText("Queue" if not rows
                             else "Queue — {} item{}".format(
                                 len(rows), '' if len(rows) == 1 else 's'))

        self._restoreSelection(selected)
        self._fitColumns()
        # Start only with something to start and nothing running, Stop
        # only with something to stop: a stop request the daemon keeps
        # pending would otherwise halt the next queue after one item.
        self.startBtn.setEnabled(bool(items) and not running)
        self.stopBtn.setEnabled(bool(running))
        self.clearBtn.setEnabled(bool(items))
        self._selectionChanged()

    def setEnvironment(self, exists, idle=True):
        """Show whether the daemon's RunEngine environment is open, and
        offer the transition it is in a state for.

        `exists` None is what a poll that could not reach the daemon
        knows: neither button is offered then, because neither call has
        anywhere to go.  Close is held back while the worker is busy --
        the daemon refuses it, and a button that is always refused is
        worse than one that waits."""
        self.envLabel.setText("Environment: {}".format(
            "not known" if exists is None else "open" if exists else "closed"))
        self.openBtn.setEnabled(exists is False)
        self.closeBtn.setEnabled(exists is True and idle)

    def setAutostart(self, armed):
        """Show whether the daemon runs what is added to the queue.

        The box carries no intent of its own: every draw is the daemon's
        `queue_autostart_enabled`, so an arming the GUI's own submission
        did reads here as itself, and a disarming the daemon did after a
        failed scan is visible instead of being guessed from a queue that
        sits still.  `armed` None is a poll that could not reach the
        daemon, drawn the way the environment's unknown state is."""
        self.autostartBox.setChecked(bool(armed))
        self.autostartBox.setEnabled(armed is not None)

    def _fitColumns(self):
        """Fit to contents, then hand what is left over to File."""
        table = self.table
        table.resizeColumnsToContents()

        widths = [max(table.columnWidth(col), _MIN_COLUMN_WIDTH)
                  for col in range(table.columnCount())]
        widths[COL_POS] = _POS_COLUMN_WIDTH

        spare = table.viewport().width() - sum(widths)
        if spare > 0:
            widths[COL_FILE] += spare

        for col, width in enumerate(widths):
            table.setColumnWidth(col, width)

    # ------------------------------------------------------------------
    # Selection
    # ------------------------------------------------------------------

    def _uidOfRow(self, row):
        cell = self.table.item(row, COL_POS)
        return None if cell is None else cell.data(qt.Qt.UserRole)

    def selectedUids(self):
        """item_uids of the selected rows; the running scan has none."""
        rows = sorted({idx.row() for idx in self.table.selectedIndexes()})
        return [uid for uid in (self._uidOfRow(row) for row in rows)
                if uid is not None]

    def _restoreSelection(self, uids):
        """
        Re-select the same items after a rebuild.

        One selection command for every row rather than a selectRow() per
        row: in extended-selection mode each of those clears what the
        previous one selected, so a multiple selection would come back as
        its last row alone.
        """
        if not uids:
            return

        wanted = set(uids)
        model = self.table.model()
        last = self.table.columnCount() - 1
        selection = qt.QItemSelection()
        for row in range(self.table.rowCount()):
            if self._uidOfRow(row) in wanted:
                selection.select(model.index(row, 0), model.index(row, last))

        self.table.selectionModel().select(
            selection, qt.QItemSelectionModel.ClearAndSelect)

    def _selectionChanged(self):
        selected = self.selectedUids()
        self.removeBtn.setEnabled(bool(selected))
        # One item at a time: "one place earlier" is a place a block of
        # items does not have.
        self.upBtn.setEnabled(len(selected) == 1)
        self.downBtn.setEnabled(len(selected) == 1)

    def _requestRemove(self):
        uids = self.selectedUids()
        if uids:
            self.sigRemoveRequested.emit(uids)

    def _requestMove(self, step):
        uids = self.selectedUids()
        if len(uids) == 1:
            self.sigMoveRequested.emit(uids[0], step)
