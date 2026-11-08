"""Stop all: SPEC's ^C in every state, and an abort of the plan when one
runs or is paused."""

import types
import unittest
from unittest import mock

from bl5a_gui import gate, main

SPEC_FREE = {"Connected": "Connected", "Busy": "Ready", "Cmd:Busy": "Idle"}


class PVs:
    def __init__(self, connected):
        self.pv = types.SimpleNamespace(connected=connected)
        self.puts = []

    def watch(self, name):
        return self.pv

    def put(self, name, value):
        self.puts.append((name, value))


def window(qs, connected=True, after_frame=False):
    w = types.SimpleNamespace(qs=qs, spec=dict(SPEC_FREE), afterFrame=gate.StopAfterFrame(), pvs=PVs(connected),
                              runs=[], lines=[], shown=0)
    w.afterFrame.asked = after_frame
    w.RE = types.SimpleNamespace(abort=object())
    w.run = lambda label, fn: w.runs.append((label, fn))
    w.messages = types.SimpleNamespace(add=w.lines.append)
    w._show = lambda: setattr(w, "shown", w.shown + 1)
    w._abortPlan = lambda: main.MainWindow._abortPlan(w)
    return w


def stop_all(w):
    with mock.patch.object(main.qt.QMessageBox, "warning") as warning:
        main.MainWindow._stopAll(w)
    return warning


class StopAllTest(unittest.TestCase):
    def test_idle_or_silent_qs_gets_spec_only(self):
        for qs in ({"re_state": "idle"}, None):
            w = window(qs)
            stop_all(w).assert_not_called()
            self.assertEqual(w.pvs.puts, [(main.STOP_ALL, 1)], qs)
            self.assertEqual(w.runs, [], qs)

    def test_running_or_paused_plan_is_aborted_too(self):
        for state in ("executing_queue", "paused"):
            w = window({"re_state": state, "running_item_name": "mar_series"}, after_frame=True)
            stop_all(w).assert_not_called()
            self.assertEqual(w.pvs.puts, [(main.STOP_ALL, 1)], state)
            self.assertEqual(w.runs, [("abort", w.RE.abort)], state)
            self.assertFalse(w.afterFrame.asked, state)

    def test_disconnected_spec_warns_and_still_aborts(self):
        w = window({"re_state": "executing_queue"}, connected=False)
        stop_all(w).assert_called_once()
        self.assertEqual(w.pvs.puts, [])
        self.assertEqual(w.runs, [("abort", w.RE.abort)])
        self.assertIn("SPEC got no ^C", w.lines[-1])


if __name__ == "__main__":
    unittest.main()
