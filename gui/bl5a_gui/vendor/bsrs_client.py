"""bsrs daemon client for the BL7C nano-XRM GUI (doc/bsrs-core-plan.md).

`RunEngineProxy` stands in for bluesky's RunEngine in main.py: the GUI
keeps calling `RE(plan, **md)`,
`RE.state`, `RE.request_pause()`, `RE.resume()`, `RE.abort()` and
`RE.subscribe(cb)` / `RE.unsubscribe(token)`. Commands go to the bsrs qs
control socket (ZMQ REQ, JSON `{"method", "params"}`, port 60615, the
bluesky-queueserver envelope); documents come from the qs PUB socket (port
60625, bluesky Publisher frames `b"<prefix> <name> <msgpack body>"`) through
`DocumentRelay`, which fans them out to the subscribed callbacks on its own
thread the way RunEngine callbacks ran on the scan thread.

What differs from the RunEngine, and what the GUI code (commit 14) relies on:

- A plan is a queue item built with `plan(name, *args, **kwargs)`; the
  generator functions of 90-plans.py live in the daemon.  `RE(item, **md)`
  puts `md` into the item's `md` keyword, which every BL7C plan merges into
  its start document.  The multi-scan loop 90-plans.py built around the
  scans is the daemon's queue on this core (main.py `enqueue_scan`); a
  Python generator that yields items is still driven by `RE(gen)`.
- `RE(item)` blocks until the item has finished, through pauses, and raises
  `RunEngineInterrupted` when it did not succeed.  bluesky returned at the
  pause and made `resume()` block instead; here `resume()` returns at once.
  Before returning, `RE(item)` waits until the relay has delivered the run's
  stop document, so `db[-1]` already holds the run.
- `install_suspenders()` / `uninstall_suspenders()` are daemon functions
  reached with `function_execute`; they need an open environment.

- While `RE(...)` blocks on the GUI thread the Qt event loop is pumped
  (`qt_during_wait`), as bluesky's DefaultDuringTask did.

Only pyzmq and msgpack are required (qtpy when present), so the GUI process
no longer imports bluesky in bsrs mode.
"""

import copy
import inspect
import itertools
import json
import logging
import os
import threading
import time

import msgpack
import zmq

logger = logging.getLogger(__name__)

CONTROL_ADDRESS = "tcp://localhost:60615"
DOCUMENT_ADDRESS = "tcp://localhost:60625"

# Which group the daemon puts this process in (rust/bl7c-qs/permissions.toml).
# The GUI needs the operator key: `install_suspenders`, `uninstall_suspenders`
# and the camera's `connect_*` / `warmup_*` go through `function_execute`,
# which the daemon classifies as an admin method. Unset, the GUI is whatever
# the daemon's default_group is and those calls come back refused.
API_KEY = os.environ.get("BL7C_QS_API_KEY", "")

# bsrs `re_state` -> the RunEngine state names the GUI and DataViewer show.
_RE_STATES = {
    "idle": "idle",
    "executing_queue": "running",
    "paused": "paused",
}

# Item exit statuses that mean the plan did not run to completion.
_INTERRUPTED = ("abort", "fail", "halt", "stop")


class QsError(Exception):
    """A qs request failed (transport error or `success: false`)."""


class RunEngineInterrupted(QsError):
    """The plan ended by abort, halt, stop or failure (bluesky's name)."""

    def __init__(self, exit_status, msg=""):
        super().__init__("plan {}: {}".format(exit_status, msg) if msg else "plan {}".format(exit_status))
        self.exit_status = exit_status


def plan(name, *args, **kwargs):
    """A queue item for the daemon plan `name` (positional and keyword args
    as the plan's parameter list declares them)."""
    return {"item_type": "plan", "name": name, "args": list(args), "kwargs": dict(kwargs)}


