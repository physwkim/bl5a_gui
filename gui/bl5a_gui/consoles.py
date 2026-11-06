"""The bottom tabs: SPEC's output and a command line to it, the commands
each plan sent, and the GUI's own messages."""

import time

from silx.gui import qt

from . import config
from .speclog import LogLines

MAX_LINES = 5000


def _text(parent):
    w = qt.QPlainTextEdit(parent)
    w.setReadOnly(True)
    w.setMaximumBlockCount(MAX_LINES)
    w.setLineWrapMode(qt.QPlainTextEdit.NoWrap)
    font = qt.QFontDatabase.systemFont(qt.QFontDatabase.FixedFont)
    w.setFont(font)
    return w


def stamp():
    return time.strftime("%H:%M:%S")


class SpecConsole(qt.QWidget):
    """SPEC's tty lines as spec-ioc's Log PV carries them, and a line that
    sends one command through BL5A:SPEC:Cmd. A burst longer than the Log
    PV's tail between two updates shows as lines not shown; a command's own
    reply comes whole from Cmd:Reply."""

    replied = qt.Signal()

    def __init__(self, pvs, parent=None):
        super().__init__(parent)
        self.pvs = pvs
        self.out = _text(self)
        self.entry = qt.QLineEdit()
        self.entry.setPlaceholderText("SPEC command (wa, p A[th], …)")
        self.send = qt.QPushButton("Send")
        self.abort = qt.QPushButton("^C")
        self.abort.setToolTip("BL5A:SPEC:Cmd:Abort: interrupt the command sent from here")
        line = qt.QHBoxLayout()
        line.addWidget(self.entry, 1)
        line.addWidget(self.send)
        line.addWidget(self.abort)
        layout = qt.QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(self.out, 1)
        layout.addLayout(line)
        self.pending = None
        self.allowed = False
        self.log = LogLines()
        self.replied.connect(self._replied)
        self.entry.returnPressed.connect(self._send)
        self.send.clicked.connect(self._send)
        self.abort.clicked.connect(lambda: self.pvs.put(config.SPEC + "Cmd:Abort", 1))
        pvs.watch(config.SPEC + "Log")
        pvs.changed.connect(self._changed)
        self.setAllowed(False)

    def setAllowed(self, allowed):
        """Commands only while no plan runs and SPEC is connected and idle;
        ^C only for the command sent from here, never a plan's."""
        self.allowed = allowed
        ok = allowed and self.pending is None
        self.entry.setEnabled(ok)
        self.send.setEnabled(ok)
        self.abort.setEnabled(self.pending is not None)

    def _changed(self, name, value, text):
        if name == config.SPEC + "Log":
            for line in self.log.new(text):
                self.out.appendPlainText(line)

    def _send(self):
        cmd = self.entry.text().strip()
        if not cmd or self.pending is not None or not self.send.isEnabled():
            return
        self.pending = cmd
        self.setAllowed(self.allowed)
        self.out.appendPlainText("{} SPEC> {}".format(stamp(), cmd))
        # The completion callback runs on a CA thread; the signal brings it
        # to this one before any CA call is made.
        self.pvs.put(config.SPEC + "Cmd", cmd, callback=self.replied.emit)

    def _replied(self):
        cmd, self.pending = self.pending, None
        status = self.pvs.get_text(config.SPEC + "Cmd:Status")
        reply = self.pvs.get_text(config.SPEC + "Cmd:Reply")
        if reply:
            self.out.appendPlainText(reply.rstrip("\n"))
        if status != "OK":
            self.out.appendPlainText("{} {!r}: {}".format(stamp(), cmd, status or "no status"))
        self.entry.clear()
        self.setAllowed(self.allowed)


class CommandsView(qt.QWidget):
    """The SPEC commands bl5a-qs sent for each plan, with their replies."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.out = _text(self)
        layout = qt.QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(self.out)

    def add(self, run, command):
        reply = str(command.get("reply") or "").rstrip("\n")
        self.out.appendPlainText("{} [{}] {}".format(stamp(), run.plan, command.get("cmd")))
        if reply:
            self.out.appendPlainText("    " + reply.replace("\n", "\n    "))


class MessagesView(qt.QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.out = _text(self)
        layout = qt.QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(self.out)

    def add(self, text):
        self.out.appendPlainText("{} {}".format(stamp(), text))
