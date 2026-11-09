import pytest
from repics import CaDisconnected, CaTimeout

from bl5a_gui.bridge import PVs


class FakePV:
    pvname = "fake"

    def __init__(self, fails=None):
        self.fails, self.puts = fails, []

    def put(self, value, wait, timeout):
        if self.fails is not None:
            raise self.fails
        self.puts.append((value, wait, timeout))


@pytest.mark.parametrize("pv, puts, error", [
    (FakePV(), [("wa", True, None)], None),
    (FakePV(fails=CaTimeout("channel not found: fake", 80)), [], "channel not found: fake"),
    (FakePV(fails=CaDisconnected("disconnected", 24)), [], "disconnected"),
])
def test_put_callback_runs_once_the_put_completes_or_fails(pv, puts, error):
    called = []
    PVs._put_wait(pv, "wa", called.append)
    assert called == [error] and pv.puts == puts
