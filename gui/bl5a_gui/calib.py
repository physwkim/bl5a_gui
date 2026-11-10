"""The Calibration tab: the plate's PONI refined on a frame of a
calibrant by the integrator's `calibrate`, Dioptas's automatic
refinement in rsFAI. From the start PONI it finds the peaks within Δ2θ
of each ring in turn, from the innermost, and refines the geometry on
all of them after each ring. Peaks picked by hand are refined on first,
for a start too far off for the search to find the inner rings."""

import collections
import math
import os
import time

import numpy as np
from silx.gui import qt
from silx.gui.colors import Colormap
from silx.gui.plot import Plot2D

from . import items, mar
from .editor import dialog_dir
from .plots import Loader

# Cubic calibrants pyFAI ships, by lattice constant (Å) and the lattice
# its .D file lists: P every hkl, F hkl all odd or all even, D (diamond)
# those of F all odd or with h + k + l a multiple of 4.
CALIBRANTS = {"LaB6": (4.156826, "P"), "CeO2": (5.411651, "F"), "Si": (5.4311946, "D"), "Al": (4.0495, "F")}
CUBIC = "Cubic P"
D_FILE = ".D file"
# h² + k² + l² up to this: d below a third of the smallest a, past any
# ring a plate here sees.
N_MAX = 200
# The table's rows: the name the integrator fixes by, mar.parse_poni's
# key, the factor to the unit shown, and the unit.
ROWS = (("dist", "distance", 1e3, "mm"), ("poni1", "poni1", 1e3, "mm"), ("poni2", "poni2", 1e3, "mm"),
        ("rot1", "rot1", 180 / math.pi, "°"), ("rot2", "rot2", 180 / math.pi, "°"),
        ("rot3", "rot3", 180 / math.pi, "°"))
# The upper limit "none" sends: above any count a frame holds.
NO_LIMIT = 1e30
# A pick snaps to the brightest pixel this many pixels each way.
SNAP = 5
# Results whose id has not come back yet, kept: a calibrate that fails
# at once publishes before its reply names the id.
EARLY = 16
START, REFINED, NO_RINGS = "Start", "Refined", "No rings"
INTEGRATOR = "the integrator's"


def d_spacings(a, lattice):
    """The distinct d (Å) of the cubic `lattice` ("P", "F" or "D") of
    lattice constant `a`, largest first."""
    ns = set()
    top = math.isqrt(N_MAX)
    for h in range(top + 1):
        for k in range(h + 1):
            for m in range(k + 1):
                n = h * h + k * k + m * m
                odd = h % 2 + k % 2 + m % 2
                if not 0 < n <= N_MAX or (lattice != "P" and odd not in (0, 3)):
                    continue
                if lattice == "D" and odd == 0 and (h + k + m) % 4:
                    continue
                ns.add(n)
    return [a / math.sqrt(n) for n in sorted(ns)]


def read_d(text):
    """The d (Å) of a pyFAI .D file, largest first: each line's first
    number; `#` starts a comment."""
    ds = set()
    for line in text.splitlines():
        words = line.split("#", 1)[0].split()
        if words:
            d = float(words[0])
            if not d > 0:
                raise ValueError("d {!r}: not positive".format(words[0]))
            ds.add(d)
    if not ds:
        raise ValueError("no d spacing")
    return sorted(ds, reverse=True)


class _Run:
    """A calibrate sent: its id once the reply names it, and what it was
    sent with."""

    def __init__(self, path, start):
        self.id = None
        self.path = path
        self.start = start
        self.t0 = time.monotonic()


def _spin(lo, hi, value, decimals=None, step=None, tip=""):
    box = qt.QSpinBox() if decimals is None else qt.QDoubleSpinBox()
    if decimals is not None:
        box.setDecimals(decimals)
    box.setRange(lo, hi)
    box.setValue(value)
    if step is not None:
        box.setSingleStep(step)
    box.setToolTip(tip)
    return box


class _Side(qt.QScrollArea):
    """Scrolls its panel up and down only: never narrower than the panel, so
    the plot beside it gives up width rather than the panel's numbers."""

    def __init__(self, panel):
        super().__init__()
        self.setWidgetResizable(True)
        self.setFrameShape(qt.QFrame.NoFrame)
        self.setHorizontalScrollBarPolicy(qt.Qt.ScrollBarAlwaysOff)
        self.setWidget(panel)

    def minimumSizeHint(self):
        return qt.QSize(self.widget().minimumSizeHint().width() + self.verticalScrollBar().sizeHint().width(),
                        super().minimumSizeHint().height())

    def event(self, e):
        # The panel's own updateGeometry() arrives here by way of the viewport:
        # the splitter must hear that the panel widened as the result filled.
        if e.type() == qt.QEvent.LayoutRequest:
            self.updateGeometry()
        return super().event(e)


