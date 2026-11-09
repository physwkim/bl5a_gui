"""The GUI's links to bl5a-qs, the integrator and the PVs. Each blocks
only its own thread; what they learn reaches the widgets as Qt signals,
which Qt delivers on the GUI thread."""

import json
import logging
import queue
import threading
import time

import zmq
from repics import CaError
from repics.pv import PV
from silx.gui import qt

from .vendor.bsrs_client import DocumentRelay, QsClient, QsError, RunEngineProxy

logger = logging.getLogger(__name__)

CONNECT_TIMEOUT = 2.0  # s, for a PV to connect before a put to it


class QsPoller(qt.QThread):
    """bl5a-qs's status twice a second, its queue whenever the status says
    the queue or the running item changed, and its history whenever the
    history changed. `status` carries None while qs does not answer."""

    status = qt.Signal(object)
    queue = qt.Signal(object)
    history = qt.Signal(object)
    lost = qt.Signal(str)

    def __init__(self, address, period=0.5, parent=None):
        super().__init__(parent)
        self.address = address
        self.period = period
        self._quit = threading.Event()

    def stop(self):
        self._quit.set()
        self.wait(3000)

    def run(self):
        client = QsClient(self.address, 2000)
        seen = None  # (plan_queue_uid, running_item_uid) of the last queue drawn
        seen_history = None  # plan_history_uid of the last history read
        reachable = True
        while not self._quit.is_set():
            try:
                st = client.ok("status")
                if (st.get("plan_queue_uid"), st.get("running_item_uid")) != seen:
                    q = client.ok("queue_get")
                    seen = (st.get("plan_queue_uid"), st.get("running_item_uid"))
                    self.queue.emit(q)
                if st.get("plan_history_uid") != seen_history:
                    h = client.ok("history_get")
                    seen_history = h.get("plan_history_uid")
                    self.history.emit(h.get("items") or [])
            except QsError as e:
                if reachable:
                    self.lost.emit(str(e))
                reachable = False
                seen = seen_history = None
                self.status.emit(None)
            else:
                reachable = True
                self.status.emit(st)
            self._quit.wait(self.period)
        client.close()


class Commands(qt.QObject):
    """bl5a-qs requests, one at a time on a worker thread, through the
    vendored RunEngineProxy. `done(label, result)` or `failed(label,
    message)` follows every `submit`."""

    done = qt.Signal(str, object)
    failed = qt.Signal(str, str)

    def __init__(self, address, relay, user, parent=None):
        super().__init__(parent)
        self.client = QsClient(address, 10000)
        self.RE = RunEngineProxy(self.client, relay, user=user, during_wait=time.sleep)
        self._jobs = queue.Queue()
        self._thread = threading.Thread(target=self._run, name="bl5a-gui-commands", daemon=True)
        self._thread.start()

    def submit(self, label, fn, *args, **kwargs):
        self._jobs.put((label, fn, args, kwargs))

    def stop(self):
        self._jobs.put(None)

    def _run(self):
        while True:
            job = self._jobs.get()
            if job is None:
                return
            label, fn, args, kwargs = job
            try:
                result = fn(*args, **kwargs)
            except Exception as e:  # every failure is the operator's to read
                logger.warning("%s: %s", label, e)
                self.failed.emit(label, str(e))
            else:
                self.done.emit(label, result)


class Documents(qt.QObject):
    """bl5a-qs's document stream as `document(name, doc)` signals."""

    document = qt.Signal(str, object)

    def __init__(self, address, parent=None):
        super().__init__(parent)
        self.relay = DocumentRelay(address)
        self.relay.subscribe(self.document.emit)
        self.relay.start()

    def stop(self):
        self.relay.stop()