class QsClient:
    """The control socket: one request at a time, JSON in, JSON out.

    ZMQ REQ sockets lock up after a lost reply, so a timed-out request
    replaces the socket before the next call."""

    def __init__(self, address=CONTROL_ADDRESS, timeout_ms=5000, api_key=None):
        self.address = address
        self.timeout_ms = timeout_ms
        self.api_key = API_KEY if api_key is None else api_key
        self._ctx = zmq.Context.instance()
        self._lock = threading.Lock()
        self._sock = None

    def _socket(self):
        if self._sock is None:
            s = self._ctx.socket(zmq.REQ)
            s.setsockopt(zmq.LINGER, 0)
            s.setsockopt(zmq.RCVTIMEO, self.timeout_ms)
            s.setsockopt(zmq.SNDTIMEO, self.timeout_ms)
            s.connect(self.address)
            self._sock = s
        return self._sock

    def _reset(self):
        if self._sock is not None:
            self._sock.close(linger=0)
            self._sock = None

    def close(self):
        with self._lock:
            self._reset()

    def call(self, method, params=None):
        """Send `method` and return the reply as a dict; raises QsError on a
        transport failure."""
        params = dict(params or {})
        if self.api_key and "api_key" not in params:
            params["api_key"] = self.api_key
        request = json.dumps({"method": method, "params": params}).encode()
        with self._lock:
            try:
                sock = self._socket()
                sock.send(request)
                reply = sock.recv()
            except zmq.ZMQError as e:
                self._reset()
                raise QsError("{}: {} ({})".format(method, e, self.address))
        try:
            return json.loads(reply.decode())
        except ValueError as e:
            raise QsError("{}: undecodable reply: {}".format(method, e))

    def ok(self, method, params=None):
        """`call`, raising QsError when the manager answers `success: false`."""
        reply = self.call(method, params)
        if not reply.get("success", False):
            raise QsError("{}: {}".format(method, reply.get("msg") or "request failed"))
        return reply


class DocumentRelay(threading.Thread):
    """SUB on the daemon's document stream; every `(name, doc)` goes to each
    subscribed callback in subscription order.  A callback that raises is
    logged and skipped, the others still run."""

    def __init__(self, address=DOCUMENT_ADDRESS, poll_ms=200):
        super().__init__(name="bsrs-document-relay", daemon=True)
        self.address = address
        self.poll_ms = poll_ms
        self._ctx = zmq.Context.instance()
        # Not `_stop`: `threading.Thread._stop` is a method `join` calls, and
        # an Event in its place made `join` raise instead of waiting.
        self._quit = threading.Event()
        self._lock = threading.Lock()
        self._callbacks = {}  # token -> callback
        self._by_callback = {}  # callback -> token (one subscription per callback)
        self._tokens = itertools.count()
        self._cond = threading.Condition()
        self._stopped_runs = set()
        self.last_start = None
        # Documents received so far: what a waiter reads to tell a stream
        # that is still coming from one that is over (wait_for_run_documents).
        self.dispatched = 0

    def subscribe(self, callback):
        """Register `callback(name, doc)`; returns the token for `unsubscribe`.
        A callback that is already subscribed gets its existing token back
        and is not called twice (bluesky's CallbackRegistry did the same,
        and scan_mixin.subscribe_callback relies on it: the fly branch
        subscribes the AfterScanCallback a second time)."""
        with self._lock:
            token = self._by_callback.get(callback)
            if token is None:
                token = next(self._tokens)
                self._callbacks[token] = callback
                self._by_callback[callback] = token
        return token

    def unsubscribe(self, token):
        """Remove a subscription; an unknown token is ignored."""
        with self._lock:
            callback = self._callbacks.pop(token, None)
            if callback is not None:
                self._by_callback.pop(callback, None)

    def stop(self):
        self._quit.set()

    def wait_for_stop(self, run_uid, timeout):
        """True once the stop document of `run_uid` has been dispatched."""
        deadline = time.monotonic() + timeout
        with self._cond:
            while run_uid not in self._stopped_runs:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return False
                self._cond.wait(remaining)
        return True

    def run(self):
        sock = self._ctx.socket(zmq.SUB)
        sock.setsockopt(zmq.LINGER, 0)
        sock.setsockopt(zmq.RCVTIMEO, self.poll_ms)
        sock.setsockopt(zmq.SUBSCRIBE, b"")
        sock.connect(self.address)
        try:
            while not self._quit.is_set():
                try:
                    frame = sock.recv()
                except zmq.Again:
                    continue
                except zmq.ZMQError as e:
                    logger.error("document relay: %s", e)
                    break
                parsed = self._parse(frame)
                if parsed is not None:
                    self._dispatch(*parsed)
        finally:
            sock.close(linger=0)

    @staticmethod
    def _parse(frame):
        parts = frame.split(b" ", 2)
        if len(parts) != 3:
            logger.warning("document relay: malformed frame (%d bytes)", len(frame))
            return None
        name = parts[1].decode()
        try:
            doc = msgpack.unpackb(parts[2], raw=False)
        except Exception as e:  # msgpack raises several exception types
            logger.warning("document relay: undecodable %s document: %s", name, e)
            return None
        return name, doc

    def _dispatch(self, name, doc):
        self.dispatched += 1
        if name == "start":
            self.last_start = doc.get("uid")
        with self._lock:
            callbacks = list(self._callbacks.values())
        for cb in callbacks:
            try:
                cb(name, doc)
            except Exception:
                logger.exception("document relay: %s callback %r failed", name, cb)
        if name == "stop":
            with self._cond:
                self._stopped_runs.add(doc.get("run_start"))
                self._cond.notify_all()


