"""Alignment sequences in the GUI: the combo that lists them, a block's
form and the sequence editor, a window of its own that reorders the
blocks, groups them into loops and saves the sequence as the user's
(`sequences`)."""

import copy
import html

from silx.gui import qt

from . import config, items, sequences
from .items import Invalid


def source():
    """(data folder, user) as last saved: where sequences are kept."""
    s = qt.QSettings(*config.SETTINGS)
    return str(s.value("data_root", "") or ""), str(s.value("align_user", "") or "")


def resolve(name):
    """(name, blocks) of the sequence `name` for the current user."""
    root, user = source()
    return name, sequences.get(name, root, user)


class SequenceCombo(qt.QComboBox):
    """The templates and the user's sequences, read again each time it
    opens."""

    def __init__(self, current=sequences.DEFAULT, parent=None):
        super().__init__(parent)
        self.setSizeAdjustPolicy(qt.QComboBox.AdjustToContents)
        self.refresh(current)

    def refresh(self, current=None):
        current = current or self.currentText()
        self.blockSignals(True)
        self.clear()
        names = sequences.names(*source())
        self.addItems(names)
        if len(names) > len(sequences.TEMPLATES):
            self.insertSeparator(len(sequences.TEMPLATES))
        # A sequence the user no longer has falls back to the default.
        found = self.findText(current)
        self.setCurrentIndex(found if found >= 0 else self.findText(sequences.DEFAULT))
        self.blockSignals(False)

    def showPopup(self):
        self.refresh()
        super().showPopup()