class CalibrationTab(qt.QWidget):
    """A calibrant frame with its rings where a PONI puts them; Refine
    sends it to the integrator, Save and apply writes the refined PONI and
    hands it to the Integration box (`applyRequested(path)`)."""

    applyRequested = qt.Signal(str)

    def __init__(self, integrator, parent=None):
        super().__init__(parent)
        self.integrator = integrator
        self.plot = Plot2D(self)
        self.plot.setKeepDataAspectRatio(True)
        self.plot.setDefaultColormap(Colormap("viridis", normalization=Colormap.LOGARITHM))

        self.framePath = qt.QLineEdit()
        self.framePath.setReadOnly(True)
        self.framePath.setPlaceholderText("no frame")
        self.latestButton = qt.QPushButton("Latest")
        self.latestButton.setToolTip("The last frame the mar345 took")
        self.latestButton.setEnabled(False)
        self.openButton = qt.QPushButton("Open…")

        self.startLabel = qt.QLabel("none yet")
        self.fromIntegrator = qt.QPushButton("Integrator's")
        self.fromIntegrator.setToolTip("The PONI the integrator integrates with; it follows the integrator's")
        self.fromIntegrator.setEnabled(False)
        self.loadPoni = qt.QPushButton("Load…")
        self.loadPoni.setToolTip("A PONI of the full 345 mm plate")
        self.fromResult = qt.QPushButton("Result")
        self.fromResult.setToolTip("The last refined PONI, to refine again from")
        self.fromResult.setEnabled(False)

        self.calibrant = qt.QComboBox()
        self.calibrant.addItems(list(CALIBRANTS) + [CUBIC, D_FILE])
        self.calibrant.setToolTip("LaB6, CeO2, Si and Al are pyFAI's (NIST SRM 660c, 674b, 640c); "
                                  "Cubic P takes every hkl of the a given")
        self.lattice = _spin(1.0, 100.0, 3.95, decimals=5, step=0.01, tip="Lattice constant")
        self.lattice.setSuffix(" Å")
        self.dPath = qt.QLineEdit()
        self.dPath.setPlaceholderText("pyFAI .D file of d spacings")
        self.dBrowse = qt.QPushButton("Browse…")

        self.rings = _spin(1, 100, 15, tip="Rings searched, from the innermost (Dioptas's Number of rings)")
        self.delta = _spin(0.001, 5.0, 0.1, decimals=3, step=0.01,
                           tip="Half width of the band around each ring its peaks are searched in "
                               "(Dioptas's Delta 2Th)")
        self.delta.setSuffix("°")
        self.factor = _spin(0.0, 100.0, 1.0, decimals=2, step=0.1,
                            tip="A peak's pixels exceed factor × the band's mean + its standard deviation "
                                "(Dioptas's Intensity Mean Factor, 3 there). On the sim's broad rings 3 finds "
                                "only the inner two; 1 finds all six.")
        self.upper = _spin(0, 2**31 - 1, 0, tip="Pixels above this count are left out, saturated ones "
                                                 "(Dioptas's Intensity Limit); none keeps every pixel")
        self.upper.setSpecialValueText("none")
        self.fixed = {}
        for name, *_ in ROWS:
            box = qt.QCheckBox(name)
            box.setToolTip("Held at the start value")
            self.fixed[name] = box
        self.fixed["rot3"].setChecked(True)
        self.fixed["rot3"].setToolTip("Held at the start value. rot3 turns the plate about the beam, "
                                      "which round rings barely show")

        self.pick = qt.QPushButton("Pick")
        self.pick.setCheckable(True)
        self.pick.setToolTip("While down, a click on the image picks the brightest pixel within {} pixels as a "
                             "peak of the ring #. Picked peaks are refined on before the search, for a start "
                             "too far off for it.".format(SNAP))
        self.pickRing = _spin(1, 100, 1, tip="The ring a pick is on, 1 the innermost")
        self.clearPicks = qt.QPushButton("Clear")
        self.pickCount = qt.QLabel("none")

        self.refineButton = qt.QPushButton("Refine")
        self.refineButton.setToolTip("Send the frame, start PONI, calibrant and picks to the integrator")
        self.state = qt.QLabel()
        self.state.setWordWrap(True)
        self.state.setTextFormat(qt.Qt.PlainText)
        self.state.setTextInteractionFlags(qt.Qt.TextSelectableByMouse)
        self.ringsOf = qt.QComboBox()
        self.ringsOf.addItems([START, REFINED, NO_RINGS])
        self.ringsOf.setToolTip("The PONI the calibrant's rings are drawn with")
        self.table = qt.QTableWidget(len(ROWS), 3)
        self.table.setHorizontalHeaderLabels(["Start", "Refined", "Change"])
        self.table.setVerticalHeaderLabels(["{} [{}]".format(name, unit) for name, _, _, unit in ROWS])
        self.table.setEditTriggers(qt.QAbstractItemView.NoEditTriggers)
        # Columns as wide as their widest cell and the table as wide as its
        # columns, so no number is cut short; _Side keeps the panel whole.
        self.table.horizontalHeader().setSectionResizeMode(qt.QHeaderView.ResizeToContents)
        self.table.setSizeAdjustPolicy(qt.QAbstractScrollArea.AdjustToContents)
        self.table.setSizePolicy(qt.QSizePolicy.Fixed, self.table.sizePolicy().verticalPolicy())
        self.table.setHorizontalScrollBarPolicy(qt.Qt.ScrollBarAlwaysOff)
        self.table.setVerticalScrollBarPolicy(qt.Qt.ScrollBarAlwaysOff)
        self.saveButton = qt.QPushButton("Save and apply…")
        self.saveButton.setToolTip("Write the refined PONI to a file and send it as the Integration box's "
                                   "Apply does: frames after those queued are integrated with it")
        self.saveButton.setEnabled(False)
        self._layout()

        self.frame = None  # (path, shape, pixel)
        self.latest = None  # the last exposure's file
        self.start = None  # (source, text, parsed)
        self.integratorPoni = None
        self.picks = []  # (row, column, ring from 0)
        self.result = None  # {"path", "text", "poni", "points", "chi2"}
        self.current = None  # the _Run whose result is awaited
        self._sent = collections.deque()  # runs whose reply has not come, in the order sent
        self._early = collections.OrderedDict()  # id -> a result that came before its reply

        self.loader = Loader(self)
        self.loader.loaded.connect(self._loaded)
        self.loader.failed.connect(lambda p, e: self._say("{}: {}".format(p, e)))
        self.latestButton.clicked.connect(lambda: self.load(self.latest))
        self.openButton.clicked.connect(self._open)
        self.fromIntegrator.clicked.connect(lambda: self._setStart(INTEGRATOR, self.integratorPoni))
        self.loadPoni.clicked.connect(self._loadPoni)
        self.fromResult.clicked.connect(lambda: self._setStart("the result", self.result["text"]))
        self.calibrant.currentIndexChanged.connect(self._calibrantChanged)
        self.lattice.editingFinished.connect(self._drawRings)
        self.dPath.editingFinished.connect(self._drawRings)
        self.dBrowse.clicked.connect(self._browseD)
        self.rings.valueChanged.connect(lambda _: self._drawRings())
        self.ringsOf.activated.connect(lambda _: self._drawRings())
        self.clearPicks.clicked.connect(self._clearPicks)
        self.plot.sigPlotSignal.connect(self._plotSignal)
        self.refineButton.clicked.connect(self.refine)
        self.saveButton.clicked.connect(self._save)
        integrator.reply.connect(self._reply)
        integrator.published.connect(self._published)
        integrator.failed.connect(self._failed)
        self._calibrantChanged()
        self._table()

    def _layout(self):
        frame = qt.QGroupBox("Frame")
        g = qt.QGridLayout(frame)
        g.addWidget(self.framePath, 0, 0, 1, 2)
        g.addWidget(self.latestButton, 1, 0)
        g.addWidget(self.openButton, 1, 1)
        start = qt.QGroupBox("Start PONI")
        g = qt.QGridLayout(start)
        g.addWidget(self.startLabel, 0, 0, 1, 3)
        g.addWidget(self.fromIntegrator, 1, 0)
        g.addWidget(self.loadPoni, 1, 1)
        g.addWidget(self.fromResult, 1, 2)
        calibrant = qt.QGroupBox("Calibrant")
        g = qt.QGridLayout(calibrant)
        g.addWidget(self.calibrant, 0, 0, 1, 2)
        g.addWidget(qt.QLabel("a"), 1, 0)
        g.addWidget(self.lattice, 1, 1)
        g.addWidget(self.dPath, 2, 0)
        g.addWidget(self.dBrowse, 2, 1)
        search = qt.QGroupBox("Ring search")
        f = qt.QFormLayout(search)
        f.addRow("Rings", self.rings)
        f.addRow("Δ2θ", self.delta)
        f.addRow("Mean factor", self.factor)
        f.addRow("Upper limit", self.upper)
        fixed = qt.QGridLayout()
        for k, box in enumerate(self.fixed.values()):
            fixed.addWidget(box, k // 3, k % 3)
        f.addRow("Fixed", fixed)
        picks = qt.QGroupBox("Picked peaks")
        g = qt.QGridLayout(picks)
        g.addWidget(self.pick, 0, 0)
        g.addWidget(qt.QLabel("Ring #"), 0, 1)
        g.addWidget(self.pickRing, 0, 2)
        g.addWidget(self.clearPicks, 0, 3)
        g.addWidget(self.pickCount, 1, 0, 1, 4)
        result = qt.QGroupBox("Result")
        g = qt.QGridLayout(result)
        g.addWidget(qt.QLabel("Rings of"), 0, 0)
        g.addWidget(self.ringsOf, 0, 1)
        g.addWidget(self.table, 1, 0, 1, 2)
        g.addWidget(self.saveButton, 2, 0, 1, 2)
        self.table.setMinimumHeight(self.table.verticalHeader().length() + self.table.horizontalHeader().height()
                                    + 2 * self.table.frameWidth())
        panel = qt.QWidget()
        v = qt.QVBoxLayout(panel)
        for w in (frame, start, calibrant, search, picks, self.refineButton, self.state, result):
            v.addWidget(w)
        v.addStretch(1)
        split = qt.QSplitter(qt.Qt.Horizontal)
        split.addWidget(_Side(panel))
        split.addWidget(self.plot)
        split.setStretchFactor(1, 1)
        split.setSizes([420, 1000])
        layout = qt.QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(split)

    # -- frame -----------------------------------------------------------
    def exposure(self, path):
        """The mar345's last frame, which Latest loads."""
        if path:
            self.latest = path
            self.latestButton.setEnabled(True)
            self.latestButton.setToolTip(path)

    def load(self, path):
        self._say("loading {}".format(path))
        self.loader.load(path)

    def _open(self):
        here = dialog_dir(os.path.dirname(self.frame[0] if self.frame else self.latest or ""))
        path, _ = qt.QFileDialog.getOpenFileName(self, "Calibrant frame", here, "mar345 (*.mar*);;All files (*)")
        if path:
            self.load(path)

    def _loaded(self, path, image, pixel):
        self.frame = (path, image.shape, pixel)
        self.picks = []
        # A log scale needs positive values; counts of 0 are drawn as 1.
        self.plot.addImage(np.maximum(image, 1), legend="frame", resetzoom=True)
        self.plot.setGraphTitle(os.path.basename(path))
        self.framePath.setText(path)
        self._say("{} × {}, {:g} µm, max {}".format(image.shape[0], image.shape[1], pixel * 1e6, int(image.max())))
        self._drawPicks()
        self._drawPeaks()
        self._drawRings()

    # -- start PONI ------------------------------------------------------
    def _config(self, body):
        if body.get("poni"):
            self.integratorPoni = body["poni"]
            self.fromIntegrator.setEnabled(True)
            if self.start is None or self.start[0] == INTEGRATOR:
                self._setStart(INTEGRATOR, self.integratorPoni)

    def _loadPoni(self):
        path, _ = qt.QFileDialog.getOpenFileName(self, "Start PONI", dialog_dir(), "PONI (*.poni);;All files (*)")
        if not path:
            return
        try:
            with open(path) as f:
                text = f.read()
        except OSError as e:
            self._say("PONI {}: {}".format(path, e))
            return
        self._setStart(path, text)

    def _setStart(self, source, text):
        try:
            poni = mar.parse_poni(text)
        except ValueError as e:
            self._say("Start PONI ({}): {}".format(source, e))
            return
        self.start = (source, text, poni)
        self.startLabel.setText(os.path.basename(source))
        self.startLabel.setToolTip(source)
        self.ringsOf.setCurrentText(START)
        self._table()
        self._drawRings()

    # -- calibrant -------------------------------------------------------
    def _calibrantChanged(self):
        name = self.calibrant.currentText()
        self.lattice.setEnabled(name == CUBIC)
        self.dPath.setEnabled(name == D_FILE)
        self.dBrowse.setEnabled(name == D_FILE)
        self._drawRings()

    def _browseD(self):
        path, _ = qt.QFileDialog.getOpenFileName(self, "Calibrant d spacings", dialog_dir(self.dPath.text()),
                                                 "d spacings (*.D *.d);;All files (*)")
        if path:
            self.dPath.setText(path)
            self._drawRings()

    def dspacings(self):
        """The chosen calibrant's d (Å), largest first; OSError or
        ValueError for a .D file that does not give them."""
        name = self.calibrant.currentText()
        if name in CALIBRANTS:
            return d_spacings(*CALIBRANTS[name])
        if name == CUBIC:
            return d_spacings(self.lattice.value(), "P")
        path = items.required(self.dPath.text().strip(), D_FILE, "choose a pyFAI .D file with Browse…")
        with open(path) as f:
            return read_d(f.read())

    # -- picks -----------------------------------------------------------
    def _plotSignal(self, event):
        if self.pick.isChecked() and event.get("event") == "mouseClicked" and event.get("button") == "left":
            self.addPick(event["x"], event["y"])

    def addPick(self, x, y):
        """The brightest pixel within SNAP of the plot point (x, y), as a
        peak of the ring in Ring #."""
        item = self.plot.getImage("frame")
        if item is None:
            return
        image = item.getData(copy=False)
        n1, n2 = image.shape
        r, c = math.floor(y), math.floor(x)
        if not (0 <= r < n1 and 0 <= c < n2):
            return
        r0, c0 = max(r - SNAP, 0), max(c - SNAP, 0)
        window = image[r0:r + SNAP + 1, c0:c + SNAP + 1]
        dr, dc = np.unravel_index(np.argmax(window), window.shape)
        self.picks.append((int(r0 + dr), int(c0 + dc), self.pickRing.value() - 1))
        self._drawPicks()

    def _clearPicks(self):
        self.picks = []
        self._drawPicks()

    def _drawPicks(self):
        if self.picks:
            rows, cols, _ = zip(*self.picks)
            # Pixel k's centre is at k + 0.5 on an image at origin 0, scale 1.
            self.plot.addCurve(np.add(cols, 0.5), np.add(rows, 0.5), legend="picks", color="#ff7f0e", symbol="x",
                               linestyle=" ", resetzoom=False, selectable=False)
            counts = collections.Counter(ring + 1 for _, _, ring in self.picks)
            self.pickCount.setText("; ".join("ring {}: {}".format(r, n) for r, n in sorted(counts.items())))
        else:
            self.plot.remove("picks", kind="curve")
            self.pickCount.setText("none")

    # -- refinement ------------------------------------------------------
    def refine(self):
        if self.frame is None:
            self._say("Load a calibrant frame first.")
            return
        if self.start is None:
            self._say("No start PONI: the integrator has not said its own; load one.")
            return
        try:
            ds = self.dspacings()
        except (OSError, ValueError) as e:
            self._say("Calibrant: {}".format(e))
            return
        run = _Run(self.frame[0], self.start[1])
        self._sent.append(run)
        self.current = run
        self.integrator.request(
            "calibrate", path=run.path, poni=run.start, calibrant="".join("{!r}\n".format(d) for d in ds),
            rings=self.rings.value(), delta_tth=self.delta.value(), factor=self.factor.value(),
            upper=float(self.upper.value() or NO_LIMIT), fixed=[n for n, box in self.fixed.items() if box.isChecked()],
            points=[list(p) for p in self.picks])
        self._say("Refining {}…".format(os.path.basename(run.path)))

    def _reply(self, method, body):
        if method == "config":
            self._config(body)
        elif method == "calibrate" and self._sent:
            run = self._sent.popleft()
            run.id = body.get("calibrating")
            early = self._early.pop(run.id, None)
            if run is self.current and early is not None:
                self._calibrated(early)

    def _published(self, topic, body):
        if topic == "config":
            self._config(body)
        elif topic == "calibration":
            if self.current is not None and self.current.id is not None and body.get("id") == self.current.id:
                self._calibrated(body)
            elif self._sent:
                self._early[body.get("id")] = body
                while len(self._early) > EARLY:
                    self._early.popitem(last=False)

    def _failed(self, method, message):
        if method == "calibrate" and self._sent:
            run = self._sent.popleft()
            if run is self.current:
                self.current = None
                self._say("Refused: {}".format(message))

    def _calibrated(self, body):
        run, self.current = self.current, None
        seconds = time.monotonic() - run.t0
        if "error" in body:
            self._say("Failed: {}".format(body["error"]))
            return
        try:
            poni = mar.parse_poni(body["poni"])
        except (KeyError, ValueError) as e:
            self._say("The refined PONI: {}".format(e))
            return
        points = body.get("points") or []
        self.result = {"path": run.path, "text": body["poni"], "poni": poni, "points": points,
                       "chi2": body.get("chi2")}
        counts = collections.Counter(int(p[2]) + 1 for p in points)
        self._say("{} peaks ({}), chi2 {:.3g} rad², {:.1f} s".format(
            len(points), "; ".join("ring {}: {}".format(r, n) for r, n in sorted(counts.items())),
            float(body.get("chi2") or math.nan), seconds))
        self.fromResult.setEnabled(True)
        self.saveButton.setEnabled(True)
        self._table()
        self._drawPeaks()
        self.ringsOf.setCurrentText(REFINED)
        self._drawRings()

    def _save(self):
        frame = self.result["path"]
        suggested = os.path.splitext(frame)[0] + ".poni"
        path, _ = qt.QFileDialog.getSaveFileName(self, "Refined PONI", suggested, "PONI (*.poni);;All files (*)")
        if not path:
            return
        try:
            with open(path, "w") as f:
                f.write(self.result["text"])
        except OSError as e:
            qt.QMessageBox.warning(self, "Calibration", "{}: {}".format(path, e))
            return
        self.applyRequested.emit(path)
        self._say("Saved {} and sent it to the integrator.".format(path))

    # -- drawing ---------------------------------------------------------
    def _table(self):
        start = self.start[2] if self.start else None
        refined = self.result["poni"] if self.result else None
        for i, (_, key, scale, unit) in enumerate(ROWS):
            digits = 4 if unit == "mm" else 5
            values = (start and start[key] * scale, refined and refined[key] * scale)
            change = None if None in values else values[1] - values[0]
            cells = ["" if v is None else "{:.{}f}".format(v, digits) for v in values]
            cells.append("" if change is None else "{:+.{}f}".format(change, digits))
            for j, text in enumerate(cells):
                item = qt.QTableWidgetItem(text)
                item.setTextAlignment(qt.Qt.AlignRight | qt.Qt.AlignVCenter)
                self.table.setItem(i, j, item)

    def _drawPeaks(self):
        r = self.result
        if r and self.frame and r["path"] == self.frame[0] and r["points"]:
            p = np.asarray(r["points"], dtype=float)
            self.plot.addCurve(p[:, 1] + 0.5, p[:, 0] + 0.5, legend="peaks", color="red", symbol="o",
                               linestyle=" ", resetzoom=False, selectable=False)
            self.plot.getCurve("peaks").setSymbolSize(4)
        else:
            self.plot.remove("peaks", kind="curve")

    def _drawRings(self):
        for item in list(self.plot.getItems()):
            if item.getName().startswith("ring "):
                self.plot.removeItem(item)
        which = self.ringsOf.currentText()
        poni = (self.start[2] if which == START and self.start else
                self.result["poni"] if which == REFINED and self.result else None)
        if self.frame is None or poni is None:
            return
        try:
            ds = self.dspacings()
        except (OSError, ValueError) as e:
            self._say("Calibrant: {}".format(e))
            return
        # The rings the integrator numbers: those the wavelength reaches.
        lam = poni["wavelength"] * 1e10
        qs = [2 * math.pi / d for d in ds if d > lam / 2][:self.rings.value()]
        _, shape, pixel = self.frame
        for n, (q, lines) in enumerate(mar.rings(poni, shape, pixel, qs).items(), 1):
            for k, (x, y) in enumerate(lines):
                self.plot.addCurve(x, y, legend="ring {} {}".format(n, k), color="white", linestyle="--",
                                   symbol="", resetzoom=False, selectable=False)
            if lines:
                x, y = max(lines, key=lambda line: len(line[0]))
                k = int(np.argmax(x))
                self.plot.addMarker(x[k], y[k], legend="ring {} label".format(n), text=str(n), color="white",
                                    symbol=",")

    def _say(self, text):
        self.state.setText(text)
