"""The BL5A GUI's controls as MCP tools, over stdio, for Claude Code or
Codex. Items are built by the forms' own builders (items.py), every
action passes the GUI's gate (gate.py), and requests go through the
vendored bsrs client, so an AI and the GUI queue the same items under the
same rules. The only PVs written are the ones the GUI writes: a motor's
TWV/TWF/TWR and SPEC's Cmd and Cmd:Abort."""

import argparse
import functools
import io
import itertools
import json
import logging
import math
import sys
import threading
import time
from collections import deque
from contextlib import asynccontextmanager
from dataclasses import dataclass, field
from typing import Annotated, Literal

import epics
import numpy as np
import zmq
from mcp.server.mcpserver import Image, MCPServer
from mcp.server.mcpserver.exceptions import ToolError
from mcp_types import ToolAnnotations
from pydantic import BaseModel, ConfigDict, Field

from . import config, docs, gate, items, mar, peak
from .speclog import LogLines
from .vendor.bsrs_client import DocumentRelay, QsClient, QsError, RunEngineProxy

logger = logging.getLogger("bl5a-mcp")

INSTRUCTIONS = """BL5A beamline (PLS-II 5A): the BL5A GUI's controls. bl5a-qs runs queued
plans one at a time; SPEC moves the motors and takes the mar345 frames;
bl5a-integrator integrates each frame into its series' stack.
- Start with `status`: what runs, and `allowed`, the actions open now.
- Plans run only from the queue: queue_alignment, queue_mar_series or
  queue_insitu, then queue_start.
- tweak and spec_command need SPEC free (status.allowed.spec): qs idle with
  nothing running, SPEC connected and idle.
- messages, spec_log and commands are logs: pass `since` = the `last` the
  previous read returned.
- Ask the operator before abort and queue_clear, and before queueing
  frames: the shutter opens for them."""

# The qs status fields `status` reports.
QS_KEYS = ("manager_state", "re_state", "worker_environment_exists", "items_in_queue", "items_in_history",
           "running_item_uid", "running_item_name", "queue_stop_pending", "pause_pending",
           "queue_autostart_enabled", "plans_run", "plans_failed")
SCANS = 50  # finished and running scans kept
ALIGNED = 200  # alignment history rows kept
TWEAK_TIMEOUT = 300.0  # s, for one TWF/TWR move to complete
FRAME_PX = 1024  # frame_image's longest side


def sig(v, digits=6):
    """`v` to `digits` significant digits; not for times."""
    return float("{:.{}g}".format(v, digits)) if v is not None and math.isfinite(v) else None


def rounded(row):
    """A document row with its floats to 6 significant digits, but its
    time (s since the epoch)."""
    return {k: sig(v) if isinstance(v, float) and k != "time" else v for k, v in row.items()}


def clean(obj):
    """`obj` as JSON can carry it: NaN and infinities as null, numpy as
    Python."""
    if isinstance(obj, dict):
        return {str(k): clean(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple, deque)):
        return [clean(v) for v in obj]
    if isinstance(obj, np.ndarray):
        return clean(obj.tolist())
    if isinstance(obj, np.generic):
        obj = obj.item()
    if isinstance(obj, float) and not math.isfinite(obj):
        return None
    return obj


def out(obj):
    return json.dumps(clean(obj), ensure_ascii=False, separators=(",", ":"))


def binned(q, rows, points):
    """q and each row averaged over equal runs of bins, to at most `points`
    bins; NaN bins drop out of a run's mean."""
    k = max(1, math.ceil(len(q) / points))
    n = len(q) // k * k
    if n == 0:
        return np.asarray(q, float), [np.asarray(r, float) for r in rows]
    q = np.asarray(q, float)[:n].reshape(-1, k).mean(axis=1)
    out_rows = []
    for r in rows:
        r = np.asarray(r, float)[:n].reshape(-1, k)
        with np.errstate(invalid="ignore"):
            good = np.isfinite(r)
            total = np.where(good, r, 0.0).sum(axis=1)
            count = good.sum(axis=1)
            out_rows.append(np.where(count > 0, total / np.maximum(count, 1), np.nan))
    return q, out_rows


class Log:
    """Numbered lines, the newest `keep`; a reader passes the last number
    it read and gets what came after."""

    def __init__(self, keep=2000):
        self._lines = deque(maxlen=keep)
        self._n = 0
        self._lock = threading.Lock()

    @property
    def last(self):
        return self._n

    def add(self, text):
        with self._lock:
            self._n += 1
            self._lines.append((self._n, time.strftime("%H:%M:%S"), str(text)))

    def since(self, n, limit):
        """The lines after line `n`, at most the newest `limit`; `skipped`
        counts the newer lines not returned. An `n` past the last line is
        from before a server restart and reads from the start."""
        with self._lock:
            last = self._n
            if n > last:
                n = 0
            fresh = [entry for entry in self._lines if entry[0] > n]
        shown = fresh[-limit:] if limit > 0 else []
        return {"last": last, "skipped": (last - n) - len(shown),
                "lines": ["{} {}".format(t, text) for _, t, text in shown]}


@dataclass
class Command:
    """A SPEC command sent through Cmd; `done` is set by the put's
    completion callback, `result` by the settle that read its reply.
    `log_from` is the SPEC log's last line when it was sent: what the
    command prints goes to the log, not to Cmd:Reply."""
    cmd: str
    sent: float
    log_from: int
    done: threading.Event = field(default_factory=threading.Event)
    finished: float = None
    result: dict = None


