"""gate: what the GUI's buttons and the MCP tools allow, and stop after
frame's way from the deferred pause to the stop."""

import unittest

from bl5a_gui import gate

SPEC_FREE = {"Connected": "Connected", "Busy": "Ready", "Cmd:Busy": "Idle"}


def qs(state, running=None, uid=None):
    return {"re_state": state, "running_item_name": running, "running_item_uid": uid,
            "worker_environment_exists": True}


# bsrs's status with no worker environment: its re_state is the text "null".
CLOSED = {"re_state": "null", "manager_state": "environment_closed", "running_item_name": None,
          "running_item_uid": None, "worker_environment_exists": False}


def opened(st, spec=SPEC_FREE, after_frame=False):
    return {k for k, v in gate.allowed(st, spec, after_frame).items() if v}


class AllowedTest(unittest.TestCase):
    def test_qs_silent(self):
        self.assertEqual(opened(None), set())

    def test_environment_closed(self):
        self.assertIsNone(gate.re_state(CLOSED))
        self.assertEqual(opened(CLOSED), set())

    def test_spec_free_only_when_idle_and_ready(self):
        self.assertEqual(opened(qs("idle")), {"spec"})
        self.assertEqual(opened(qs("idle", uid="u")), set())
        for name, busy in (("Connected", "Disconnected"), ("Busy", "Busy"), ("Cmd:Busy", "Busy"),
                           ("Connected", None)):
            self.assertEqual(opened(qs("idle"), dict(SPEC_FREE, **{name: busy})), set(), name)

    def test_running(self):
        self.assertEqual(opened(qs("executing_queue", "mar_series", "u")),
                         {"pause", "stop", "abort", "stop_after_frame"})
        self.assertEqual(opened(qs("executing_queue", "mar_series", "u"), after_frame=True),
                         {"pause", "stop", "abort"})
        self.assertEqual(opened(qs("executing_queue", "align", "u")), {"pause", "stop", "abort"})
        self.assertEqual(opened(qs("paused", "mar_series", "u")), {"resume", "stop", "abort"})


class StopAfterFrameTest(unittest.TestCase):
    def test_not_asked_is_never_due(self):
        f = gate.StopAfterFrame()
        self.assertFalse(f.status(qs("paused")))

    def test_due_once_when_paused(self):
        f = gate.StopAfterFrame()
        f.asked = True
        self.assertFalse(f.status(qs("executing_queue", "mar_series", "u")))
        self.assertTrue(f.asked)
        self.assertTrue(f.status(qs("paused", "mar_series", "u")))
        self.assertFalse(f.asked)
        self.assertFalse(f.status(qs("paused", "mar_series", "u")))

    def test_idle_or_silent_drops_it(self):
        for st in (qs("idle"), CLOSED, None):
            f = gate.StopAfterFrame()
            f.asked = True
            self.assertFalse(f.status(st))
            self.assertFalse(f.asked)


if __name__ == "__main__":
    unittest.main()