def local_sleep(seconds):
    """An item the proxy handles itself: wait `seconds` without contacting
    the daemon.  Composite plans (see `RunEngineProxy.__call__`) use it to
    poll GUI state between daemon items; pause and abort apply to it."""
    return {"item_type": "wait", "seconds": float(seconds)}


_QT = None  # the Qt classes qt_during_wait needs, or False when qtpy is missing


def qt_during_wait(seconds):
    """Default `during_wait`: on the Qt GUI thread, run a nested event loop
    for `seconds` so the GUI stays responsive while `RE(...)` blocks, the
    way bluesky's DefaultDuringTask did; elsewhere, or without a
    QApplication, plain `time.sleep`."""
    global _QT
    if _QT is None:
        try:
            from qtpy.QtCore import QCoreApplication, QEventLoop, QThread, QTimer

            _QT = (QCoreApplication, QEventLoop, QThread, QTimer)
        except ImportError:
            _QT = False
    if _QT:
        QCoreApplication, QEventLoop, QThread, QTimer = _QT
        app = QCoreApplication.instance()
        if app is not None and QThread.currentThread() is app.thread():
            loop = QEventLoop()
            QTimer.singleShot(int(seconds * 1000), loop.quit)
            loop.exec_()
            return
    time.sleep(seconds)


