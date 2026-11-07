"""The centre tabs: the scan being drawn, the latest mar345 frame and the
integrated stacks."""

import math
import posixpath
import threading

import numpy as np
from silx.gui import qt
from silx.gui.colors import Colormap
from silx.gui.plot import Plot1D, Plot2D

from . import config, docs, mar, peak
from .vendor.Plot1DCustom import Plot1DCustom

MARKERS = (("cen", "CEN", "#d62728"), ("com", "COM", "#2ca02c"), ("xpk", "MAX", "#1f77b4"))


class ScanTab(qt.QWidget):
    """The scan running now, point by point, then its CEN/COM/MAX; the
    session's earlier scans stay in the list to look back at."""

    MAX_KEPT = 100

    def __init__(self, parent=None):
        super().__init__(parent)
        self.plot = Plot1DCustom(self)
        self.plot.setGraphGrid(True)
        self.scans = qt.QComboBox()
        self.scans.setSizeAdjustPolicy(qt.QComboBox.AdjustToContents)
        self.stats = qt.QLabel()
        self.stats.setTextInteractionFlags(qt.Qt.TextSelectableByMouse)
        # A failed scan's reason can be long: clipped here, whole in the tooltip.
        self.stats.setSizePolicy(qt.QSizePolicy.Ignored, qt.QSizePolicy.Preferred)
        top = qt.QHBoxLayout()
        top.addWidget(qt.QLabel("Scan"))
        top.addWidget(self.scans)
        top.addWidget(self.stats, 1)
        layout = qt.QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addLayout(top)
        layout.addWidget(self.plot)
        self.kept = []  # (scan, stage or None)
        self.live = None
        self.scans.activated.connect(self._show)
        # Points are drawn at most five times a second.
        self._dirty = False
        self._timer = qt.QTimer(self)
        self._timer.setInterval(200)
        self._timer.timeout.connect(self._redraw)
        self._timer.start()

    def start(self, scan):
        self.live = scan
        self.kept.append([scan, None])
        del self.kept[:-self.MAX_KEPT]
        self.scans.clear()
        self.scans.addItems([s.title for s, _ in self.kept])
        self.scans.setCurrentIndex(len(self.kept) - 1)
        self._say("running")
        self._draw(scan, None)

    def point(self, scan):
        if scan is self.live:
            self._dirty = True

    def stop(self, scan):
        if scan is self.live:
            self.live = None
            self._dirty = False
            self._draw(scan, None)

    def stage(self, scan, stage):
        """The stage that read `scan`: its target goes on the plot."""
        for entry in self.kept:
            if entry[0] is scan:
                entry[1] = stage
        if self.scans.currentIndex() == len(self.kept) - 1 and self.kept and self.kept[-1][0] is scan:
            self._draw(scan, stage)

    def _redraw(self):
        if self._dirty and self.live is not None and self._current() is self.live:
            self._dirty = False
            self._draw(self.live, None)

    def _current(self):
        k = self.scans.currentIndex()
        return self.kept[k][0] if 0 <= k < len(self.kept) else None

    def _show(self, k):
        if 0 <= k < len(self.kept):
            self._draw(*self.kept[k])

    def _draw(self, scan, stage):
        plot = self.plot
        plot.clearMarkers()
        plot.setGraphTitle(scan.title)
        plot.getXAxis().setLabel(scan.motor)
        plot.getYAxis().setLabel(scan.det)
        if scan.x:
            plot.addCurve(scan.x, scan.y, legend="scan", resetzoom=True, symbol="o")
        else:
            plot.remove("scan", kind="curve")
        if scan is self.live:
            return
        if stage is None:
            st = scan.stats()
        else:
            # The statistics the stage moved by, of its outlier-filtered points.
            st = peak.Stats(*(math.nan if stage.get(k) is None else float(stage[k])
                              for k in ("xmax", "ymax", "com", "cen", "fwhm")))
        if st is None:
            self._say("no points" + (" ({})".format(docs.outcome(scan.exit_status, scan.reason))
                                     if scan.exit_status else ""))
            return
        for key, label, color in MARKERS:
            v = getattr(st, key)
            if math.isfinite(v):
                plot.addXMarker(v, legend=label, text=label, color=color)
        text = "CEN {:.5g}  FWHM {:.4g}  COM {:.5g}  MAX {:.5g} at {:.5g}".format(st.cen, st.fwhm, st.com, st.ymax,
                                                                                   st.xpk)
        if stage is not None and stage.get("target") is not None:
            plot.addXMarker(stage["target"], legend="target", text=str(stage.get("pick") or "target"),
                            color="black")
            text += "  → {} {:.5g}".format(stage.get("pick"), stage["target"])
        if stage is not None and stage.get("rejected"):
            text += "  ({} outlier{} replaced)".format(stage["rejected"], "" if stage["rejected"] == 1 else "s")
        if scan.exit_status and scan.exit_status != "success":
            text += "  ({})".format(docs.outcome(scan.exit_status, scan.reason))
        self._say(text)

    def _say(self, text):
        self.stats.setText(text)
        self.stats.setToolTip(text)


