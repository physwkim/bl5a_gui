"""The bsrs document stream as the GUI reads it, with no Qt: the scans
being drawn, the alignment stages and history, the exposures and the
SPEC commands each plan run sent.

Documents of nested runs interleave (an alignment plan's run stays open
around each scan's run), so every event is routed by its descriptor's
`run_start`, never by the latest start document."""

import time
from dataclasses import dataclass, field

from . import peak

# The scan plans bl5a-qs opens a run for; a fly scan's events are SPEC's
# own points, each the counts of one sample, sent as SPEC takes them.
STEP_SCANS = ("ascan", "lup")
FLY_SCANS = ("cscan", "clup")
# The plans whose runs carry alignment stages.
ALIGNING = ("align", "insitu_step")


@dataclass
class Scan:
    uid: str
    plan: str
    motor: str
    det: str
    start: dict
    x: list = field(default_factory=list)
    y: list = field(default_factory=list)
    exit_status: str = ""
    reason: str = ""

    @property
    def fly(self):
        return self.plan in FLY_SCANS

    @property
    def title(self):
        a = self.start.get("plan_args") or {}
        if self.fly:
            return "{} {} {:g} {:g} {:g}".format(self.plan, self.motor, a.get("start", 0), a.get("finish", 0),
                                                 a.get("time", 0))
        # SPEC's count time: monitor counts as their negative.
        t = -a["monitor"] if "monitor" in a else a.get("time", 0)
        return "{} {} {:g} {:g} {} {:g}".format(self.plan, self.motor, a.get("start", 0), a.get("finish", 0),
                                                a.get("intervals", 0), t)

    def add(self, data):
        """One primary event: the motor position and the detector's counts."""
        x = data.get(self.motor)
        y = data.get(self.det)
        if x is None or y is None:
            return False
        self.x.append(float(x))
        self.y.append(float(y))
        return True

    def stats(self):
        return peak.stats(self.x, self.y)


@dataclass
class Run:
    uid: str
    plan: str
    series: str
    time: float
    start: dict
    stages: list = field(default_factory=list)
    final: dict = None
    exposures: list = field(default_factory=list)
    exit_status: str = ""
    reason: str = ""


def what(plan, sequence=None):
    """A plan as the GUI names it, with the alignment sequence it ran:
    "align: Thin film"."""
    return "{}: {}".format(plan, sequence) if sequence else plan


def pick(stage):
    """What a stage moved its motor to: "CEN", or "CEN dip" when the
    statistics are of the scan turned over."""
    return "{}{}".format(stage.get("pick") or "target", " dip" if stage.get("dip") else "")


def outcome(exit_status, reason=None):
    """An exit status with the reason the run or item ended so, if any."""
    return "{}: {}".format(exit_status, reason) if reason else exit_status


def history_row(run):
    """The alignment history's row for a finished run with stages: what the
    alignment left the sample at.

    `dth0` is the correction this run made to th's zero, the sum of
    (target − set) over the th stages that redefined th; SPEC's dial is not
    published, so the absolute offset is not known here. The positions
    are the "final" event's, which every alignment records."""
    if not run.stages and not run.final:
        return None
    last = {}
    for s in run.stages:
        last[s.get("motor")] = s.get("after")
    dth0 = None
    for s in run.stages:
        if s.get("motor") == "th" and s.get("set") is not None:
            dth0 = (dth0 or 0.0) + s["target"] - s["set"]
    final = run.final or {}
    return {
        "series": run.series,
        "plan": run.plan,
        "sequence": run.start.get("sequence"),
        "time": run.time,
        "T": run.start.get("T"),  # insitu_step's setpoint
        "sy": final.get("sy", last.get("sy")),
        "shold": final.get("shold", last.get("shold")),
        "dth0": dth0,
        "exit": run.exit_status,
        "reason": run.reason,
    }


class Documents:
    """Feed it every (name, doc); it calls the listeners it was given.

    on_scan_start(scan), on_scan_point(scan), on_scan_stop(scan),
    on_stage(run, stage, scan), on_exposure(run, exposure),
    on_command(run, command), on_run_stop(run, history_row or None),
    on_run_start(run)."""

    def __init__(self, **listeners):
        self.listeners = listeners
        self.runs = {}  # uid -> Run (plans) or Scan (scans), while open
        self.descriptors = {}  # uid -> (stream, run_start)
        # Each plan run's last finished scan, which its next stage reads.
        self.last_scan = {}

    def _emit(self, name, *args):
        cb = self.listeners.get(name)
        if cb is not None:
            cb(*args)

    def _parent(self):
        """The plan run a scan belongs to: the open plan run (bl5a-qs runs
        one plan at a time; its scans nest inside it)."""
        plans = [r for r in self.runs.values() if isinstance(r, Run)]
        return plans[-1] if plans else None

    def feed(self, name, doc):
        if name == "start":
            plan = doc.get("plan_name") or ""
            if plan in STEP_SCANS or plan in FLY_SCANS:
                motors = doc.get("motors") or [""]
                scan = Scan(doc["uid"], plan, motors[0], doc.get("det") or "det", doc)
                self.runs[scan.uid] = scan
                self._emit("on_scan_start", scan)
            else:
                run = Run(doc["uid"], plan, doc.get("series") or doc["uid"], doc.get("time") or time.time(), doc)
                self.runs[run.uid] = run
                self._emit("on_run_start", run)
        elif name == "descriptor":
            self.descriptors[doc["uid"]] = (doc.get("name"), doc.get("run_start"))
        elif name == "event":
            stream, run_uid = self.descriptors.get(doc.get("descriptor"), (None, None))
            run = self.runs.get(run_uid)
            data = doc.get("data") or {}
            if isinstance(run, Scan):
                if stream == "primary" and run.add(data):
                    self._emit("on_scan_point", run)
            elif isinstance(run, Run):
                if stream == "stages":
                    run.stages.append(data)
                    self._emit("on_stage", run, data, self.last_scan.get(run.uid))
                elif stream == "final":
                    run.final = data
                elif stream == "exposures":
                    run.exposures.append(data)
                    self._emit("on_exposure", run, data)
                elif stream == "commands":
                    self._emit("on_command", run, data)
        elif name == "stop":
            run = self.runs.pop(doc.get("run_start"), None)
            if isinstance(run, Scan):
                run.exit_status = doc.get("exit_status") or ""
                run.reason = doc.get("reason") or ""
                parent = self._parent()
                if parent is not None:
                    self.last_scan[parent.uid] = run
                self._emit("on_scan_stop", run)
            elif isinstance(run, Run):
                run.exit_status = doc.get("exit_status") or ""
                run.reason = doc.get("reason") or ""
                self.last_scan.pop(run.uid, None)
                self._emit("on_run_stop", run, history_row(run))
            self.descriptors = {k: v for k, v in self.descriptors.items() if v[1] != doc.get("run_start")}