def gui_data_folder():
    """The GUI's Data folder setting, as last saved; "" when unset."""
    from silx.gui import qt  # only here: Qt is not needed otherwise

    s = qt.QSettings(*config.SETTINGS)
    s.sync()
    return str(s.value("data_root", "") or "")


def _item(item):
    plan, name, params, folder, user = items.describe(item)
    return {"uid": item.get("item_uid"), "plan": plan, "name": name, "params": params, "folder": folder,
            "user": user}


class Beamline:
    """What the GUI holds, without the widgets: qs's status, queue and
    history, the document stream's scans and stages, the logs, the PVs and
    the SPEC command in flight.

    `lock` guards that state and is never held across network I/O;
    `re_lock` makes each qs request sequence one unit (a multi-item add
    lands in one piece); `spec_lock` owns `spec_job`, the one thing SPEC is
    doing for this server: None, "tweak <motor>" or a Command."""

    def __init__(self, qs_control, qs_documents, integrator_rep, integrator_pub, user):
        self.integrator_rep = integrator_rep
        self.integrator_pub = integrator_pub
        self.lock = threading.RLock()
        self.qs = None
        self.qs_error = "not read yet"
        self.queue = {"items": [], "running_item": {}}
        self.history = []
        self.finished = items.Finished()
        self.after_frame = gate.StopAfterFrame()
        self.spec = dict.fromkeys(gate.SPEC_STATE)
        self.values = {}  # PV name -> (value, text) while connected
        self.scans = deque(maxlen=SCANS)  # (id, docs.Scan)
        self._scan_ids = itertools.count(1)
        self.stages = {"caption": "Stages", "rows": []}
        self.aligned = deque(maxlen=ALIGNED)
        self.last_frame = None
        self.messages, self.commands, self.spec_log = Log(), Log(), Log()
        self._log_lines = LogLines()
        self.spec_lock = threading.Lock()
        self.spec_job = None
        self.last_command = None
        self.re_lock = threading.Lock()
        self.relay = DocumentRelay(qs_documents)
        self.RE = RunEngineProxy(QsClient(qs_control, 10000), self.relay, user=user, during_wait=time.sleep)
        self.docs = docs.Documents(
            on_scan_start=self._scan_start, on_stage=self._stage, on_exposure=self._exposure,
            on_command=self._command, on_run_start=self._run_start, on_run_stop=self._run_stop)
        self.pvs = {}
        self._quit = threading.Event()
        # sync()'s: one REQ socket, and what it read last.
        self._poll_lock = threading.Lock()
        self._poll_client = QsClient(qs_control, 2000)
        self._seen = self._seen_history = None
        self._reachable = True
        self._zmq = zmq.Context.instance()
        self._int_lock = threading.Lock()
        self._int_sock = None
        self._threads = []

    # -- life ------------------------------------------------------------
    def start(self):
        """PVs, the document relay and the polling threads. Called once the
        stdio server serves: libca and pyepics write to stdout, which the
        server has diverted from the wire by then."""
        watched = [config.SPEC + s for s in gate.SPEC_STATE + ("Log",)]
        watched += [config.RING_PV, config.SHUTTER_PV, config.TEMP_SP_PV, config.TEMP_RBV_PV,
                    config.TEMP_RAMP_PV]
        watched += [config.MAR + s for s in ("DetectorState_RBV", "FilePath_RBV", "FilePathExists_RBV")]
        for m in config.MOTORS:
            watched += [config.SPEC + m + "." + f for f in ("RBV", "DMOV", "TWV", "PREC", "EGU")]
        for name in watched:
            if name:
                self.pvs[name] = epics.PV(name, auto_monitor=True, form="ctrl", callback=self._changed,
                                          connection_callback=self._connected)
        puts = [config.SPEC_CMD] + [config.SPEC + s for s in ("Cmd:Status", "Cmd:Reply", "Cmd:Abort")]
        puts += [config.SPEC + m + f for m in config.MOTORS for f in (".TWF", ".TWR")]
        for name in puts:
            self.pvs[name] = epics.PV(name, auto_monitor=False)
        self.relay.subscribe(self._document)
        self.relay.start()
        for target, name in ((self._poll, "bl5a-mcp-qs"), (self._integrator_pub, "bl5a-mcp-integrator")):
            t = threading.Thread(target=target, name=name, daemon=True)
            t.start()
            self._threads.append(t)

    def stop(self):
        self._quit.set()
        self.relay.stop()
        for t in self._threads:
            t.join(3.0)
        with self._poll_lock:
            self._poll_client.close()
        with self._int_lock:
            if self._int_sock is not None:
                self._int_sock.close(linger=0)
                self._int_sock = None

    # -- qs ----------------------------------------------------------------
    def _poll(self):
        while not self._quit.is_set():
            self.sync()
            self._quit.wait(0.5)

    def sync(self):
        """bridge.QsPoller's step: qs's status, its queue when that or the
        running item changed, its history when it changed. The poller runs
        it twice a second, and every tool that reads or gates on qs runs it
        first, so a tool sees what the one before it did."""
        with self._poll_lock:
            if self._quit.is_set():
                return
            queue = history = None
            try:
                st = self._poll_client.ok("status")
                if (st.get("plan_queue_uid"), st.get("running_item_uid")) != self._seen:
                    q = self._poll_client.ok("queue_get")
                    self._seen = (st.get("plan_queue_uid"), st.get("running_item_uid"))
                    queue = {"items": q.get("items") or [], "running_item": q.get("running_item") or {}}
                if st.get("plan_history_uid") != self._seen_history:
                    h = self._poll_client.ok("history_get")
                    self._seen_history = h.get("plan_history_uid")
                    history = h.get("items") or []
            except QsError as e:
                if self._reachable:
                    self.messages.add("qs: {}".format(e))
                self._reachable = False
                self._seen = self._seen_history = None
                with self.lock:
                    self.qs, self.qs_error = None, str(e)
                    self.after_frame.status(None)
                return
            self._reachable = True
            fresh = []
            with self.lock:
                self.qs = st
                due = self.after_frame.status(st)
                if queue is not None:
                    self.queue = queue
                if history is not None:
                    self.history = history
                    # The history, not the stop document, says how each
                    # item ended: one that failed before its plan opened a
                    # run has no stop.
                    fresh = self.finished.new(history)
            for item in fresh:
                self.messages.add(items.ended(item))
            if due:
                threading.Thread(target=self._background, args=("stop after frame: stop", self.RE.stop),
                                 daemon=True).start()

    def _background(self, label, fn):
        try:
            with self.re_lock:
                fn()
        except Exception as e:
            self.messages.add("{}: {}".format(label, e))
        else:
            self.messages.add("{}: done".format(label))

    def require(self, action):
        """Raise unless the GUI's gate opens `action` now."""
        self.sync()
        self.check(action)

    def check(self, action):
        """Raise unless the gate opens `action` on the state read last."""
        with self.lock:
            if gate.allowed(self.qs, self.spec, self.after_frame.asked)[action]:
                return
            st = self.qs
            if st is None:
                qs = "qs not answering: {}".format(self.qs_error)
            else:
                qs = "qs {}".format(st.get("re_state"))
                if st.get("running_item_uid"):
                    qs += ", running {}".format(st.get("running_item_name"))
            spec = ", ".join("{} {}".format(k, v or "—") for k, v in self.spec.items())
            asked = ", stop after frame already asked" if self.after_frame.asked else ""
        raise ToolError("{}: not open now ({}{}; SPEC {})".format(action, qs, asked, spec))

    def re(self, fn, *args, **kwargs):
        with self.re_lock:
            return fn(*args, **kwargs)

    def add(self, queued, data_folder):
        """Queue the items in order as one unit, each with the Data folder:
        `data_folder`, or the GUI's setting when None."""
        root = gui_data_folder() if data_folder is None else data_folder
        queued = [items.placed(item, root) for item in queued]
        done = []
        with self.re_lock:
            for item in queued:
                try:
                    done.append(self.RE.queue_item_add(item))
                except QsError as e:
                    raise ToolError("queued {} of {} ({}), then: {}".format(
                        len(done), len(queued), ", ".join(i.get("item_uid") for i in done) or "none", e))
        self.sync()
        with self.lock:
            st = self.qs or {}
        return {"queued": [_item(i) for i in done], "re_state": st.get("re_state"),
                "queue_autostart_enabled": st.get("queue_autostart_enabled")}

    # -- documents -------------------------------------------------------
    def _document(self, name, doc):
        try:
            with self.lock:
                self.docs.feed(name, doc)
        except Exception as e:  # one bad document must not stop the stream
            logger.exception("document %s", name)
            self.messages.add("document {}: {}".format(name, e))

    def _scan_start(self, scan):
        self.scans.append((next(self._scan_ids), scan))

    def scan_id(self, scan):
        return next((k for k, s in self.scans if s is scan), None)

    def _stage(self, run, stage, scan):
        self.stages["rows"].append(dict(stage, scan=self.scan_id(scan) if scan is not None else None))

    def _exposure(self, run, exposure):
        self.last_frame = exposure.get("file")
        self.messages.add("{}: frame {} ({})".format(run.plan, exposure.get("file"), run.series))

    def _command(self, run, command):
        self.commands.add("[{}] {}".format(run.plan, command.get("cmd")))
        reply = str(command.get("reply") or "").rstrip("\n")
        if reply:
            self.commands.add("    " + reply.replace("\n", "\n    "))

    def _run_start(self, run):
        self.messages.add("{} started ({})".format(run.plan, run.series))
        if run.plan in docs.ALIGNING:
            self.stages = {"caption": "Stages — {} ({})".format(run.plan, run.series), "rows": []}

    def _run_stop(self, run, row):
        if row is not None:
            self.aligned.append(row)

    # -- PVs -------------------------------------------------------------
    # CA callbacks only store; every CA call is made on a tool's thread.
    def _changed(self, pvname=None, value=None, char_value=None, **kw):
        text = "" if char_value is None else str(char_value)
        if pvname == config.SPEC + "Log":
            for line in self._log_lines.new(text):
                self.spec_log.add(line)
            return
        with self.lock:
            self.values[pvname] = (value, text)
            suffix = pvname[len(config.SPEC):] if pvname.startswith(config.SPEC) else None
            if suffix in self.spec:
                self.spec[suffix] = text

    def _connected(self, pvname=None, conn=None, **kw):
        if conn:
            return
        with self.lock:
            self.values.pop(pvname, None)
            suffix = pvname[len(config.SPEC):] if pvname.startswith(config.SPEC) else None
            if suffix in self.spec:
                self.spec[suffix] = None

    def value(self, name):
        with self.lock:
            return self.values.get(name, (None, None))

    def text(self, name, timeout=2.0):
        """A fresh read, not the monitor's."""
        epics.ca.use_initial_context()
        v = self.pvs[name].get(as_string=True, timeout=timeout, use_monitor=False)
        return "" if v is None else str(v)

    def put(self, name, value, timeout):
        """A put that waits for completion; raises unless it completed."""
        epics.ca.use_initial_context()
        r = self.pvs[name].put(value, wait=True, timeout=timeout)
        if r is None:
            raise ToolError("{}: not connected".format(name))
        if r != 1:
            raise ToolError("{}: put not completed in {:g} s".format(name, timeout))

    # -- SPEC --------------------------------------------------------------
    def _settle(self):
        """Under spec_lock: a finished command's reply read and the job
        freed. The job stays claimed while Cmd:Status and Cmd:Reply are
        read, so no next command can overwrite them first."""
        c = self.spec_job
        if not isinstance(c, Command) or not c.done.is_set():
            return
        status = self.text(config.SPEC + "Cmd:Status")
        reply = self.text(config.SPEC + "Cmd:Reply")
        c.result = {"cmd": c.cmd, "status": status or "no status", "reply": reply.rstrip("\n"),
                    "seconds": round((c.finished or time.time()) - c.sent, 3), "spec_log_since": c.log_from}
        self.spec_job = None
        self.last_command = c

    def _claim(self, job):
        """Under spec_lock: make `job` SPEC's, if SPEC is free."""
        self._settle()
        if isinstance(self.spec_job, Command):
            raise ToolError("SPEC is running {!r}, sent {:.0f} s ago: spec_result waits for it, spec_abort "
                            "interrupts it".format(self.spec_job.cmd, time.time() - self.spec_job.sent))
        if self.spec_job is not None:
            raise ToolError("SPEC is busy: {}".format(self.spec_job))
        self.require("spec")
        self.spec_job = job

    def spec_command(self, cmd, wait_s):
        cmd = cmd.strip()
        if not cmd:
            raise ToolError("an empty command")
        epics.ca.use_initial_context()
        with self.spec_lock:
            c = Command(cmd, time.time(), self.spec_log.last)
            self._claim(c)

            def completed(**kw):
                c.finished = time.time()
                c.done.set()

            try:
                r = self.pvs[config.SPEC_CMD].put(cmd, wait=False, use_complete=True, callback=completed)
            except Exception:
                self.spec_job = None
                raise
            if r is None:
                self.spec_job = None
                raise ToolError(config.SPEC_CMD + ": not connected")
        c.done.wait(wait_s)
        return self.command_result(c)

    def command_result(self, c=None):
        """`c`'s reply (the last command's when None), or that it still
        runs."""
        with self.spec_lock:
            self._settle()
            c = c or (self.spec_job if isinstance(self.spec_job, Command) else self.last_command)
        if c is None:
            return {"cmd": None, "note": "no command sent from this server yet"}
        if c.result is None:
            return {"cmd": c.cmd, "running_s": round(time.time() - c.sent, 1), "spec_log_since": c.log_from}
        return c.result

    def spec_abort(self):
        epics.ca.use_initial_context()
        with self.spec_lock:
            self._settle()
            c = self.spec_job
            if not isinstance(c, Command):
                raise ToolError("no command sent from this server is running; spec_abort interrupts only "
                                "those, as the GUI's ^C does")
            r = self.pvs[config.SPEC + "Cmd:Abort"].put(1, wait=False)
            if r is None:
                raise ToolError(config.SPEC + "Cmd:Abort: not connected")
        return {"aborting": c.cmd}

    def tweak(self, motor, step, direction):
        if motor not in config.MOTORS:
            raise ToolError("motor: one of {}, got {!r}".format(", ".join(config.MOTORS), motor))
        if not (math.isfinite(step) and step > 0):
            raise ToolError("step: positive, got {!r}".format(step))
        base = config.SPEC + motor
        with self.spec_lock:
            self._claim("tweak " + motor)
        t0 = time.time()
        try:
            self.put(base + ".TWV", step, 5.0)
            # A put to TWF/TWR completes when the move does (DMOV 0 → 1).
            self.put(base + (".TWF" if direction == "forward" else ".TWR"), 1, TWEAK_TIMEOUT)
            rbv = self.pvs[base + ".RBV"].get(timeout=2.0, use_monitor=False)
        finally:
            with self.spec_lock:
                self.spec_job = None
        return {"motor": motor, "direction": direction, "step": step, "rbv": sig(rbv),
                "unit": self.value(base + ".EGU")[1], "seconds": round(time.time() - t0, 2)}

    # -- integrator --------------------------------------------------------
    def integrator(self, method, **params):
        """One REQ to bl5a-integrator; its {"error"} reply raises."""
        req = json.dumps(dict(params, method=method)).encode()
        with self._int_lock:
            if self._int_sock is None:
                s = self._zmq.socket(zmq.REQ)
                s.setsockopt(zmq.LINGER, 0)
                s.setsockopt(zmq.RCVTIMEO, 10000)
                s.setsockopt(zmq.SNDTIMEO, 10000)
                s.connect(self.integrator_rep)
                self._int_sock = s
            try:
                self._int_sock.send(req)
                body = json.loads(self._int_sock.recv().decode())
            except zmq.ZMQError as e:
                # A REQ socket that lost a reply is stuck; the next call
                # starts a new one.
                self._int_sock.close(linger=0)
                self._int_sock = None
                raise ToolError("integrator {}: {} ({})".format(method, e, self.integrator_rep))
        if isinstance(body, dict) and "error" in body:
            raise ToolError("integrator {}: {}".format(method, body["error"]))
        return body

    def _integrator_pub(self):
        sock = self._zmq.socket(zmq.SUB)
        sock.setsockopt(zmq.LINGER, 0)
        sock.setsockopt(zmq.RCVTIMEO, 200)
        sock.setsockopt(zmq.SUBSCRIBE, b"")
        sock.connect(self.integrator_pub)
        while not self._quit.is_set():
            try:
                frame = sock.recv()
            except zmq.Again:
                continue
            topic, _, body = frame.partition(b" ")
            try:
                body = json.loads(body.decode())
            except ValueError:
                logger.warning("integrator PUB: undecodable %r", frame[:80])
                continue
            if topic == b"error":
                self.messages.add("integrator: {}".format(body.get("error")))
            elif topic == b"config":
                self.messages.add("integrator: npt {}, q to {:.4g} {} from now on".format(
                    body.get("npt"), float(body.get("q_max") or 0), body.get("unit")))
        sock.close(linger=0)