class _Loader(qt.QObject):
    """Decodes one mar345 file at a time off the GUI thread; a file asked
    for while one is decoding replaces any still waiting."""

    loaded = qt.Signal(str, object, float)
    failed = qt.Signal(str, str)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._cond = threading.Condition()
        self._next = None
        threading.Thread(target=self._run, name="bl5a-gui-mar", daemon=True).start()

    def load(self, path):
        with self._cond:
            self._next = path
            self._cond.notify()

    def _run(self):
        while True:
            with self._cond:
                while self._next is None:
                    self._cond.wait()
                path, self._next = self._next, None
            try:
                image, pixel = mar.read(path)
            except Exception as e:  # a missing or torn file is the operator's to read
                self.failed.emit(path, str(e))
            else:
                self.loaded.emit(path, image, pixel)


class ImageTab(qt.QWidget):
    """The latest frame on a log scale, with the rings of the q values in
    the box drawn where the integrator's geometry puts them."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.plot = Plot2D(self)
        self.plot.setKeepDataAspectRatio(True)
        self.colormap = Colormap("viridis", normalization=Colormap.LOGARITHM)
        self.plot.setDefaultColormap(self.colormap)
        self.follow = qt.QCheckBox("Follow new frames")
        self.follow.setChecked(True)
        self.ringsBox = qt.QCheckBox("Rings q [Å⁻¹]")
        self.ringsBox.setChecked(True)
        self.ringQ = qt.QLineEdit(", ".join("{:g}".format(q) for q in config.RINGS))
        self.ringQ.setToolTip("Comma separated; default Al 111 and Al 200")
        self.open = qt.QPushButton("Open…")
        self.info = qt.QLabel()
        self.info.setTextInteractionFlags(qt.Qt.TextSelectableByMouse)
        top = qt.QHBoxLayout()
        for w in (self.follow, self.ringsBox, self.ringQ, self.open):
            top.addWidget(w)
        top.addWidget(self.info, 1)
        layout = qt.QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addLayout(top)
        layout.addWidget(self.plot)
        self.poni = None
        self.image = None  # (path, shape, pixel)
        self._rings = {}  # (shape, pixel, q) -> polylines
        self.loader = _Loader(self)
        self.loader.loaded.connect(self._loaded)
        self.loader.failed.connect(lambda p, e: self.info.setText("{}: {}".format(p, e)))
        self.ringsBox.toggled.connect(self._drawRings)
        self.ringQ.editingFinished.connect(self._drawRings)
        self.open.clicked.connect(self._open)
        try:
            with open(config.PONI) as f:
                self.setPoni(f.read())
        except OSError:
            pass

    def setPoni(self, text):
        try:
            self.poni = mar.parse_poni(text)
        except ValueError as e:
            self.info.setText("PONI: {}".format(e))
            return
        self._rings.clear()
        self._drawRings()

    def exposure(self, path):
        if self.follow.isChecked() and path:
            self.load(path)

    def load(self, path):
        self.info.setText("loading {}".format(path))
        self.loader.load(path)

    def _open(self):
        path, _ = qt.QFileDialog.getOpenFileName(self, "mar345 frame", "", "mar345 (*.mar*);;All files (*)")
        if path:
            self.load(path)

    def _loaded(self, path, image, pixel):
        self.image = (path, image.shape, pixel)
        # A log scale needs positive values; counts of 0 are drawn as 1.
        self.plot.addImage(np.maximum(image, 1), legend="frame", resetzoom=self.plot.getImage("frame") is None)
        self.plot.setGraphTitle(path.rsplit("/", 1)[-1])
        self.info.setText("{} × {}, {:g} µm, max {}".format(image.shape[0], image.shape[1], pixel * 1e6,
                                                           int(image.max())))
        self._drawRings()

    def _drawRings(self):
        for item in list(self.plot.getItems()):
            if item.getName().startswith("ring "):
                self.plot.removeItem(item)
        if not self.ringsBox.isChecked() or self.image is None or self.poni is None:
            return
        try:
            qs = [float(v) for v in self.ringQ.text().replace(",", " ").split()]
        except ValueError:
            self.info.setText("Rings: not numbers: {!r}".format(self.ringQ.text()))
            return
        _, shape, pixel = self.image
        todo = [q for q in qs if (shape, pixel, q) not in self._rings]
        if todo:
            for q, lines in mar.rings(self.poni, shape, pixel, todo).items():
                self._rings[(shape, pixel, q)] = lines
        for q in qs:
            for k, (x, y) in enumerate(self._rings[(shape, pixel, q)]):
                self.plot.addCurve(x, y, legend="ring {:g} {}".format(q, k), color="white", linestyle="--",
                                   symbol="", resetzoom=False, selectable=False)


class StackTab(qt.QWidget):
    """A stack's integrated rows: q against frame or time as a heatmap,
    and the rows themselves overlaid or as a waterfall. A stack is one
    series on one q axis in one data folder, named by its file's path."""

    def __init__(self, integrator, parent=None):
        super().__init__(parent)
        self.integrator = integrator
        self.data = {}  # path -> {"q": array, "unit": str, "frames": [...], "rows": [array]}
        self.latest = None  # the stack of the last row published
        self.series = qt.QComboBox()
        self.series.setSizeAdjustPolicy(qt.QComboBox.AdjustToContents)
        self.refresh = qt.QPushButton("Refresh")
        self.yaxis = qt.QComboBox()
        self.yaxis.addItems(["frame #", "time [min]"])
        self.th = qt.QComboBox()
        self.th.addItem("all th")
        self.mode = qt.QComboBox()
        self.mode.addItems(["overlay", "waterfall"])
        self.last = qt.QSpinBox()
        self.last.setRange(1, 1000)
        self.last.setValue(20)
        self.last.setPrefix("last ")
        self.offset = qt.QDoubleSpinBox()
        self.offset.setRange(0.0, 10.0)
        self.offset.setSingleStep(0.1)
        self.offset.setValue(0.3)
        self.offset.setPrefix("offset ")
        self.offset.setToolTip("Waterfall step, as a fraction of the rows' median maximum")
        self.logI = qt.QCheckBox("log I")
        self.logI.setChecked(True)
        top = qt.QHBoxLayout()
        for w in (qt.QLabel("Stack"), self.series, self.refresh, qt.QLabel("y"), self.yaxis, self.th,
                  self.logI, self.mode, self.last, self.offset):
            top.addWidget(w)
        top.addStretch()
        self.heat = Plot2D(self)
        self.heat.setKeepDataAspectRatio(False)
        self.heat.getXAxis().setLabel("q [Å⁻¹]")
        # Rows are I/icxrd, well below 1: silx's fixed-point log ticks
        # would all read "0".
        self.heat.getColorBarWidget().getColorScaleBar().getTickBar().setDisplayType("e")
        self.colormap = Colormap("viridis", normalization=Colormap.LOGARITHM)
        self.profiles = Plot1D(self)
        self.profiles.getXAxis().setLabel("q [Å⁻¹]")
        self.profiles.getYAxis().setLabel("I / icxrd")
        self.info = qt.QLabel("integrator: not answering")
        self.info.setWordWrap(True)
        self.info.setTextInteractionFlags(qt.Qt.TextSelectableByMouse)
        split = qt.QSplitter(qt.Qt.Vertical)
        split.addWidget(self.heat)
        split.addWidget(self.profiles)
        layout = qt.QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addLayout(top)
        layout.addWidget(split, 1)
        layout.addWidget(self.info)
        self.refresh.clicked.connect(self.reload)
        self.series.activated.connect(lambda _: self._select())
        for w in (self.yaxis, self.th, self.mode):
            w.activated.connect(lambda _: self.draw())
        for w in (self.last, self.offset):
            w.valueChanged.connect(lambda _: self.draw())
        self.logI.toggled.connect(lambda _: self.draw())
        integrator.reply.connect(self._reply)
        integrator.published.connect(self._published)

    def reload(self):
        self.integrator.request("config")
        self.integrator.request("stacks")

    def current(self):
        """The shown stack's path."""
        return self.series.currentData()

    def _select(self):
        path = self.current()
        if path:
            self.data.pop(path, None)
            self.integrator.request("history", path=path)

    def _reply(self, method, body):
        if method == "stacks":
            stacks = body.get("stacks") or []
            keep = self.current()
            self.series.clear()
            for s in stacks:
                path = s.get("path")
                self.series.addItem("{} — {}".format(s.get("series"), posixpath.join(
                    posixpath.basename(posixpath.dirname(path)), posixpath.basename(path))), path)
            paths = [s.get("path") for s in stacks]
            for want in (keep, self.latest):
                if want in paths:
                    self.series.setCurrentIndex(paths.index(want))
                    break
            else:
                if paths:
                    self.series.setCurrentIndex(len(paths) - 1)
            self._select()
        elif method == "history":
            path = body.get("path")
            entry = self.data.setdefault(path, {"q": np.asarray(body["q"], float), "unit": body.get("unit"),
                                                "frames": [], "rows": []})
            start = body.get("from", 0)
            if start != len(entry["rows"]):
                # A row arrived while an earlier reply was on its way; ask
                # again from where this copy ends.
                self.integrator.request("history", path=path, **{"from": len(entry["rows"])})
                return
            entry["frames"].extend(body.get("frames") or [])
            entry["rows"].extend(np.asarray(r, dtype=float) for r in body.get("intensity") or [])
            if path == self.current():
                self._ths(entry)
                self.draw()

    def _published(self, topic, body):
        if topic != "row":
            return
        path = body.get("path")
        self.latest = path
        if self.series.findData(path) < 0:
            # A new stack: list it. The tab stays on the stack shown, and
            # shows this one when none was.
            self.integrator.request("stacks")
            return
        entry = self.data.get(path)
        if entry is not None:
            self.integrator.request("history", path=path, **{"from": len(entry["rows"])})

    def _ths(self, entry):
        keep = self.th.currentText()
        ths = sorted({round(f.get("th", 0.0), 4) for f in entry["frames"]})
        self.th.clear()
        self.th.addItem("all th")
        self.th.addItems(["th {:g}".format(t) for t in ths])
        if self.th.findText(keep) >= 0:
            self.th.setCurrentText(keep)

    def _rows(self, entry):
        """(original indices, frames, rows) under the th filter."""
        pick = self.th.currentText()
        want = None if pick == "all th" or not pick else float(pick.split()[1])
        idx = [k for k, f in enumerate(entry["frames"]) if want is None or round(f.get("th", 0.0), 4) == want]
        return idx, [entry["frames"][k] for k in idx], [entry["rows"][k] for k in idx]

    def draw(self):
        path = self.current()
        entry = self.data.get(path)
        self.heat.clear()
        self.profiles.clear()
        if not entry:
            return
        q = entry["q"]
        self.info.setText("{}: {} rows; q {:.4g} to {:.4g} {} in {} bins".format(
            path, len(entry["rows"]), q[0], q[-1], entry["unit"], len(q)))
        if not entry["rows"]:
            return
        dq = q[1] - q[0] if len(q) > 1 else 1.0
        idx, frames, rows = self._rows(entry)
        if not rows:
            return
        if self.yaxis.currentIndex() == 0:
            edges = [(k - 0.5, k + 0.5) for k in idx]
            self.heat.getYAxis().setLabel("frame #")
        else:
            t0 = entry["frames"][0]["time"]
            t = [(f["time"] - t0) / 60.0 for f in frames]
            step = float(np.median(np.diff(t))) if len(t) > 1 else 1.0
            ends = t[1:] + [t[-1] + (step if step > 0 else 1.0)]
            edges = list(zip(t, ends))
            self.heat.getYAxis().setLabel("time [min] from the first frame")
        stack = np.vstack(rows)
        finite = stack[np.isfinite(stack)]
        positive = finite[finite > 0]
        log = self.logI.isChecked() and positive.size > 0
        if finite.size:
            lo = float(np.percentile(positive if log else finite, 1))
            hi = float(np.percentile(positive if log else finite, 99.9))
            self.colormap.setNormalization(Colormap.LOGARITHM if log else Colormap.LINEAR)
            self.colormap.setVRange(lo, max(hi, lo * 1.0001) if log else max(hi, lo + 1e-30))
        for k, ((y0, y1), row) in enumerate(zip(edges, rows)):
            if y1 <= y0:
                continue
            self.heat.addImage(row[np.newaxis, :], origin=(q[0] - dq / 2, y0), scale=(dq, y1 - y0),
                               legend="row {}".format(k), colormap=self.colormap, resetzoom=False)
        self.heat.resetZoom()
        tail = list(zip(idx, frames, rows))[-self.last.value():]
        peaks = [np.nanmax(r) for _, _, r in tail if np.isfinite(r).any()]
        step = self.offset.value() * float(np.median(peaks)) if peaks and self.mode.currentIndex() else 0.0
        for n, (k, f, row) in enumerate(tail):
            self.profiles.addCurve(q, row + n * step, legend="#{} th {:g}".format(k, f.get("th", 0.0)),
                                   resetzoom=False)
        self.profiles.setGraphTitle("{}: {} of {} rows".format(self.series.currentText(), len(tail),
                                                               len(entry["rows"])))
        self.profiles.resetZoom()