class UserCombo(qt.QComboBox):
    """The user, editable; it lists the users with a folder of sequences."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setEditable(True)
        self.setInsertPolicy(qt.QComboBox.NoInsert)
        self.setEditText(source()[1])

    def showPopup(self):
        text = self.currentText()
        self.blockSignals(True)
        self.clear()
        self.addItems(sequences.users(source()[0]))
        self.setEditText(text)
        self.blockSignals(False)
        super().showPopup()


def _line(text=""):
    return qt.QLineEdit(text)


def _g(v):
    return "" if v is None else "{:g}".format(v)


def _num(w, field):
    return items.number(w.text().strip(), field)


def _combo(names, current):
    c = qt.QComboBox()
    c.addItems(names)
    c.setCurrentIndex(max(0, c.findText(current)))
    return c


# A new block of each kind, as Add puts it before its form opens.
NEW = {
    "scan": {"kind": "scan", "type": "lup", "motor": "th", "start": -0.2, "finish": 0.2, "intervals": 20,
             "time": 0.5, "settle": 0.1, "counter": "det", "pick": "CEN"},
    "move": {"kind": "move", "moves": [["th", 0]]},
    "spec": {"kind": "spec", "command": ""},
    "wait": {"kind": "wait", "seconds": 1},
    "erase": {"kind": "erase"},
}
# Per loop a block is in: two em spaces, about the tree's own indent.
INDENT = "  "


def parse_moves(text):
    """"tth 1 th 0" as [["tth", 1.0], ["th", 0.0]]."""
    words = text.split()
    if not words or len(words) % 2:
        raise Invalid("moves: motor position pairs, as umv takes them, got {!r}".format(text))
    return [[m, items.number(x, "moves: " + m)] for m, x in zip(words[::2], words[1::2])]


def parse_when(text):
    """"martran < 1200" as {"motor", "op", "value"}; None when empty."""
    words = text.replace("<", " < ").replace(">", " > ").split()
    if not words:
        return None
    if len(words) != 3 or words[1] not in "<>":
        raise Invalid("when: motor < value or motor > value, e.g. martran < 1200; got {!r}".format(text))
    return {"motor": words[0], "op": words[1], "value": items.number(words[2], "when: value")}


class BlockDialog(qt.QDialog):
    """One block's fields, by its kind; OK only when qs would take it."""

    def __init__(self, block, parent=None):
        super().__init__(parent)
        self.kind = block["kind"]
        self.setWindowTitle("Block: " + self.kind)
        form = qt.QFormLayout(self)
        b = block
        if self.kind == "scan":
            self.type = _combo(list(items.SCAN_TYPES), b["type"])
            self.type.setToolTip("ascan/cscan: absolute; lup/clup: relative to where the motor is.\n"
                                 "ascan/lup: steps; cscan/clup: SPEC's fly scan.")
            self.motor = _combo(config.MOTORS, b["motor"])
            self.start, self.finish = _line(_g(b["start"])), _line(_g(b["finish"]))
            self.intervals = qt.QSpinBox()
            self.intervals.setRange(1, 100000)
            self.intervals.setValue(int(b.get("intervals") or 20))
            self.time = _line(_g(b["time"]))
            self.time.setToolTip("Seconds per point (ascan, lup) or of the whole move (cscan, clup)")
            self.settle = _line(_g(b.get("settle", 0.1)))
            self.settle.setToolTip("ascan/lup: seconds to wait at each point before counting")
            self.sleep = _line(_g(b.get("sleep", 0.05)))
            self.sleep.setToolTip("cscan/clup: SPEC's sleep between the fly scan's samples, seconds")
            self.counter = _combo(items.COUNTERS, b["counter"])
            self.pick = _combo(items.PICKS, b["pick"])
            self.pick.setToolTip("Where the motor goes after the scan: CEN (FWHM centre), COM (centre of mass),\n"
                                 "xMAX (the maximum) or EDGE (the half-height edge of a step)")
            self.dip = qt.QCheckBox("of a dip")
            self.dip.setChecked(bool(b.get("dip")))
            self.dip.setToolTip("The scan dips rather than peaks: CEN, COM and xMAX of the scan turned over,\n"
                                "its maximum less each point. Not for EDGE.")
            self.set = _line(_g(b.get("set")))
            self.set.setToolTip("Empty: keep the motor's position; a number: `set motor <number>` there")
            for label, w in (("Type", self.type), ("Motor", self.motor), ("Start", self.start),
                             ("Finish", self.finish), ("Intervals", self.intervals), ("Time [s]", self.time),
                             ("Settle [s]", self.settle), ("Sleep [s]", self.sleep), ("Counter", self.counter),
                             ("Go to", self.pick), ("", self.dip), ("Set to", self.set)):
                form.addRow(label, w)
            self.type.currentTextChanged.connect(self._type)
            self._type()
            self.pick.currentTextChanged.connect(self._pick)
            self._pick()
        elif self.kind == "move":
            self.moves = _line(" ".join("{} {}".format(m, _g(x)) for m, x in b["moves"]))
            self.moves.setToolTip("Motor position pairs, moved together as umv moves them: tth 1 th 0")
            form.addRow("umv", self.moves)
        elif self.kind == "spec":
            self.command = _line(b["command"])
            self.command.setToolTip("One SPEC command line")
            form.addRow("Command", self.command)
        elif self.kind == "wait":
            self.seconds = _line(_g(b["seconds"]))
            form.addRow("Seconds", self.seconds)
        elif self.kind == "loop":
            self.inner = b["blocks"]
            self.times = qt.QSpinBox()
            self.times.setRange(1, 100000)
            self.times.setValue(int(b["times"]))
            form.addRow("Times", self.times)
            form.addRow(qt.QLabel("Runs its {} blocks in order, this many times over.".format(len(self.inner))))
        else:
            form.addRow(qt.QLabel("Erases the mar345 plate, as the macros do after aligning."))
        w = b.get("when")
        self.when = _line("" if not w else "{} {} {}".format(w["motor"], w["op"], _g(w["value"])))
        self.when.setToolTip("Run the block only when a motor is below or above a value, e.g. martran < 1200;\n"
                             "empty: always")
        self.note = _line(b.get("note") or "")
        form.addRow("Only when", self.when)
        form.addRow("Note", self.note)
        buttons = qt.QDialogButtonBox(qt.QDialogButtonBox.Ok | qt.QDialogButtonBox.Cancel)
        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)
        form.addRow(buttons)
        self.result_block = None

    def _type(self):
        fly = items.SCAN_TYPES[self.type.currentText()]
        self.intervals.setEnabled(not fly)
        self.settle.setEnabled(not fly)
        self.sleep.setEnabled(fly)

    def _pick(self):
        self.dip.setEnabled(self.pick.currentText() != "EDGE")

    def block(self):
        """The block the fields give, as qs takes it; Invalid otherwise."""
        b = {"kind": self.kind}
        if self.kind == "scan":
            t = self.type.currentText()
            b.update(type=t, motor=self.motor.currentText(), start=_num(self.start, "start"),
                     finish=_num(self.finish, "finish"), time=_num(self.time, "time"))
            if items.SCAN_TYPES[t]:
                b["sleep"] = _num(self.sleep, "sleep")
            else:
                b.update(intervals=self.intervals.value(), settle=_num(self.settle, "settle"))
            b.update(counter=self.counter.currentText(), pick=self.pick.currentText())
            if self.dip.isEnabled() and self.dip.isChecked():
                b["dip"] = True
            if self.set.text().strip():
                b["set"] = _num(self.set, "set")
        elif self.kind == "move":
            b["moves"] = parse_moves(self.moves.text())
        elif self.kind == "spec":
            b["command"] = self.command.text().strip()
        elif self.kind == "wait":
            b["seconds"] = _num(self.seconds, "seconds")
        elif self.kind == "loop":
            b.update(times=self.times.value(), blocks=self.inner)
        when = parse_when(self.when.text())
        if when:
            b["when"] = when
        if self.note.text().strip():
            b["note"] = self.note.text().strip()
        return items.block("block", b)

    def accept(self):
        try:
            self.result_block = self.block()
        except Invalid as e:
            qt.QMessageBox.warning(self, "Block", str(e))
            return
        super().accept()


