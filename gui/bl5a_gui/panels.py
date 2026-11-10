"""The status line and the left column's motor and alignment panels."""

import html

from silx.gui import qt

from . import config, gate, items, sequences
from .editor import SequenceEditor, UserCombo, dialog_dir, resolve, source
from .items import Invalid

DASH = "—"


def _dot(color):
    return '<span style="color:{}">●</span>'.format(color)


class StatusBar(qt.QWidget):
    """Ring, shutter, SPEC, qs, temperature, mar345 and its last file, one
    label each, on two rows: one row is wider than the main window. A PV
    that is not configured or not connected reads "—"."""

    def __init__(self, parent=None):
        super().__init__(parent)
        layout = qt.QVBoxLayout(self)
        layout.setContentsMargins(4, 2, 4, 2)
        layout.setSpacing(2)
        self.labels = {}
        for fields in ((("ring", "Ring"), ("shutter", "Shutter"), ("spec", "SPEC"), ("qs", "QS")),
                       (("temp", "T"), ("mar", "MAR"), ("file", "File"))):
            row = qt.QHBoxLayout()
            layout.addLayout(row)
            for key, caption in fields:
                if row.count():
                    row.addSpacing(14)
                label = qt.QLabel(self)
                label.setTextFormat(qt.Qt.RichText)
                self.labels[key] = (caption, label)
                row.addWidget(label)
                self.set(key, DASH)
            row.addStretch(1)
        self.labels["file"][1].setTextInteractionFlags(qt.Qt.TextSelectableByMouse)

    def set(self, key, rich):
        caption, label = self.labels[key]
        label.setText("<b>{}</b> {}".format(caption, rich))

    def setSpec(self, connected, busy):
        if connected is None:
            self.set("spec", DASH)
        elif not connected:
            self.set("spec", _dot("#c33") + " disconnected")
        else:
            self.set("spec", _dot("#e90" if busy else "#3a3") + (" busy" if busy else " ready"))

    def setQs(self, status, stopping=False):
        if status is None:
            self.set("qs", _dot("#c33") + " not answering")
            return
        state = gate.re_state(status) or status.get("manager_state") or "?"
        color = {"idle": "#3a3", "paused": "#e90"}.get(state, "#36c")
        text = state
        if status.get("running_item_name"):
            text += " " + status["running_item_name"]
        text += ", {} queued".format(status.get("items_in_queue", 0))
        if not status.get("worker_environment_exists"):
            text += ", environment closed"
        if stopping:
            text += ", stopping after the frame"
        self.set("qs", _dot(color) + " " + html.escape(text))

    def setTemperature(self, sp, rbv, ramp):
        if sp is None and rbv is None:
            self.set("temp", DASH)
            return
        fmt = lambda v: DASH if v is None else "{:.2f}".format(v)  # noqa: E731
        self.set("temp", "{} → {} °C, ramp {} °C/min".format(fmt(rbv), fmt(sp), fmt(ramp)))