# -- tools --------------------------------------------------------------------

class InsituRow(BaseModel):
    """One in-situ table row; a field left out takes a new row's value."""
    model_config = ConfigDict(extra="forbid")

    T: float | None = Field(items.NEW_ROW["T"], description="settemp's set point, °C; null: no temperature change")
    ramp: float | None = Field(items.NEW_ROW["ramp"], description="set_ramp, °C/min, positive")
    wait: bool = Field(items.NEW_ROW["wait"], description="wait until T is reached (needs T)")
    soak_n: int | None = Field(items.NEW_ROW["soak_n"], description="loopscan points while soaking (with soak_t)")
    soak_t: float | None = Field(items.NEW_ROW["soak_t"], description="seconds per soak point (with soak_n)")
    tolerance: float | None = Field(items.NEW_ROW["tolerance"],
                                    description="with wait: how close to T counts as reached, °C")
    align: bool = Field(items.NEW_ROW["align"], description="align_in.mac before the frames")
    th_list: list[float] = Field(list(items.NEW_ROW["th_list"]),
                                 description="mymar_in's th values, one frame each; [] for no frames")
    texpo: float = Field(items.NEW_ROW["texpo"], description="seconds (> 0) or monitor counts (< 0)")
    name: str = Field(items.NEW_ROW["name"], description="mar345 file name (needed with frames)")