class Integrator(qt.QObject):
    """bl5a-integrator: requests to its REP on one worker thread
    (`reply(method, reply)`, `failed(method, message)`) and its PUB's
    `row`/`error` messages (`published(topic, body)`)."""

    reply = qt.Signal(str, object)
    failed = qt.Signal(str, str)
    published = qt.Signal(str, object)

    def __init__(self, rep, pub, parent=None):
        super().__init__(parent)
        self.rep = rep
        self.pub = pub
        self._ctx = zmq.Context.instance()
        self._jobs = queue.Queue()
        self._quit = threading.Event()
        threading.Thread(target=self._requests, name="bl5a-gui-integrator", daemon=True).start()
        threading.Thread(target=self._subscribe, name="bl5a-gui-integrator-pub", daemon=True).start()

    def request(self, method, **params):
        self._jobs.put(dict(params, method=method))

    def stop(self):
        self._quit.set()
        self._jobs.put(None)

    def _socket(self):
        s = self._ctx.socket(zmq.REQ)
        s.setsockopt(zmq.LINGER, 0)
        s.setsockopt(zmq.RCVTIMEO, 10000)
        s.setsockopt(zmq.SNDTIMEO, 10000)
        s.connect(self.rep)
        return s

    def _requests(self):
        sock = self._socket()
        while True:
            req = self._jobs.get()
            if req is None:
                break
            method = req["method"]
            try:
                sock.send(json.dumps(req).encode())
                body = json.loads(sock.recv().decode())
            except zmq.ZMQError as e:
                # A REQ socket that lost a reply is stuck; start a new one.
                sock.close(linger=0)
                sock = self._socket()
                self.failed.emit(method, "{} ({})".format(e, self.rep))
                continue
            if isinstance(body, dict) and "error" in body:
                self.failed.emit(method, str(body["error"]))
            else:
                self.reply.emit(method, body)
        sock.close(linger=0)

    def _subscribe(self):
        sock = self._ctx.socket(zmq.SUB)
        sock.setsockopt(zmq.LINGER, 0)
        sock.setsockopt(zmq.RCVTIMEO, 200)
        sock.setsockopt(zmq.SUBSCRIBE, b"")
        sock.connect(self.pub)
        while not self._quit.is_set():
            try:
                frame = sock.recv()
            except zmq.Again:
                continue
            topic, _, body = frame.partition(b" ")
            try:
                self.published.emit(topic.decode(), json.loads(body.decode()))
            except ValueError:
                logger.warning("integrator PUB: undecodable %r", frame[:80])
        sock.close(linger=0)


class PVs(qt.QObject):
    """Channel Access monitors as `changed(name, value, text)` and
    `connected(name, bool)`; `text` is the PV's char_value (an enum's
    state name, a char waveform's string)."""

    changed = qt.Signal(str, object, str)
    connected = qt.Signal(str, bool)

    def __init__(self, parent=None):
        super().__init__(parent)
        self.pvs = {}

    def watch(self, name):
        if name and name not in self.pvs:
            # form="ctrl": with the default "time" form a monitor's
            # char_value of an enum is its index until the state names are
            # fetched some other way.
            self.pvs[name] = PV(name, auto_monitor=True, form="ctrl", connection_timeout=CONNECT_TIMEOUT,
                                callback=self._changed, connection_callback=self._connected)
        return self.pvs.get(name)

    def _changed(self, pvname=None, value=None, char_value=None, **kw):
        self.changed.emit(pvname, value, "" if char_value is None else str(char_value))

    def _connected(self, pvname=None, conn=None, **kw):
        self.connected.emit(pvname, bool(conn))

    def put(self, name, value, callback=None):
        """A put that does not wait; `callback()` (on a helper thread) at
        completion, or once the put has failed, when given."""
        pv = self.watch(name)
        if callback is None:
            try:
                pv.put(value, wait=False, timeout=CONNECT_TIMEOUT)
            except CaError as e:
                logger.warning("%s: put %r: %s", name, value, e)
        else:
            threading.Thread(target=self._put_wait, args=(pv, value, callback), name="put " + name,
                             daemon=True).start()

    @staticmethod
    def _put_wait(pv, value, callback):
        """The PV's connection_timeout bounds the connect; the completion
        waits as long as the record takes."""
        try:
            pv.put(value, wait=True, timeout=None)
        except CaError as e:
            logger.warning("%s: put %r: %s", pv.pvname, value, e)
        finally:
            callback()

    def get_text(self, name, timeout=1.0):
        try:
            return str(self.watch(name).get(as_string=True, timeout=timeout, use_monitor=False))
        except CaError:
            return ""
