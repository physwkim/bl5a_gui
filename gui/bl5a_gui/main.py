"""The BL5A main window: run buttons and status line on top; the
Alignment, Image and Stack tabs in the centre; panes the operator can
move, float, close and resize: around the tabs motors and the forms on
the left, the queue on the right, SPEC, commands and messages below; in
the Alignment tab the stages and alignment history below its scan plot."""

import argparse
import html
import logging
import sys

from silx.gui import qt

from . import bridge, config, docs, gate, items
from .consoles import CommandsView, MessagesView, SpecConsole
from .forms import BatteryForm, InsituForm, ScanForm
from .panels import AlignBox, DataBox, IntegrationBox, MotorsPanel, StatusBar
from .plots import ImageTab, ScanTab, StackTab
from .tables import HistoryTable, QueueTable, StagesTable

# SPEC's ^C: it stops every motor SPEC moves, a count and the command.
STOP_ALL = config.SPEC + "Cmd:Abort"


# Where the default layout docks each pane, in order: in the window or
# in the Alignment tab; and whether the panes of an area share it as
# tabs (the first in front) or split it.
PANES = (
    ("window", qt.Qt.LeftDockWidgetArea, True, ("setup", "motors")),
    ("window", qt.Qt.RightDockWidgetArea, False, ("queue",)),
    ("window", qt.Qt.BottomDockWidgetArea, True, ("spec", "commands", "messages")),
    ("alignment", qt.Qt.BottomDockWidgetArea, False, ("stages", "history")),
)
# A saved layout of another version is not restored: the panes it
# placed have moved since.
LAYOUT = 1


def _scrolled(widget):
    # A pane narrower or shorter than its contents scrolls them, so no
    # pane's contents stop the one beside it from growing.
    area = qt.QScrollArea()
    area.setWidgetResizable(True)
    area.setFrameShape(qt.QFrame.NoFrame)
    area.setWidget(widget)
    return area


class _Page(qt.QMainWindow):
    """A tab with panes of its own; a right click on their title bars
    opens the window's View menu."""

    def __init__(self, menu):
        super().__init__()
        self.setWindowFlags(qt.Qt.Widget)
        self._menu = menu

    def createPopupMenu(self):
        return self._menu()