DATA_FOLDER = Field(description="Where the item's data goes, as <folder>/<series>: an absolute path without "
                                "spaces; \"\" for bl5a-qs's --data-root; null for the GUI's Data folder setting")
SERIES = Field(description="The series: names the data folder and stack (no spaces or '/')")


def build(bl):
    """The MCP server over Beamline `bl`; its lifespan starts and stops it."""

    @asynccontextmanager
    async def lifespan(server):
        bl.start()
        try:
            yield bl
        finally:
            bl.stop()

    server = MCPServer("bl5a", instructions=INSTRUCTIONS, lifespan=lifespan)

    def tool(read_only=False, destructive=False, idempotent=False):
        hints = ToolAnnotations(read_only_hint=read_only, destructive_hint=destructive, idempotent_hint=idempotent)

        def register(fn):
            @functools.wraps(fn)
            def run(*args, **kwargs):
                try:
                    return fn(*args, **kwargs)
                except ToolError:
                    raise
                except (items.Invalid, QsError) as e:  # the text names the field, or is qs's reason
                    raise ToolError(str(e)) from e
                except Exception as e:  # the client sees only a ToolError's text
                    logger.exception("%s", fn.__name__)
                    raise ToolError("{}: {}".format(type(e).__name__, e)) from e

            return server.tool(annotations=hints, structured_output=False)(run)

        return register

    # -- reading ---------------------------------------------------------
    @tool(read_only=True)
    def status():
        """The beamline now: qs's state, SPEC's, `allowed` (the actions the
        GUI's gate opens now), the ring, shutter, detector and temperatures,
        where data goes, the last frame, what SPEC is doing for this server,
        and the last line number of each log."""
        bl.sync()
        with bl.lock:
            st = bl.qs
            allowed = gate.allowed(st, bl.spec, bl.after_frame.asked)
            spec = dict(bl.spec)
            asked = bl.after_frame.asked
            last_frame = bl.last_frame
            qs = {"error": bl.qs_error} if st is None else {k: st.get(k) for k in QS_KEYS}
        ring = bl.value(config.RING_PV)[0]
        temps = {k: bl.value(pv)[0] for k, pv in (("set_point", config.TEMP_SP_PV), ("readback", config.TEMP_RBV_PV),
                                                 ("ramp", config.TEMP_RAMP_PV)) if pv}
        job = bl.spec_job
        return out({
            "qs": qs, "spec": spec, "allowed": allowed, "stop_after_frame_asked": asked,
            "spec_job": ("command {!r}".format(job.cmd) if isinstance(job, Command) else job),
            "ring_mA": None if ring is None else round(float(ring), 1),
            "shutter": bl.value(config.SHUTTER_PV)[1] if config.SHUTTER_PV else None,
            "mar345": bl.value(config.MAR + "DetectorState_RBV")[1],
            "temperature": temps or None,
            "data": {"gui_data_folder": gui_data_folder(), "mar345_path": bl.value(config.MAR + "FilePath_RBV")[1],
                     "mar345_path_exists": bl.value(config.MAR + "FilePathExists_RBV")[1]},
            "last_frame": last_frame,
            "logs_last": {"messages": bl.messages.last, "spec_log": bl.spec_log.last,
                          "commands": bl.commands.last},
        })

    @tool(read_only=True)
    def queue():
        """The running item and the queued items, in order, with each
        item's uid (for queue_remove and queue_move), plan, file name,
        parameters and data folder."""
        bl.sync()
        with bl.lock:
            q = bl.queue
        running = q.get("running_item") or {}
        return out({"running": _item(running) if running else None, "items": [_item(i) for i in q["items"]]})

    @tool(read_only=True)
    def history(last: Annotated[int, Field(ge=1, le=200)] = 10):
        """The last items bl5a-qs finished, oldest first, each with how it
        ended."""
        bl.sync()
        with bl.lock:
            h = bl.history[-last:]
        return out([dict(_item(i), exit_status=(i.get("result") or {}).get("exit_status"),
                         reason=(i.get("result") or {}).get("reason")) for i in h])

    @tool(read_only=True)
    def motors():
        """Each SPEC motor: readback, whether it moves, tweak step, unit;
        null while its PVs are not connected."""
        rows = {}
        for m in config.MOTORS:
            base = config.SPEC + m + "."
            rbv, dmov, twv = (bl.value(base + f)[0] for f in ("RBV", "DMOV", "TWV"))
            rows[m] = None if rbv is None else {
                "rbv": sig(float(rbv)), "moving": None if dmov is None else int(dmov) == 0,
                "step": None if twv is None else sig(float(twv)), "unit": bl.value(base + "EGU")[1]}
        return out(rows)

    @tool(read_only=True)
    def scans(last: Annotated[int, Field(ge=1, le=SCANS)] = 10):
        """The last scans (ascan, lup, cscan, clup), oldest first: id (for
        `scan`), title, points, how it ended (empty while it runs) and the
        peak statistics SPEC's macros read (xpk, ymax, com, cen, fwhm)."""
        with bl.lock:
            picked = [(k, s, list(s.x), list(s.y)) for k, s in list(bl.scans)[-last:]]
        rows = []
        for k, s, x, y in picked:
            st = peak.stats(x, y)
            rows.append({"id": k, "title": s.title, "motor": s.motor, "det": s.det, "points": len(x),
                         "exit": docs.outcome(s.exit_status, s.reason) or "running",
                         "stats": None if st is None else {f: sig(v) for f, v in st._asdict().items()}})
        return out(rows)

    @tool(read_only=True)
    def scan(id: Annotated[int, Field(description="a scan id from `scans`")]):
        """One scan's points: the motor positions and detector counts."""
        with bl.lock:
            found = next(((s, list(s.x), list(s.y)) for k, s in bl.scans if k == id), None)
        if found is None:
            raise ToolError("no scan {} (the last {} are kept)".format(id, SCANS))
        s, x, y = found
        return out({"id": id, "title": s.title, "motor": s.motor, "det": s.det,
                    "exit": docs.outcome(s.exit_status, s.reason) or "running",
                    "x": [sig(v) for v in x], "y": [sig(v) for v in y]})

    @tool(read_only=True)
    def alignment(last: Annotated[int, Field(ge=1, le=ALIGNED)] = 10):
        """The stages of the last alignment (each with the scan id it took
        its statistic from) and the alignment history: where each finished
        alignment left sy, shold and th's zero."""
        with bl.lock:
            stages = {"caption": bl.stages["caption"], "rows": [rounded(r) for r in bl.stages["rows"]]}
            rows = [rounded(r) for r in list(bl.aligned)[-last:]]
        return out({"stages": stages, "history": rows})

    def log_tool(log, doc):
        def read(since: Annotated[int, Field(ge=0, description="the `last` of the previous read; 0: all kept")] = 0,
                 limit: Annotated[int, Field(ge=1, le=2000)] = 200):
            return out(log.since(since, limit))

        read.__doc__ = doc
        return read

    for name, log, doc in (
            ("messages", bl.messages, "What the GUI's Messages tab shows: items started and ended, frames "
                                      "taken, qs and integrator errors."),
            ("spec_log", bl.spec_log, "SPEC's terminal output, as spec-ioc's Log PV carries it."),
            ("commands", bl.commands, "The SPEC commands bl5a-qs sent for each plan, with their replies.")):
        fn = log_tool(log, doc)
        fn.__name__ = fn.__qualname__ = name
        tool(read_only=True)(fn)

    @tool(read_only=True)
    def integration():
        """What bl5a-integrator integrates with now: PONI text, npt, unit,
        q_max, and its data root."""
        return out(bl.integrator("config"))

    @tool(read_only=True)
    def stacks():
        """The integrated stacks: path (for `stack`), series, rows."""
        return out(bl.integrator("stacks"))

    @tool(read_only=True)
    def stack(path: Annotated[str, Field(description="a stack path from `stacks`")],
              last: Annotated[int, Field(ge=1, le=50)] = 5,
              q_min: float | None = None, q_max: float | None = None,
              points: Annotated[int, Field(ge=10, le=2000)] = 200):
        """The last rows of a stack: each frame's time (s since the epoch),
        th, file and intensity, on q between q_min and q_max, averaged to
        at most `points` bins."""
        listed = next((s for s in bl.integrator("stacks").get("stacks") or [] if s.get("path") == path), None)
        if listed is None:
            raise ToolError("no stack {!r}; `stacks` lists them".format(path))
        start = max(0, int(listed.get("rows") or 0) - last)
        body = bl.integrator("history", path=path, **{"from": start})
        q = np.asarray(body.get("q") or [], float)
        keep = np.ones(len(q), bool)
        if q_min is not None:
            keep &= q >= q_min
        if q_max is not None:
            keep &= q <= q_max
        rows = [np.asarray(r, float)[keep] for r in body.get("intensity") or []]
        qb, rows = binned(q[keep], rows, points)
        frames = body.get("frames") or []
        return out({
            "path": path, "series": body.get("series"), "rows_total": listed.get("rows"), "unit": body.get("unit"),
            "q": [sig(v) for v in qb],
            "rows": [{"index": start + k, "time": f.get("time"), "th": f.get("th"), "texpo": f.get("texpo"),
                      "file": f.get("file"), "intensity": [sig(v) for v in r]}
                     for k, (f, r) in enumerate(zip(frames, rows))],
        })

    @tool(read_only=True)
    def frame_image(path: Annotated[str | None, Field(description="a mar345 file; null: the last frame taken")]
                    = None):
        """A mar345 frame as a log-scaled grey PNG, at most 1024 px a side
        (block means), with its size and counts."""
        if path is None:
            path = bl.last_frame
            if not path:
                raise ToolError("no frame taken since the server started; give a path")
        from PIL import Image as PILImage

        image, pixel = mar.read(path)
        image = np.asarray(image, float)
        k = max(1, math.ceil(max(image.shape) / FRAME_PX))
        h, w = image.shape[0] // k * k, image.shape[1] // k * k
        small = image[:h, :w].reshape(h // k, k, w // k, k).mean(axis=(1, 3))
        positive = small[small > 0]
        with np.errstate(divide="ignore"):
            log = np.log10(np.where(small > 0, small, np.nan))
        lo, hi = (np.percentile(np.log10(positive), [1, 99.9]) if positive.size else (0.0, 1.0))
        grey = np.nan_to_num((log - lo) / max(hi - lo, 1e-9), nan=0.0)
        png = io.BytesIO()
        PILImage.fromarray((np.clip(grey, 0, 1) * 255).astype(np.uint8), "L").save(png, "PNG")
        info = {"path": path, "shape": list(image.shape), "pixel_mm": sig(pixel * 1e3), "binned": k,
                "counts": {"min": sig(float(image.min())), "max": sig(float(image.max())),
                           "mean": sig(float(image.mean()))}}
        return [out(info), Image(data=png.getvalue(), format="png")]

    @tool(read_only=True)
    def spec_result():
        """The reply of the last SPEC command sent from here, or how long it
        has been running."""
        return out(bl.command_result())

    # -- queueing --------------------------------------------------------
    @tool()
    def queue_alignment(kind: Literal["coin", "film"],
                        series: Annotated[str | None, SERIES] = None,
                        data_folder: Annotated[str | None, DATA_FOLDER] = None):
        """Queue an alignment: align_coin (coin cell: coin_align's scans) or
        align_film (thin film: align_in.mac's scans). The GUI's Alignment
        buttons."""
        return out(bl.add([items.align("align_" + kind, series)], data_folder))

    @tool()
    def queue_mar_series(name: Annotated[str, Field(description="mar345 file name: 1-27 characters, no spaces "
                                                                "or quotes")],
                         texpo: Annotated[float, Field(description="seconds 1 to 20, or negative monitor counts")],
                         beamstop_in_place: Annotated[bool, Field(description="the operator confirmed the "
                                                                              "beamstop is in place")],
                         geometry: Literal[items.GEOMETRIES] = "standard",
                         n: Annotated[int, Field(ge=1, description="frames; more than 1 needs ti")] = 1,
                         ti: Annotated[float | None, Field(description="minutes from one frame's start to the "
                                                                       "next, more than 2.5")] = None,
                         series: Annotated[str | None, Field(description="The series: names the data folder "
                                                                         "and stack; null: the file name")] = None,
                         data_folder: Annotated[str | None, DATA_FOLDER] = None):
        """Queue mar345 frames: mymar1 (n = 1) or mymar (n frames, one every
        ti minutes). The GUI's Battery form. The frames are taken with the
        shutter open: ask the operator whether the beamstop is in place."""
        if not beamstop_in_place:
            raise ToolError("the frames are taken with the shutter open: queue them once the operator confirms "
                            "the beamstop is in place")
        return out(bl.add([items.mar_series(name, texpo, geometry, n=n, ti=ti, series=series)], data_folder))

    @tool()
    def queue_insitu(rows: Annotated[list[InsituRow], Field(min_length=1)],
                     series: Annotated[str | None, Field(description="The series of every row; null: the first "
                                                                      "row name")] = None,
                     append_cooling_row: Annotated[bool, Field(description="end with myinsitu's last row: ramp "
                                                                           "25 °C/min to 25 °C, soak 3600 "
                                                                           "points of 1 s")] = False,
                     data_folder: Annotated[str | None, DATA_FOLDER] = None):
        """Queue the in-situ table, one insitu_step per row, in order and in
        one series: set_ramp → settemp (wait) → soak loopscan → align_in.mac
        → mymar_in at each th. The GUI's In-situ form."""
        table = [r.model_dump(exclude_unset=True) for r in rows]
        if append_cooling_row:
            table.append(items.cooling_row())
        return out(bl.add(items.insitu_items(table, series), data_folder))

    # -- the queue ---------------------------------------------------------
    @tool(destructive=True)
    def queue_remove(uids: Annotated[list[str], Field(min_length=1)]):
        """Take queued items out by uid. The running item is not in the
        queue: stop or abort ends it."""
        done = []
        with bl.re_lock:
            for uid in uids:
                try:
                    bl.RE.queue_item_remove(uid)
                except QsError as e:
                    raise ToolError("removed {} ({}), then {}: {}".format(
                        len(done), ", ".join(done) or "none", uid, e))
                done.append(uid)
        return out({"removed": done})

    @tool()
    def queue_move(uid: str, before_uid: str | None = None, after_uid: str | None = None):
        """Move a queued item before or after another (exactly one)."""
        if (before_uid is None) == (after_uid is None):
            raise ToolError("give exactly one of before_uid and after_uid")
        where = {"before_uid": before_uid} if before_uid is not None else {"after_uid": after_uid}
        return out(_item(bl.re(bl.RE.queue_item_move, uid, **where)))

    @tool(destructive=True, idempotent=True)
    def queue_clear():
        """Remove every queued item; the running one goes on. Ask the
        operator first."""
        bl.re(bl.RE.queue_clear)
        return out({"cleared": True})

    @tool()
    def queue_start():
        """Run the queue from its front until it is empty."""
        bl.re(bl.RE.queue_start)
        return out({"started": True})

    @tool()
    def queue_stop(cancel: Annotated[bool, Field(description="withdraw an earlier queue_stop")] = False):
        """Stop the queue after the running item, without interrupting it."""
        bl.re(bl.RE.queue_stop, cancel=cancel)
        return out({"queue_stop": not cancel})

    @tool(idempotent=True)
    def queue_autostart(enable: bool):
        """Run what is added to the queue without queue_start. qs disarms it
        when the queue ends on anything but a drain."""
        bl.re(bl.RE.queue_autostart, enable)
        return out({"queue_autostart": enable})

    @tool(idempotent=True)
    def environment_open():
        """Open bl5a-qs's worker environment; returns once it is open."""
        bl.re(bl.RE.environment_open)
        return out({"environment": "open"})

    @tool(idempotent=True)
    def environment_close():
        """Close bl5a-qs's worker environment; returns once it is closed."""
        bl.re(bl.RE.environment_close)
        return out({"environment": "closed"})

    # -- the running plan --------------------------------------------------
    @tool()
    def pause():
        """Pause the running plan now; a moving motor is stopped. The plans
        do not replay a SPEC macro cut in half, so resume then fails the
        plan: end it with stop or abort. To end a mar_series between
        frames, use stop_after_frame instead."""
        bl.require("pause")
        bl.re(bl.RE.request_pause, defer=False)
        return out({"pause": "requested"})

    @tool()
    def resume():
        """Resume the paused plan. A plan paused by `pause` fails here with
        "plan interrupted: a paused plan cannot resume, abort it"."""
        bl.require("resume")
        bl.re(bl.RE.resume)
        return out({"resume": "requested"})

    @tool(destructive=True)
    def stop():
        """End the paused or running plan as a success."""
        bl.require("stop")
        bl.re(bl.RE.stop)
        return out({"stop": "requested"})

    @tool(destructive=True)
    def abort():
        """End the running plan as aborted. Ask the operator first."""
        bl.require("abort")
        with bl.lock:
            bl.after_frame.asked = False
        bl.re(bl.RE.abort)
        return out({"abort": "requested"})

    @tool(destructive=True)
    def stop_after_frame():
        """Let the mar_series frame being taken finish, then stop the
        series: a deferred pause now, and a stop once qs reports paused.
        Asked before the first frame starts, the series stops with none."""
        bl.sync()
        with bl.lock:  # `asked` set in the same hold as the check that it was not
            bl.check("stop_after_frame")
            bl.after_frame.asked = True
        try:
            bl.re(bl.RE.request_pause, defer=True)
        except Exception:
            with bl.lock:
                bl.after_frame.asked = False
            raise
        return out({"stop_after_frame": "asked"})

    # -- SPEC --------------------------------------------------------------
    @tool()
    def tweak(motor: Annotated[str, Field(description="one of " + ", ".join(config.MOTORS))],
              step: Annotated[float, Field(gt=0, description="the move, in the motor's unit")],
              direction: Literal["forward", "reverse"]):
        """Move a motor by `step` (its TWV, then TWF or TWR) and return when
        the move is done, with the readback. Only while SPEC is free."""
        return out(bl.tweak(motor, step, direction))

    @tool()
    def spec_command(cmd: Annotated[str, Field(description="one SPEC command line, as typed at the SPEC prompt")],
                     wait_s: Annotated[float, Field(ge=0, le=600)] = 30.0):
        """Send one command to SPEC (the GUI's SPEC line) and wait up to
        wait_s for its status and reply. What it prints (wm, wa, …) goes to
        the SPEC log: spec_log(since=spec_log_since). Only while SPEC is
        free; a command still running after wait_s keeps SPEC: spec_result
        waits for it."""
        return out(bl.spec_command(cmd, wait_s))

    @tool(destructive=True)
    def spec_abort():
        """Interrupt the SPEC command sent from here (^C); never a plan's."""
        return out(bl.spec_abort())

    # -- integration -------------------------------------------------------
    @tool()
    def integration_set(poni_path: Annotated[str, Field(description="a pyFAI PONI file of the full plate; its "
                                                                    "text is sent")],
                        npt: Annotated[int, Field(ge=1, le=100000, description="bins from q = 0 to the plate's "
                                                                               "far rim")]):
        """Integrate the frames after those already queued with this PONI
        and npt. A new q axis starts a new stack; existing stacks keep
        theirs."""
        try:
            with open(poni_path) as f:
                text = f.read()
        except OSError as e:
            raise ToolError("{}: {}".format(poni_path, e.strerror)) from e
        accepted = bl.integrator("set", poni=text, npt=npt)
        active = bl.integrator("config")
        shown = {k: v for k, v in accepted.items() if k != "poni"}  # the caller has the text
        return out(dict(shown, poni_path=poni_path, applies="now" if accepted == active
                        else "after the frames already queued (messages says when)"))

    return server


def main(argv=None):
    p = argparse.ArgumentParser(prog="bl5a-mcp", description=__doc__.splitlines()[0])
    args = config.arguments(p).parse_args(sys.argv[1:] if argv is None else argv)
    logging.basicConfig(level=logging.INFO, stream=sys.stderr,
                        format="%(asctime)s %(name)s %(levelname)s %(message)s")
    bl = Beamline(args.qs_control, args.qs_documents, args.integrator_rep, args.integrator_pub, args.user)
    build(bl).run("stdio")
    return 0