class RunEngineProxy:
    """The RunEngine surface the GUI uses, over a `QsClient` and a
    `DocumentRelay`.

    `RE(item)` runs one daemon item.  `RE(gen)` drives a *composite plan*: a
    generator that yields items and gets each item's run uids sent back,
    the way 90-plans.py's multi_exafs_scan / tweak_custom yielded
    sub-plans.  Between items the daemon is idle, so the proxy keeps the
    RunEngine semantics itself: `state` stays 'running', `request_pause()`
    holds the composite before its next item ('paused' until `resume()`),
    and `abort()` raises RunEngineInterrupted inside the generator at its
    current yield, so its `finally` runs, and then out of `RE(...)`.

    Every wait goes through `during_wait(seconds)`; the default pumps the
    Qt event loop when called on the GUI thread."""

    def __init__(self, client, relay, user="bl7c", user_group="primary", poll_s=0.2, during_wait=qt_during_wait):
        self.client = client
        self.relay = relay
        self.user = user
        self.user_group = user_group
        # RE.md: keys under every run's start document, below the call's md
        # (00-runengine.py set RE.md['user']; bsrs has no server-side md).
        self.md = {"user": user}
        self.poll_s = poll_s
        self.during_wait = during_wait
        # What the daemon cannot see: the composite plan between two items.
        self._local_state = "idle"
        self._pause_requested = False
        # 'abort' | 'halt' | 'stop' requested for the running plan and not yet
        # thrown into it.  `_drive` is the only place it is delivered, and it
        # clears it there, so the items a composite yields while unwinding run
        # instead of gating -- bluesky processes a plan's cleanup messages the
        # same way after throwing RequestAbort in.
        self._interrupt = None
        self._md_plans = None  # names of the plans that take `md` (from plans_allowed)
        # `run_in_db(run_uid) -> bool`: waits until the run is stored, set by
        # the GUI to `run_reader.RunReader.wait_for_run` (main.Main).  The
        # documents are written by `mongo_writer.py` in another process, so
        # "the plan ended" and "the run can be read" are no longer the same
        # instant; `RE(...)` does not return until both hold, which is what
        # keeps `self.db[-1]` after a scan naming the scan that just ran.
        # None -- a test, a console -- means nobody reads the database and
        # there is nothing to wait for.
        self.run_in_db = None

    def _wait(self):
        self.during_wait(self.poll_s)

    # -- status ---------------------------------------------------------
    def status(self):
        return self.client.ok("status")

    def _daemon_state(self):
        re_state = self.status().get("re_state", "idle")
        return _RE_STATES.get(re_state, re_state)

    @property
    def state(self):
        """'idle', 'running' or 'paused' (bluesky's names)."""
        daemon = self._daemon_state()
        return daemon if daemon != "idle" else self._local_state

    def environment_open(self, timeout=30.0):
        """Open the daemon's environment unless it is already open."""
        if self.status().get("worker_environment_exists"):
            return
        self.client.ok("environment_open")
        deadline = time.monotonic() + timeout
        while not self.status().get("worker_environment_exists"):
            if time.monotonic() > deadline:
                raise QsError("environment_open: not open after {} s".format(timeout))
            self._wait()

    def environment_close(self, timeout=30.0):
        """Close the daemon's environment if it is open, and return once it
        is gone -- the worker takes a moment to shut down, and a caller
        that reports the state right after the request would report the
        old one."""
        if not self.status().get("worker_environment_exists"):
            return
        self.client.ok("environment_close")
        deadline = time.monotonic() + timeout
        while self.status().get("worker_environment_exists"):
            if time.monotonic() > deadline:
                raise QsError("environment_close: still open after {} s".format(timeout))
            self._wait()

    # -- interruptions --------------------------------------------------
    # A request applies to the plan in progress; with nothing running it is
    # dropped, so no stale request can hit the next plan.
    def request_pause(self, defer=False):
        if self.state == "idle":
            return
        self._pause_requested = True
        if self._daemon_state() != "idle":
            self._forward("re_pause", {"option": "deferred" if defer else "immediate"})

    def resume(self):
        self._pause_requested = False
        if self._daemon_state() == "paused":
            self.client.ok("re_resume")

    def abort(self):
        self._interrupt_with("abort")

    def halt(self):
        self._interrupt_with("halt")

    def stop(self):
        self._interrupt_with("stop")

    def _interrupt_with(self, kind):
        if self.state == "idle":
            return
        self._interrupt = kind
        self._pause_requested = False
        if self._daemon_state() != "idle":
            self._forward("re_" + kind)

    def _forward(self, method, params=None):
        """Send an interruption; the daemon having finished the item first
        is not an error."""
        try:
            self.client.ok(method, params)
        except QsError:
            if self._daemon_state() != "idle":
                raise

    # -- documents ------------------------------------------------------
    def subscribe(self, callback):
        return self.relay.subscribe(callback)

    def unsubscribe(self, token):
        self.relay.unsubscribe(token)

    # -- plans ----------------------------------------------------------
    def __call__(self, plan, **md):
        """Run `plan` -- an item from `plan(...)` or a composite generator --
        and block until it has finished, through pauses.  `RE.md` and `md`
        go into the start document of every run (items of plans without an
        `md` parameter, such as mv, do not get them); the call's `md` wins
        over `RE.md`, and an item's own `md` over both, as a plan's md did
        over RE(plan, **md) over RE.md.  Returns the run uids;
        raises `RunEngineInterrupted` when the plan was aborted, halted,
        stopped or failed, after the generator's cleanup -- including the
        items it yields while unwinding -- has run."""
        self._local_state = "running"
        self._pause_requested = False
        self._interrupt = None
        try:
            if inspect.isgenerator(plan):
                return self._drive(plan, md)
            return self._run_item(plan, md)
        finally:
            self._local_state = "idle"
            self._pause_requested = False
            self._interrupt = None

    def _drive(self, gen, md):
        uids = []
        interrupted = None
        send, value = gen.send, None
        while True:
            try:
                item = send(value)
            except StopIteration:
                break
            try:
                value = self._run_item(item, md)
            except RunEngineInterrupted as e:
                interrupted = interrupted or e
                self._interrupt = None  # delivered; let the cleanup items run
                send, value = gen.throw, e
            except Exception as e:  # QsError, a malformed item
                send, value = gen.throw, e
            else:
                uids.extend(value)
                send = gen.send
        if interrupted is not None:
            raise interrupted
        return tuple(uids)

    def _run_item(self, item, md):
        self._gate()
        if item.get("item_type") == "wait":
            self._local_wait(item["seconds"])
            return ()
        return self._execute(item, md)

    def _gate(self):
        """Honour a pause or interruption requested while the daemon was
        idle: hold here until resumed, raise if an interruption has not
        reached the plan yet."""
        while self._pause_requested and self._interrupt is None:
            self._local_state = "paused"
            self._wait()
        self._local_state = "running"
        if self._interrupt is not None:
            raise RunEngineInterrupted(self._interrupt, "between items")

    def _local_wait(self, seconds):
        deadline = time.monotonic() + seconds
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            self.during_wait(min(self.poll_s, remaining))
            self._gate()

    def _accepts_md(self, name):
        """Whether daemon plan `name` has an `md` parameter (the run-opening
        plans do; mv, sleep, stop_and_mv do not), from `plans_allowed`."""
        if self._md_plans is None or name not in self._md_plans:
            allowed = self.client.ok("plans_allowed", {"user_group": self.user_group}).get("plans_allowed") or {}
            self._md_plans = {
                plan_name: any(p.get("name") == "md" for p in meta.get("parameters") or [])
                for plan_name, meta in allowed.items()
            }
        return self._md_plans.get(name, True)

    def _with_md(self, item, md=None):
        """`item` with `RE.md`, the call's `md` and the item's own merged in
        the order `__call__` documents.  The queue is added to through here
        as well, so an item waiting in the queue carries the metadata it
        would have carried had the GUI run it immediately."""
        item = copy.deepcopy(item)
        kwargs = item.setdefault("kwargs", {})
        own = kwargs.get("md") or {}
        md = md or {}
        if (self.md or md or own) and self._accepts_md(item.get("name")):
            merged = dict(self.md)
            merged.update(md)
            merged.update(own)
            kwargs["md"] = merged
        return item

    def _execute(self, item, md):
        """One daemon item, outside the queue, to completion."""
        item = self._with_md(item, md)
        plans_run = self.status()["plans_run"]
        last_start = self.relay.last_start
        reply = self.client.ok(
            "queue_item_execute",
            {"item": item, "user": self.user, "user_group": self.user_group},
        )
        item_uid = reply["item"]["item_uid"]
        result = self._await_result(item_uid, plans_run)
        exit_status = result.get("exit_status", "fail")
        run_uid = result.get("run_uid")
        if not run_uid and self.relay.last_start != last_start:
            # An interrupted item archives no run uid, so the run it opened
            # is the one the relay saw start while it ran.  Its documents
            # are waited for like a finished run's: a caller that
            # unsubscribes as soon as `RE(...)` raises would otherwise miss
            # the stop document of the run it just aborted.
            run_uid = self.relay.last_start
        self.wait_for_run_documents(run_uid)
        if exit_status in _INTERRUPTED:
            # The daemon archives the cause as `reason`, bluesky's
            # `RunEngineResult.reason` (bsrs qs/server.rs); `msg` is the name
            # bluesky-queueserver uses for the same text.
            raise RunEngineInterrupted(exit_status, result.get("reason") or result.get("msg") or "")
        return (run_uid,) if run_uid else ()

    def wait_for_run_documents(self, run_uid=None, idle_timeout=5.0):
        """Wait until the relay has dispatched `run_uid`'s stop document,
        by default the last run it saw start.

        The daemon archives an item -- and the queue moves on, or goes
        idle -- as soon as the plan ends, which can be before the relay
        has dispatched that run's last documents.  A caller that
        unsubscribes when the plan ends therefore has to wait here first,
        or its callback misses the stop document it was subscribed for.

        How long that takes is the run's size, not a constant: a fly CT
        publishes its frames as one bulk event big enough to lose a fixed
        race, and a deadline short enough to bound a dead stream expires
        in the middle of a live one.  So the wait ends when the relay goes
        quiet instead: while documents keep arriving there is more of the
        run to deliver, and `idle_timeout` seconds without a single one
        mean the stream is over.

        Then the run has to be *stored* as well: `run_in_db` waits for the
        writer process, so a caller that reads the run back out of Mongo
        after `RE(...)` reads the run it just ran."""
        run_uid = run_uid or self.relay.last_start
        if not run_uid:
            return True
        dispatched = self.relay.dispatched
        quiet_since = time.monotonic()
        while not self.relay.wait_for_stop(run_uid, 0.0):
            if self.relay.dispatched != dispatched:
                dispatched = self.relay.dispatched
                quiet_since = time.monotonic()
            elif time.monotonic() - quiet_since > idle_timeout:
                logger.warning("run %s: stop document not seen on the relay", run_uid)
                return False
            self._wait()
        if self.run_in_db is not None and not self.run_in_db(run_uid):
            logger.warning("run %s: not stored in the database", run_uid)
            return False
        return True

    def _await_result(self, item_uid, plans_run):
        """The item's archived result, once the daemon has run it.

        The wait is on `plans_run` and not on an idle manager: the daemon
        answers `queue_item_execute` from the request thread and the worker
        flips the state out of idle only afterwards, so an idle manager
        right after the reply means "not started yet" as often as it means
        "finished" -- and a scan started on a daemon that has just come up
        loses that race.  `plans_run` is counted in the same step that
        archives the result, so it never reads finished early."""
        while True:
            if self.status()["plans_run"] != plans_run:
                result = self._history_entry(item_uid)
                if result is not None:
                    return result
            self._wait()

    def _history_entry(self, item_uid):
        """The item's archived result, or None while it is not archived."""
        for entry in reversed(self.client.ok("history_get").get("items", [])):
            if entry.get("item_uid") == item_uid:
                return entry.get("result") or {}
        return None

    # -- the daemon's queue -----------------------------------------------
    # The GUI runs a scan with `__call__` (queue_item_execute), which does
    # not touch the queue, so the queue holds what the operator put there
    # and these are the calls a queue view needs.  Every one of them raises
    # QsError when the daemon refuses, which is what the caller reports.
    def queue_get(self):
        """`{'items': [...], 'running_item': {...} or {}, 'plan_queue_uid':
        ...}`.  `plan_queue_uid` changes on every queue mutation, so a poll
        can tell an unchanged queue without comparing the items.

        The running item comes back as it was queued, `item_uid` included,
        but it has left the queue: no queue edit reaches it, and `abort` /
        `stop` are what end it."""
        reply = self.client.ok("queue_get")
        return {"items": list(reply.get("items") or []),
                "running_item": reply.get("running_item") or {},
                "plan_queue_uid": reply.get("plan_queue_uid")}

    def queue_item_add(self, item, md=None, **where):
        """Put `item` at the end of the queue, or at `pos=` / `before_uid=`
        / `after_uid=`.  Returns the queued item, which carries the
        `item_uid` the queue is edited by."""
        params = {"item": self._with_md(item, md),
                  "user": self.user, "user_group": self.user_group}
        params.update(where)
        return self.client.ok("queue_item_add", params)["item"]

    def queue_item_remove(self, uid):
        """Take one item out of the queue by its `item_uid`."""
        return self.client.ok("queue_item_remove", {"uid": uid})["item"]

    def queue_item_move(self, uid, **dest):
        """Move `uid` to `before_uid=`, `after_uid=` or `pos_dest=`.

        A neighbour's uid rather than an index: the queue can have changed
        since the view drew it, and "before that item" still names the same
        place when it has, while "position 3" does not."""
        return self.client.ok("queue_item_move", dict(dest, uid=uid))["item"]

    def queue_clear(self):
        """Drop every queued item.  The running one is not in the queue."""
        self.client.ok("queue_clear")

    def queue_start(self):
        """Run the queue from its front until it is empty."""
        self.client.ok("queue_start")

    def queue_autostart(self, enable=True):
        """Arm the queue: the daemon runs what is added to it, without a
        `queue_start`.

        Enabling it starts a queue that already has items in it.  The
        daemon disarms it again whenever the queue ends on anything but a
        normal drain -- a failed or aborted plan, a `queue_stop` -- so a
        caller that wants the next add to run says so again."""
        self.client.ok("queue_autostart", {"enable": bool(enable)})

    def queue_stop(self, cancel=False):
        """Stop the queue after the running item, or withdraw that request.
        The running item is not interrupted -- `abort`/`stop` do that."""
        self.client.ok("queue_stop_cancel" if cancel else "queue_stop")

    # -- daemon functions -------------------------------------------------
    def function_execute(self, name, *args, timeout=15.0, **kwargs):
        """Call the daemon's Lua global `name` and return its Lua-formatted
        return value (e.g. 'true', 'nil')."""
        reply = self.client.ok(
            "function_execute",
            {
                "item": {"item_type": "function", "name": name, "args": list(args), "kwargs": kwargs},
                "user": self.user,
                "user_group": self.user_group,
            },
        )
        task_uid = reply["task_uid"]
        deadline = time.monotonic() + timeout
        while True:
            task = self.client.ok("task_result", {"task_uid": task_uid})
            # The daemon reports `completed` only for a call that succeeded
            # and `failed` for one that raised (qs/tasks.rs:130), so the poll
            # ends on anything but `running` and `result.success` is what
            # says which it was; the reason is in `traceback` for a Lua
            # error.
            if task.get("status") != "running":
                result = task.get("result") or {}
                if not result.get("success", False):
                    raise QsError("{}: {}".format(name, result.get("msg") or result.get("traceback") or "failed"))
                return result.get("return_value")
            if time.monotonic() > deadline:
                raise QsError("{}: no result after {} s".format(name, timeout))
            self._wait()

    def install_suspenders(self):
        self.function_execute("install_suspenders")

    def uninstall_suspenders(self):
        self.function_execute("uninstall_suspenders")


def connect(control=CONTROL_ADDRESS, documents=DOCUMENT_ADDRESS, timeout_ms=5000, **proxy_kwargs):
    """A `RunEngineProxy` on a running daemon, with its relay thread started.
    Raises QsError if the daemon does not answer."""
    client = QsClient(control, timeout_ms)
    client.ok("ping")
    relay = DocumentRelay(documents)
    relay.start()
    return RunEngineProxy(client, relay, **proxy_kwargs)