class SequenceEditor(qt.QWidget):
    """The loaded sequence's blocks, a loop's inside it: add, edit,
    reorder, loop and remove them, and save the sequence as the user's. A
    template is saved under another name. A window of its own, shown
    on demand; closing it keeps the blocks."""

    changed = qt.Signal()

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setWindowFlags(qt.Qt.Window)
        self.name, self.blocks, self.dirty = "", [], False
        layout = qt.QVBoxLayout(self)
        self.title = qt.QLabel()
        layout.addWidget(self.title)
        body = qt.QHBoxLayout()
        self.tree = qt.QTreeWidget()
        self.tree.setHeaderLabels(["#", "Block", "Note"])
        self.tree.setSelectionMode(qt.QAbstractItemView.ExtendedSelection)
        self.tree.setAlternatingRowColors(True)
        self.tree.setUniformRowHeights(True)
        body.addWidget(self.tree, 1)
        side = qt.QVBoxLayout()
        add = qt.QPushButton("Add")
        menu = qt.QMenu(add)
        for kind in NEW:
            menu.addAction(kind, lambda kind=kind: self._add(kind))
        add.setMenu(menu)
        add.setToolTip("A new block after the selected ones, in their list")
        edit, dup, remove = qt.QPushButton("Edit…"), qt.QPushButton("Duplicate"), qt.QPushButton("Remove")
        up, down = qt.QPushButton("Up"), qt.QPushButton("Down")
        group, ungroup = qt.QPushButton("Loop…"), qt.QPushButton("Ungroup")
        group.setToolTip("Run the selected neighbouring blocks N times over, as one loop block")
        ungroup.setToolTip("Put the selected loop's blocks in its place, run once")
        for w in (add, edit, dup, remove, up, down):
            side.addWidget(w)
        side.addSpacing(12)
        side.addWidget(group)
        side.addWidget(ungroup)
        side.addStretch()
        body.addLayout(side)
        layout.addLayout(body, 1)
        bottom = qt.QHBoxLayout()
        self.saveButton, saveAs = qt.QPushButton("Save"), qt.QPushButton("Save as…")
        self.deleteButton = qt.QPushButton("Delete")
        self.deleteButton.setToolTip("Delete this saved sequence")
        close = qt.QPushButton("Close")
        close.setToolTip("Close the window; the blocks stay as they are")
        for w in (self.saveButton, saveAs, self.deleteButton):
            bottom.addWidget(w)
        bottom.addStretch()
        bottom.addWidget(close)
        layout.addLayout(bottom)
        edit.clicked.connect(self._edit)
        self.tree.itemDoubleClicked.connect(lambda item, col: self._edit())
        dup.clicked.connect(self._duplicate)
        remove.clicked.connect(self._remove)
        up.clicked.connect(lambda: self._move(-1))
        down.clicked.connect(lambda: self._move(1))
        group.clicked.connect(self._group)
        ungroup.clicked.connect(self._ungroup)
        self.saveButton.clicked.connect(self._save)
        saveAs.clicked.connect(self._saveAs)
        self.deleteButton.clicked.connect(self._delete)
        close.clicked.connect(self.close)
        self.sized = False

    def open(self):
        """Show the window, large the first time, in front."""
        if not self.sized:
            screen = (self.parentWidget() or self).screen().availableGeometry()
            self.resize(min(1100, int(screen.width() * 0.8)), int(screen.height() * 0.8))
            self.sized = True
        self.show()
        self.raise_()
        self.activateWindow()

    def load(self, name, blocks):
        """Show `name`'s blocks, once unsaved changes may be discarded;
        whether it did."""
        if not self.discard_ok():
            return False
        self.name, self.blocks, self.dirty = name, copy.deepcopy(blocks), False
        self._show([(0,)])
        return True

    def discard_ok(self):
        """Whether the shown blocks may go: saved, or the user says so."""
        if not self.dirty:
            return True
        answer = qt.QMessageBox.question(self, "Alignment", "Discard the changes to {!r}?".format(self.name),
                                         qt.QMessageBox.Discard | qt.QMessageBox.Cancel, qt.QMessageBox.Cancel)
        return answer == qt.QMessageBox.Discard

    def label(self):
        """The name a queued run records; changed blocks are not the saved
        sequence."""
        return self.name + " (edited)" if self.dirty else self.name

    def _show(self, select=()):
        template = self.name in sequences.TEMPLATES
        self.setWindowTitle("Alignment blocks — {}{}".format(self.name, " *" if self.dirty else ""))
        self.title.setText("<b>{}</b>{} — {}".format(
            html.escape(self.name), " *" if self.dirty else "",
            "template, read-only: Save as… keeps a copy" if template else "the user's"))
        self.saveButton.setEnabled(not template)
        self.deleteButton.setEnabled(not template)
        self.tree.clear()
        found = []

        def add(parent, blocks, path):
            for i, b in enumerate(blocks):
                p = path + (i,)
                line = sequences.text(b)
                # Indented as deep as the loops it is in, as its number is.
                item = qt.QTreeWidgetItem(parent, [sequences.place(p), INDENT * len(path) + line, b.get("note") or ""])
                item.setToolTip(1, line)
                item.setToolTip(2, item.text(2))
                if p in select:
                    found.append(item)
                if b["kind"] == "loop":
                    add(item, b["blocks"], p)

        add(self.tree.invisibleRootItem(), self.blocks, ())
        self.tree.expandAll()
        self.tree.resizeColumnToContents(0)
        self.tree.resizeColumnToContents(1)
        for item in found:
            item.setSelected(True)
        if found:
            self.tree.setCurrentItem(found[0], 0, qt.QItemSelectionModel.NoUpdate)
            self.tree.scrollToItem(found[0])
        self.changed.emit()

    @staticmethod
    def _path(item):
        return tuple(int(i) - 1 for i in item.text(0).split("."))

    def _selected(self):
        """(parent, lo, hi): the selected blocks, neighbours in one list (a
        selected loop stands for what is inside it); None when nothing is
        selected. Others are refused with a warning, and give False."""
        paths = sorted(self._path(i) for i in self.tree.selectedItems())
        paths = [p for p in paths if not any(p[:len(q)] == q for q in paths if len(q) < len(p))]
        if not paths:
            return None
        parent, first = paths[0][:-1], paths[0][-1]
        if paths != [parent + (first + k,) for k in range(len(paths))]:
            qt.QMessageBox.warning(self, "Alignment", "Select neighbouring blocks in one list.")
            return False
        return parent, first, first + len(paths) - 1

    def _changed(self, select):
        if select is None:
            return
        self.dirty = True
        self._show(select)

    def _add(self, kind):
        sel = self._selected()
        if sel is False:
            return
        d = BlockDialog(copy.deepcopy(NEW[kind]), self)
        if d.exec() != qt.QDialog.Accepted:
            return
        parent, at = ((), len(self.blocks)) if sel is None else (sel[0], sel[2] + 1)
        self._changed(sequences.insert(self.blocks, parent, at, d.result_block))

    def _edit(self):
        item = self.tree.currentItem()
        if item is None:
            return
        path = self._path(item)
        run = sequences.siblings(self.blocks, path[:-1])
        d = BlockDialog(run[path[-1]], self)
        if d.exec() == qt.QDialog.Accepted and d.result_block != run[path[-1]]:
            run[path[-1]] = d.result_block
            self._changed([path])

    def _duplicate(self):
        sel = self._selected()
        if sel:
            self._changed(sequences.duplicate(self.blocks, *sel))

    def _remove(self):
        sel = self._selected()
        if sel:
            self._changed(sequences.remove(self.blocks, *sel))

    def _move(self, step):
        sel = self._selected()
        if sel:
            self._changed(sequences.move(self.blocks, *sel, step))

    def _group(self):
        sel = self._selected()
        if not sel:
            return
        times, ok = qt.QInputDialog.getInt(self, "Loop", "Run the selected blocks, in order, this many times:",
                                           2, 1, 100000)
        if ok:
            self._changed(sequences.group(self.blocks, *sel, times))

    def _ungroup(self):
        sel = self._selected()
        if not sel:
            return
        parent, lo, hi = sel
        if lo != hi or sequences.siblings(self.blocks, parent)[lo]["kind"] != "loop":
            qt.QMessageBox.warning(self, "Alignment", "Select one loop block.")
            return
        self._changed(sequences.ungroup(self.blocks, parent + (lo,)))

    def _write(self, name):
        root, user = source()
        try:
            sequences.save(name, self.blocks, root, user)
        except (Invalid, OSError) as e:
            qt.QMessageBox.warning(self, "Save", str(e))
            return False
        self.name, self.dirty = name, False
        self._show([self._path(i) for i in self.tree.selectedItems()])
        return True

    def _save(self):
        if self.name in sequences.TEMPLATES:
            return self._saveAs()
        return self._write(self.name)

    def _saveAs(self):
        name, ok = qt.QInputDialog.getText(self, "Save as", "Sequence name:", text=self.name)
        name = name.strip()
        if not ok or not name:
            return False
        if name in sequences.saved(*source()) and name != self.name:
            answer = qt.QMessageBox.question(self, "Save as", "Replace the saved sequence {!r}?".format(name),
                                             qt.QMessageBox.Yes | qt.QMessageBox.No, qt.QMessageBox.No)
            if answer != qt.QMessageBox.Yes:
                return False
        return self._write(name)

    def _delete(self):
        """The saved file goes; the default template is shown instead."""
        answer = qt.QMessageBox.question(self, "Delete", "Delete the saved sequence {!r}?".format(self.name),
                                         qt.QMessageBox.Yes | qt.QMessageBox.No, qt.QMessageBox.No)
        if answer != qt.QMessageBox.Yes:
            return
        root, user = source()
        try:
            sequences.delete(self.name, root, user)
        except (Invalid, OSError) as e:
            qt.QMessageBox.warning(self, "Delete", str(e))
            return
        self.dirty = False
        self.load(sequences.DEFAULT, sequences.get(sequences.DEFAULT))