class MotorsPanel(qt.QGroupBox):
    """Each spec-ioc motor's readback and its tweak buttons. A tweak is
    the motor record's TWF/TWR by TWV, a moveTo its VAL, which spec-ioc
    turns into a SPEC move; the buttons are live only while nothing else
    drives SPEC."""

    moved = qt.Signal(str, object)  # a moveTo's motor; None, or why its put failed
    failed = qt.Signal(str)  # why a moveTo did not move

    def __init__(self, pvs, parent=None):
        super().__init__(parent)  # its pane is titled Motors
        self.pvs = pvs
        self.rows = {}
        grid = qt.QGridLayout(self)
        grid.setContentsMargins(6, 6, 6, 6)
        grid.setHorizontalSpacing(4)
        grid.setVerticalSpacing(2)
        for col, head in enumerate(("", "position", "", "step", "", "")):
            if head:
                grid.addWidget(qt.QLabel("<i>{}</i>".format(head)), 0, col)
        for row, m in enumerate(config.MOTORS, 1):
            base = config.SPEC + m
            name = qt.QLabel("<b>{}</b>".format(m))
            rbv = qt.QLabel(DASH)
            rbv.setAlignment(qt.Qt.AlignRight | qt.Qt.AlignVCenter)
            rbv.setMinimumWidth(rbv.fontMetrics().horizontalAdvance("-0000.0000 mm"))
            moving = qt.QLabel("")
            step = qt.QLineEdit()
            step.setFixedWidth(step.fontMetrics().horizontalAdvance("00.000000"))
            step.setToolTip("Tweak step ({}.TWV)".format(base))
            back = qt.QToolButton()
            back.setText("◀")
            fwd = qt.QToolButton()
            fwd.setText("▶")
            back.setToolTip("{} − step".format(m))
            fwd.setToolTip("{} + step".format(m))
            step.editingFinished.connect(lambda m=m: self._setStep(m))
            back.clicked.connect(lambda _=False, b=base: self.pvs.put(b + ".TWR", 1))
            fwd.clicked.connect(lambda _=False, b=base: self.pvs.put(b + ".TWF", 1))
            for col, w in enumerate((name, rbv, moving, step, back, fwd)):
                grid.addWidget(w, row, col)
            self.rows[m] = {"rbv": rbv, "moving": moving, "step": step, "buttons": (step, back, fwd),
                            "prec": 4, "egu": ""}
            for field in ("RBV", "DMOV", "TWV", "PREC", "EGU"):
                pvs.watch("{}.{}".format(base, field))
        last = len(config.MOTORS) + 1
        self.alignPos = qt.QPushButton("Align position")
        self.measurePos = qt.QPushButton("Measure position")
        for b in (self.alignPos, self.measurePos):
            b.setEnabled(False)
            b.setToolTip("Not defined yet: BL5A has not given these positions.")
        buttons = qt.QHBoxLayout()
        buttons.addWidget(self.alignPos)
        buttons.addWidget(self.measurePos)
        grid.addLayout(buttons, last, 0, 1, 6)
        grid.setRowStretch(last + 1, 1)  # a pane taller than the rows: the space goes below them
        grid.setColumnStretch(6, 1)  # and wider: right of them
        pvs.changed.connect(self._changed)
        pvs.connected.connect(self._connected)
        self.moved.connect(self._moved)
        self.setAllowed(False)

    def setAllowed(self, allowed):
        for row in self.rows.values():
            for w in row["buttons"]:
                w.setEnabled(allowed)

    def moveTo(self, motor, value):
        # The put completes when the move does, or at once when refused;
        # the signal brings its end to this thread.
        self.pvs.put(config.SPEC + motor + ".VAL", value, callback=lambda error: self.moved.emit(motor, error))

    def _moved(self, motor, error):
        msta = self.pvs.get_text(config.SPEC + motor + ".MSTA") if error is None else ""
        if msta and int(float(msta)) & config.MSTA_PROBLEM:
            error = "SPEC refused it; the SPEC pane says why"
        if error is not None:
            self.failed.emit("{} move: {}".format(motor, error))

    def _setStep(self, m):
        row = self.rows[m]
        try:
            v = float(row["step"].text())
        except ValueError:
            return
        if v > 0:
            self.pvs.put(config.SPEC + m + ".TWV", v)

    def _motor(self, name):
        if not name.startswith(config.SPEC) or "." not in name:
            return None, None
        m, field = name[len(config.SPEC):].split(".", 1)
        return (self.rows.get(m), field) if m in self.rows else (None, None)

    def _changed(self, name, value, text):
        row, field = self._motor(name)
        if row is None:
            return
        if field == "RBV":
            row["rbv"].setText("{:.{}f} {}".format(float(value), row["prec"], row["egu"]))
        elif field == "DMOV":
            row["moving"].setText("" if value else _dot("#e90"))
            row["moving"].setTextFormat(qt.Qt.RichText)
        elif field == "TWV" and not row["step"].hasFocus():
            row["step"].setText("{:g}".format(float(value)))
        elif field == "PREC":
            row["prec"] = max(0, min(int(value), 8))
        elif field == "EGU":
            row["egu"] = text

    def _connected(self, name, ok):
        row, field = self._motor(name)
        if row is not None and field == "RBV" and not ok:
            row["rbv"].setText(DASH)


