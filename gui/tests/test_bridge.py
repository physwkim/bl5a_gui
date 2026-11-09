import pytest
from repics import CaDisconnected

from bl5a_gui.bridge import PVs


class FakePV:
    pvname = "fake"

    def __init__(self, connected=True, fails=None):
        self.connected, self.fails, self.puts = connected, fails, []

    def wait_for_connection(self, timeout):
        return self.connected

    def put(self, value, wait, timeout):
        if self.fails is not None:
            raise self.fails
        self.puts.append((value, wait, timeout))


@pytest.mark.parametrize("pv, puts", [
    (FakePV(), [("wa", True, None)]),
    (FakePV(connected=False), []),
    (FakePV(fails=CaDisconnected("disconnected", 24)), []),
])
def test_put_callback_runs_once_the_put_completes_or_fails(pv, puts):
    called = []
    PVs._put_wait(pv, "wa", lambda: called.append(1))
    assert called == [1] and pv.puts == puts