class MainWindow(qt.QMainWindow):
    def __init__(self, args):
        super().__init__()
        self.setWindowTitle("BL5A")
        self.qs = None  # the last status, None while qs does not answer
        self.queue = {"items": [], "running_item": {}}
        self.finished = items.Finished()
        self.spec = dict.fromkeys(gate.SPEC_STATE)
        self.afterFrame = gate.StopAfterFrame()

        self.pvs = bridge.PVs(self)
        self.documents = bridge.Documents(args.qs_documents, self)
        self.commands = bridge.Commands(args.qs_control, self.documents.relay, args.user, self)
        self.RE = self.commands.RE
        self.poller = bridge.QsPoller(args.qs_control, parent=self)
        self.integrator = bridge.Integrator(args.integrator_rep, args.integrator_pub, self)

        # -- widgets ---------------------------------------------------
        self.status = StatusBar()
        self.motors = MotorsPanel(self.pvs)
        self.data = DataBox(self.pvs)
        self.integration = IntegrationBox(self.integrator)
        self.align = AlignBox()
        self.battery = BatteryForm()
        self.insitu = InsituForm()
        self.scanForm = ScanForm()
        forms = qt.QTabWidget()
        forms.addTab(self.battery, "Battery")
        forms.addTab(self.insitu, "In-situ")
        forms.addTab(self.scanForm, "Scan")
        setup = qt.QWidget()
        setupLayout = qt.QVBoxLayout(setup)
        setupLayout.setContentsMargins(0, 0, 0, 0)
        setupLayout.addWidget(self.data)
        setupLayout.addWidget(self.integration)
        setupLayout.addWidget(self.align)
        setupLayout.addWidget(forms, 1)

        self.scanTab = ScanTab()
        self.imageTab = ImageTab()
        self.stackTab = StackTab(self.integrator)
        self.alignment = _Page(self.createPopupMenu)  # the scan plot and the panes of an alignment
        self.alignment.setCentralWidget(self.scanTab)
        self.tabs = qt.QTabWidget()
        self.tabs.addTab(self.alignment, "Alignment")
        self.tabs.addTab(self.imageTab, "Image")
        self.tabs.addTab(self.stackTab, "Stack")

        self.setCentralWidget(_scrolled(self.tabs))

        self.queueView = QueueTable()
        self.stages = StagesTable()
        self.history = HistoryTable()
        self.console = SpecConsole(self.pvs)
        self.commandsView = CommandsView()
        self.messages = MessagesView()

        self.hosts = {"window": self, "alignment": self.alignment}
        for host in self.hosts.values():
            host.setDockOptions(host.dockOptions() | qt.QMainWindow.AllowNestedDocks)
            host.setTabPosition(qt.Qt.AllDockWidgetAreas, qt.QTabWidget.North)
        hostOf = {name: self.hosts[host] for host, _, _, names in PANES for name in names}
        self.panes = {}
        for name, title, widget in (
                ("motors", "Motors", self.motors), ("setup", "Setup", setup),
                ("queue", "Queue", self.queueView), ("stages", self.stages.caption, self.stages),
                ("history", "Alignment history", self.history), ("spec", "SPEC", self.console),
                ("commands", "Commands", self.commandsView), ("messages", "Messages", self.messages)):
            pane = qt.QDockWidget(title, hostOf[name])
            pane.setObjectName(name)  # its key in saveState
            pane.setWidget(_scrolled(widget))
            self.panes[name] = pane

        bar = self.addToolBar("Run")
        bar.setObjectName("run")
        bar.setMovable(False)
        self.pauseAct = bar.addAction("Pause")
        self.pauseAct.setToolTip("Pause the running plan now (re_pause immediate)")
        self.resumeAct = bar.addAction("Resume")
        self.stopAct = bar.addAction("Stop")
        self.stopAct.setToolTip("End the paused or running plan as a success (re_stop)")
        self.abortAct = bar.addAction("Abort")
        self.abortAct.setToolTip("End the running plan as aborted (re_abort)")
        self.pauseAct.triggered.connect(lambda: self.run("pause", self.RE.request_pause, defer=False))
        self.resumeAct.triggered.connect(lambda: self.run("resume", self.RE.resume))
        self.stopAct.triggered.connect(lambda: self.run("stop", self.RE.stop))
        self.abortAct.triggered.connect(self._abort)
        bar.addSeparator()
        self.stopAll = qt.QPushButton("Stop all")
        self.stopAll.setStyleSheet("QPushButton { background: #c62828; color: white; font-weight: bold;"
                                   " padding: 2px 14px; }")
        self.stopAll.setToolTip("SPEC's ^C ({}): stops every motor, count and command SPEC runs, and aborts "
                                "the running or paused plan. Always live; does not ask.".format(STOP_ALL))
        bar.addWidget(self.stopAll)
        self.stopAll.clicked.connect(self._stopAll)
        self.addToolBarBreak()
        line = self.addToolBar("Status")
        line.setObjectName("status")
        line.setMovable(False)
        self.status.setSizePolicy(qt.QSizePolicy.Expanding, qt.QSizePolicy.Preferred)
        line.addWidget(self.status)

        self.resetLayoutAct = qt.QAction("Reset layout", self)
        self.resetLayoutAct.setToolTip("Every pane shown and back where a first start puts it")
        self.resetLayoutAct.triggered.connect(self.defaultLayout)
        self._view(self.menuBar().addMenu("View"))
        self.resize(1850, 1000)
        self.defaultLayout()
        self._restoreLayout()

        # -- documents -------------------------------------------------
        self.docs = docs.Documents(
            on_scan_start=self.scanTab.start,
            on_scan_point=self.scanTab.point,
            on_scan_stop=self._scanStop,
            on_stage=self._stage,
            on_exposure=self._exposure,
            on_command=self.commandsView.add,
            on_run_start=self._runStart,
            on_run_stop=self._runStop,
        )
        self.documents.document.connect(self._document)

        # -- wiring ----------------------------------------------------
        self.poller.status.connect(self._status)
        self.poller.queue.connect(self._queue)
        self.poller.history.connect(self._history)
        self.poller.lost.connect(lambda e: self.messages.add("qs: " + e))
        self.commands.done.connect(self._done)
        self.commands.failed.connect(lambda label, e: self._failed(label, e))
        self.integrator.reply.connect(self._integratorReply)
        self.integrator.failed.connect(lambda m, e: self.messages.add("integrator {}: {}".format(m, e)))
        self.integrator.published.connect(self._integratorPub)
        self.pvs.changed.connect(self._pv)
        self.pvs.connected.connect(self._pvConnected)

        q = self.queueView
        q.sigStartRequested.connect(lambda: self.run("queue start", self.RE.queue_start))
        q.sigStopRequested.connect(lambda: self.run("queue stop", self.RE.queue_stop))
        q.sigEnvOpenRequested.connect(lambda: self.run("environment open", self.RE.environment_open))
        q.sigEnvCloseRequested.connect(lambda: self.run("environment close", self.RE.environment_close))
        q.sigRemoveRequested.connect(self._remove)
        q.sigMoveRequested.connect(self._move)
        q.sigClearRequested.connect(self._clear)
        q.sigAutostartRequested.connect(lambda on: self.run("autostart", self.RE.queue_autostart, on))
        self.align.queueRequested.connect(lambda item: self.add([item]))
        self.battery.queueRequested.connect(lambda item: self.add([item]))
        self.battery.stopAfterFrameRequested.connect(self._stopAfterFrame)
        self.insitu.queueRequested.connect(self.add)
        self.scanForm.queueRequested.connect(lambda item: self.add([item]))

        for suffix in gate.SPEC_STATE:
            self.pvs.watch(config.SPEC + suffix)
        self.pvs.watch(STOP_ALL)
        for name in (config.RING_PV, config.SHUTTER_PV, config.MAR + "DetectorState_RBV"):
            self.pvs.watch(name)
        temps = (config.TEMP_SP_PV, config.TEMP_RBV_PV, config.TEMP_RAMP_PV)
        self.temps = dict.fromkeys(t for t in temps if t)
        for name in self.temps:
            self.pvs.watch(name)
        self._gate()
        self.poller.start()
        self.stackTab.reload()

    # -- layout --------------------------------------------------------
    def defaultLayout(self):
        """Every pane shown and docked where a first start puts it."""
        for host, area, tabbed, names in PANES:
            host = self.hosts[host]
            for name in names:
                pane = self.panes[name]
                pane.setFloating(False)
                host.addDockWidget(area, pane)
                pane.show()
            if tabbed:
                for before, after in zip(names, names[1:]):
                    host.tabifyDockWidget(self.panes[before], self.panes[after])
                self.panes[names[0]].raise_()
        self.resizeDocks([self.panes["motors"], self.panes["queue"]],
                         [430, self.queueView.minimumSizeHint().width()], qt.Qt.Horizontal)
        self.resizeDocks([self.panes["spec"]], [200], qt.Qt.Vertical)
        self.alignment.resizeDocks([self.panes["stages"]], [230], qt.Qt.Vertical)

    def _view(self, menu):
        for pane in self.panes.values():
            menu.addAction(pane.toggleViewAction())
        menu.addSeparator()
        menu.addAction(self.resetLayoutAct)

    def createPopupMenu(self):
        """The View menu, also on a right click: it lists no toolbar, so
        Stop all cannot be hidden and stay hidden in the saved layout."""
        menu = qt.QMenu(self)
        self._view(menu)
        return menu

    def _restoreLayout(self):
        s = qt.QSettings(*config.SETTINGS)
        geometry = s.value("window/geometry")
        if geometry is not None:
            self.restoreGeometry(geometry)
        for key, host in self.hosts.items():
            state = s.value(key + "/state")
            if state is not None:
                host.restoreState(state, LAYOUT)

    def _saveLayout(self):
        s = qt.QSettings(*config.SETTINGS)
        s.setValue("window/geometry", self.saveGeometry())
        for key, host in self.hosts.items():
            s.setValue(key + "/state", host.saveState(LAYOUT))

    # -- qs ------------------------------------------------------------
    def run(self, label, fn, *args, **kwargs):
        self.commands.submit(label, fn, *args, **kwargs)

    def add(self, queued):
        """Queue the items in order, as one job, so a later click cannot
        slip in between them; each carries the Data folder as it is now."""
        try:
            queued = [items.placed(item, self.data.root.text()) for item in queued]
        except items.Invalid as e:
            qt.QMessageBox.warning(self, "Data", str(e))
            return

        def add_all():
            for item in queued:
                self.RE.queue_item_add(item)
            return len(queued)

        self.run("queue add {}".format(", ".join(i["name"] for i in queued)), add_all)

    def _abort(self):
        answer = qt.QMessageBox.question(self, "Abort", "Abort the running plan?",
                                         qt.QMessageBox.Yes | qt.QMessageBox.No, qt.QMessageBox.No)
        if answer == qt.QMessageBox.Yes:
            self._abortPlan()

    def _abortPlan(self):
        self.afterFrame.asked = False
        self._show()
        self.run("abort", self.RE.abort)

    def _stopAll(self):
        """SPEC's ^C whatever state qs and SPEC are in, and the plan
        aborted so it moves nothing after it; qs then stops what the plan
        still moves."""
        sent = self.pvs.watch(STOP_ALL).connected
        if sent:
            self.pvs.put(STOP_ALL, 1)
        if gate.allowed(self.qs, self.spec, self.afterFrame.asked)["abort"]:
            self._abortPlan()
        if sent:
            self.messages.add("Stop all: sent SPEC ^C")
        else:
            self.messages.add("Stop all: {} is not connected, SPEC got no ^C".format(STOP_ALL))
            qt.QMessageBox.warning(self, "Stop all", "{} is not connected: SPEC got no ^C. Stop the motors "
                                   "at SPEC's own terminal.".format(STOP_ALL))

    def _stopAfterFrame(self):
        self.afterFrame.asked = True
        self._show()
        self.run("stop after frame", self.RE.request_pause, defer=True)

    def _remove(self, uids):
        def remove():
            for uid in uids:
                self.RE.queue_item_remove(uid)

        self.run("queue remove", remove)

    def _move(self, uid, step):
        """One place earlier or later: before the item ahead of it, after the
        one behind it, named by uid as the queue stood when drawn."""
        uids = [i.get("item_uid") for i in self.queue.get("items") or []]
        if uid not in uids:
            return
        k = uids.index(uid) + step
        if 0 <= k < len(uids):
            where = {"before_uid": uids[k]} if step < 0 else {"after_uid": uids[k]}
            self.run("queue move", self.RE.queue_item_move, uid, **where)

    def _clear(self):
        answer = qt.QMessageBox.question(self, "Queue", "Remove every queued item?",
                                         qt.QMessageBox.Yes | qt.QMessageBox.No, qt.QMessageBox.No)
        if answer == qt.QMessageBox.Yes:
            self.run("queue clear", self.RE.queue_clear)

    def _done(self, label, result):
        self.messages.add("{}: done".format(label))

    def _failed(self, label, message):
        self.messages.add("{}: {}".format(label, message))
        if label == "stop after frame":
            self.afterFrame.asked = False
            self._show()
        qt.QMessageBox.warning(self, "bl5a-qs", "{}: {}".format(label, message))

    def _status(self, st):
        self.qs = st
        if self.afterFrame.status(st):
            self.run("stop", self.RE.stop)
        if st is not None:
            self.queueView.setEnvironment(st.get("worker_environment_exists"), st.get("re_state") == "idle")
            self.queueView.setAutostart(st.get("queue_autostart_enabled"))
        if st is None:
            self.queueView.setEnvironment(None)
            self.queueView.setAutostart(None)
        self._show()

    def _queue(self, reply):
        self.queue = reply
        self.queueView.setQueue(reply.get("running_item"), reply.get("items"))

    def _history(self, history):
        # The history, not the stop document, reports how each item ended:
        # an item that failed before its plan opened a run has no stop.
        for item in self.finished.new(history):
            self.messages.add(items.ended(item))

    def _show(self):
        self.status.setQs(self.qs, self.afterFrame.asked)
        self._gate()

    def _gate(self):
        """What the operator may do in this state (gate.allowed)."""
        ok = gate.allowed(self.qs, self.spec, self.afterFrame.asked)
        self.motors.setAllowed(ok["spec"])
        self.console.setAllowed(ok["spec"])
        self.pauseAct.setEnabled(ok["pause"])
        self.resumeAct.setEnabled(ok["resume"])
        self.stopAct.setEnabled(ok["stop"])
        self.abortAct.setEnabled(ok["abort"])
        self.battery.stopAfterFrame.setEnabled(ok["stop_after_frame"])

    # -- documents -----------------------------------------------------
    def _document(self, name, doc):
        try:
            self.docs.feed(name, doc)
        except Exception as e:  # one bad document must not stop the stream
            logging.exception("document %s", name)
            self.messages.add("document {}: {}".format(name, e))

    def _scanStop(self, scan):
        self.scanTab.stop(scan)

    def _stage(self, run, stage, scan):
        self.stages.add(stage)
        if scan is not None:
            self.scanTab.stage(scan, stage)

    def _exposure(self, run, exposure):
        # The frame's FullFileName_RBV, which bl5a-qs read after readout.
        self.status.set("file", html.escape(str(exposure.get("file") or "—")))
        self.imageTab.exposure(exposure.get("file"))
        self.messages.add("{}: frame {} ({})".format(run.plan, exposure.get("file"), run.series))

    def _runStart(self, run):
        self.messages.add("{} started ({})".format(run.plan, run.series))
        if run.plan in docs.ALIGNING:
            self.stages.start(run)
            self.panes["stages"].setWindowTitle(self.stages.caption)

    def _runStop(self, run, row):
        if row is not None:
            self.history.add(row)

    # -- integrator ----------------------------------------------------
    def _integratorReply(self, method, body):
        if method == "config" and body.get("poni"):
            self.imageTab.setPoni(body["poni"])

    def _integratorPub(self, topic, body):
        if topic == "error":
            self.messages.add("integrator: {}".format(body.get("error")))
        elif topic == "config":
            self.messages.add("integrator: npt {}, q to {:.4g} {} from now on".format(
                body.get("npt"), float(body.get("q_max") or 0), body.get("unit")))
            if body.get("poni"):
                self.imageTab.setPoni(body["poni"])

    # -- PVs -----------------------------------------------------------
    def _pv(self, name, value, text):
        spec = name[len(config.SPEC):] if name.startswith(config.SPEC) else None
        if spec in self.spec:
            self.spec[spec] = text
            self.status.setSpec(self.spec["Connected"] == "Connected" if self.spec["Connected"] else None,
                                self.spec["Busy"] == "Busy" or self.spec["Cmd:Busy"] == "Busy")
            self._gate()
        elif name == config.RING_PV:
            self.status.set("ring", "{:.1f} mA".format(float(value)))
        elif name == config.SHUTTER_PV:
            self.status.set("shutter", text)
        elif name == config.MAR + "DetectorState_RBV":
            self.status.set("mar", text)
        elif name in self.temps:
            self.temps[name] = float(value)
            self.status.setTemperature(self.temps.get(config.TEMP_SP_PV), self.temps.get(config.TEMP_RBV_PV),
                                       self.temps.get(config.TEMP_RAMP_PV))

    def _pvConnected(self, name, ok):
        if ok:
            return
        spec = name[len(config.SPEC):] if name.startswith(config.SPEC) else None
        if spec in self.spec:
            self.spec[spec] = None
            self.status.setSpec(None, False)
            self._gate()
        for key, pv in (("ring", config.RING_PV), ("shutter", config.SHUTTER_PV),
                        ("mar", config.MAR + "DetectorState_RBV")):
            if name == pv:
                self.status.set(key, "—")

    def closeEvent(self, event):
        if not self.align.editor.discard_ok():
            event.ignore()
            return
        self._saveLayout()
        self.poller.stop()
        self.commands.stop()
        self.documents.stop()
        self.integrator.stop()
        super().closeEvent(event)


def parse(argv):
    p = argparse.ArgumentParser(prog="bl5a-gui", description=__doc__.splitlines()[0])
    p.add_argument("--font-pt", type=int, default=config.FONT_PT, help="the font size in points (%(default)s)")
    return config.arguments(p).parse_args(argv)


def main(argv=None):
    args = parse(sys.argv[1:] if argv is None else argv)
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s")
    app = qt.QApplication.instance() or qt.QApplication(sys.argv[:1])
    app.setFont(qt.QFont(config.FONT, args.font_pt))
    w = MainWindow(args)
    w.show()
    return app.exec()