class AlignBox(qt.QGroupBox):
    """The alignment sequence being used: load a template or one the user
    saved, edit its blocks in their own window, save it as the user's,
    queue it in the series."""

    queueRequested = qt.Signal(object)

    def __init__(self, parent=None):
        super().__init__("Alignment", parent)
        layout = qt.QGridLayout(self)
        self.coin = qt.QPushButton("Coin cell")
        self.coin.setToolTip("Load coin_align's sequence")
        self.film = qt.QPushButton("Thin film")
        self.film.setToolTip("Load align_in.mac's sequence")
        self.load = qt.QPushButton("Load…")
        self.load.setToolTip("Load one of the user's saved sequences")
        self.load.setMenu(qt.QMenu(self.load))
        self.load.menu().aboutToShow.connect(self._savedMenu)
        loads = qt.QHBoxLayout()
        for b in (self.coin, self.film, self.load):
            loads.addWidget(b)
        self.user = UserCombo()
        self.user.setToolTip("Whose sequences: kept in <data folder>/alignments/<user>")
        self.editor = SequenceEditor(self)
        self.shown = qt.QLabel()
        self.edit = qt.QPushButton("Blocks…")
        self.edit.setToolTip("The loaded sequence's blocks, in a window of their own")
        self.series = qt.QLineEdit()
        self.series.setToolTip("The history groups alignments by this, and it names the data folder (no spaces);\n"
                               "empty: each run on its own, in a folder named after the plan")
        self.queue = qt.QPushButton("Queue")
        self.queue.setToolTip("Queue the blocks shown, as they are")
        layout.addLayout(loads, 0, 0, 1, 3)
        layout.addWidget(qt.QLabel("User"), 1, 0)
        layout.addWidget(self.user, 1, 1, 1, 2)
        layout.addWidget(self.shown, 2, 0, 1, 2)
        layout.addWidget(self.edit, 2, 2)
        layout.addWidget(qt.QLabel("Series"), 3, 0)
        layout.addWidget(self.series, 3, 1)
        layout.addWidget(self.queue, 3, 2)
        layout.setColumnStretch(1, 1)
        self.user.editTextChanged.connect(self._user)
        self.coin.clicked.connect(lambda: self._load("Coin cell"))
        self.film.clicked.connect(lambda: self._load("Thin film"))
        self.edit.clicked.connect(self.editor.open)
        self.editor.changed.connect(self._shown)
        self.queue.clicked.connect(self._queue)
        self.editor.load(*resolve(sequences.DEFAULT))

    def _user(self, text):
        _settings().setValue("align_user", text.strip())

    def _savedMenu(self):
        menu = self.load.menu()
        menu.clear()
        root, user = source()
        names = sequences.saved(root, user)
        for name in names:
            # "&&": a lone "&" in an action's text marks a shortcut key.
            menu.addAction(name.replace("&", "&&"), lambda name=name: self._load(name))
        if not names:
            menu.addAction("No sequences saved by {!r}".format(sequences.owner(user)) if root
                           else "Set the Data folder first").setEnabled(False)

    def _load(self, name):
        try:
            _, blocks = resolve(name)
        except Invalid as e:
            qt.QMessageBox.warning(self, "Alignment", str(e))
            return
        if self.editor.load(name, blocks):
            self.editor.open()

    def _shown(self):
        e = self.editor
        self.shown.setText("<b>{}</b>{}, {} blocks".format(html.escape(e.name), " *" if e.dirty else "",
                                                          len(e.blocks)))

    def _queue(self):
        try:
            item = items.align(self.editor.blocks, self.editor.label(), self.series.text().strip() or None)
        except Invalid as e:
            qt.QMessageBox.warning(self, "Alignment", str(e))
            return
        self.queueRequested.emit(item)


def _settings():
    return qt.QSettings(*config.SETTINGS)


class DataBox(qt.QGroupBox):
    """The data folder each queued item carries (md.data_root), and the
    folder mar345-ioc writes to now, which bl5a-qs sets per item."""

    def __init__(self, pvs, parent=None):
        super().__init__("Data", parent)
        self.root = qt.QLineEdit(str(_settings().value("data_root", "") or ""))
        self.root.setPlaceholderText("bl5a-qs's --data-root")
        self.root.setToolTip(
            "Each item queued from now on puts its frames, SPEC file and stacks in\n"
            "<this folder>/<series>, made when the item starts; empty: bl5a-qs's --data-root.\n"
            "The path must be the same on the bl5a-qs, mar345-ioc and integrator hosts,\n"
            "without spaces: mar345dtb takes a frame's path in one word.")
        browse = qt.QPushButton("Browse…")
        self.mar = qt.QLabel(DASH)
        self.mar.setTextFormat(qt.Qt.RichText)
        self.mar.setWordWrap(True)
        self.mar.setTextInteractionFlags(qt.Qt.TextSelectableByMouse)
        self.mar.setToolTip("mar345-ioc's FilePath_RBV and FilePathExists_RBV")
        layout = qt.QGridLayout(self)
        layout.addWidget(qt.QLabel("Folder"), 0, 0)
        layout.addWidget(self.root, 0, 1)
        layout.addWidget(browse, 0, 2)
        layout.addWidget(qt.QLabel("mar345"), 1, 0)
        layout.addWidget(self.mar, 1, 1, 1, 2)
        self.path = {"FilePath_RBV": None, "FilePathExists_RBV": None}
        for suffix in self.path:
            pvs.watch(config.MAR + suffix)
        pvs.changed.connect(self._pv)
        pvs.connected.connect(self._connected)
        browse.clicked.connect(self._browse)
        self.root.editingFinished.connect(self._save)

    def _browse(self):
        path = qt.QFileDialog.getExistingDirectory(self, "Data folder", self.root.text())
        if path:
            self.root.setText(path)
            self._save()

    def _save(self):
        _settings().setValue("data_root", self.root.text().strip())

    def _pv(self, name, value, text):
        suffix = name[len(config.MAR):] if name.startswith(config.MAR) else None
        if suffix in self.path:
            self.path[suffix] = text
            self._show()

    def _connected(self, name, ok):
        suffix = name[len(config.MAR):] if name.startswith(config.MAR) else None
        if suffix in self.path and not ok:
            self.path[suffix] = None
            self._show()

    def _show(self):
        path, exists = self.path["FilePath_RBV"], self.path["FilePathExists_RBV"]
        if path is None:
            self.mar.setText(DASH)
            return
        mark = {"Yes": "", "No": " " + _dot("#c33") + " missing"}.get(exists, "")
        self.mar.setText(html.escape(path) + mark)


def _poni_lines(text):
    return "; ".join(line for line in str(text or "").splitlines() if line and not line.startswith("#"))


class IntegrationBox(qt.QGroupBox):
    """What the integrator integrates with now, and a PONI and npt to send
    it. A change applies to the frames after those already queued; a new
    q axis starts a new stack, and existing stacks keep theirs."""

    def __init__(self, integrator, parent=None):
        super().__init__("Integration", parent)
        self.integrator = integrator
        self.poni = qt.QLineEdit(str(_settings().value("poni", "") or ""))
        self.poni.setPlaceholderText("PONI file")
        self.poni.setToolTip("pyFAI PONI of the full 345 mm plate; its text is sent")
        browse = qt.QPushButton("Browse…")
        self.npt = qt.QSpinBox()
        self.npt.setRange(1, 100000)
        self.npt.setValue(2000)
        self.npt.setToolTip("Bins from q = 0 to the plate's far rim")
        self.apply = qt.QPushButton("Apply")
        self.apply.setToolTip("Send the PONI and npt; frames already queued keep the current ones")
        self.state = qt.QLabel("integrator: not answering")
        self.state.setTextFormat(qt.Qt.PlainText)
        self.state.setWordWrap(True)
        self.state.setTextInteractionFlags(qt.Qt.TextSelectableByMouse)
        layout = qt.QGridLayout(self)
        layout.addWidget(qt.QLabel("PONI"), 0, 0)
        layout.addWidget(self.poni, 0, 1, 1, 2)
        layout.addWidget(browse, 0, 3)
        layout.addWidget(qt.QLabel("npt"), 1, 0)
        layout.addWidget(self.npt, 1, 1)
        layout.addWidget(self.apply, 1, 3)
        layout.addWidget(self.state, 2, 0, 1, 4)
        self.active = None  # the config the integrator reports
        self.pending = None  # a set it accepted that has not taken effect
        self.refused = None
        browse.clicked.connect(self._browse)
        self.apply.clicked.connect(lambda: self.applyFile(self.poni.text()))
        integrator.reply.connect(self._reply)
        integrator.published.connect(self._published)
        integrator.failed.connect(self._failed)

    def _browse(self):
        path, _ = qt.QFileDialog.getOpenFileName(self, "PONI", dialog_dir(self.poni.text()),
                                                 "PONI (*.poni);;All files (*)")
        if path:
            self.poni.setText(path)

    def applyFile(self, path):
        """Send the PONI file at `path` with the box's npt: the GUI's one
        way to a `set`, Apply's and the Calibration tab's."""
        path = path.strip()
        self.poni.setText(path)
        try:
            with open(items.required(path, "PONI", "choose a .poni file with Browse…")) as f:
                text = f.read()
        except Invalid as e:
            qt.QMessageBox.warning(self, "Integration", str(e))
            return
        except OSError as e:
            qt.QMessageBox.warning(self, "Integration", "PONI {!r}: {}".format(path, e))
            return
        _settings().setValue("poni", path)
        self.refused = None
        self.integrator.request("set", poni=text, npt=self.npt.value())

    def _reply(self, method, body):
        if method == "config":
            self._active(body)
        elif method == "set":
            # The work thread may have applied it before this reply came.
            self.pending = None if body == self.active else body
            self._show()

    def _published(self, topic, body):
        if topic == "config":
            self.pending = None
            self._active(body)

    def _failed(self, method, message):
        if method == "set":
            self.refused = message
            self._show()

    def _active(self, body):
        self.active = body
        self.npt.setValue(int(body.get("npt") or self.npt.value()))
        self._show()

    def _show(self):
        lines = []
        for caption, c in (("Now", self.active), ("Next, after the queued frames", self.pending)):
            if c:
                lines.append("{}: npt {}, q to {:.4g} {}; {}".format(
                    caption, c.get("npt"), float(c.get("q_max") or 0), c.get("unit"), _poni_lines(c.get("poni"))))
        if self.refused:
            lines.append("Refused: " + self.refused)
        self.state.setText("\n".join(lines) or "integrator: not answering")
