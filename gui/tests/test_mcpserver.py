import math
import types

import anyio
import numpy as np
import pytest
from repics import CaDisconnected, CaError, CaTimeout

from bl5a_gui import config, items, mcpserver, sequences
from bl5a_gui.mcpserver import InsituRow, Log, binned, clean


def test_log_since_returns_the_lines_after_n():
    log = Log(keep=10)
    for k in range(5):
        log.add("line {}".format(k + 1))
    got = log.since(3, 100)
    assert got["last"] == 5 and got["skipped"] == 0
    assert [s.split(" ", 1)[1] for s in got["lines"]] == ["line 4", "line 5"]
    assert log.since(5, 100)["lines"] == []


def test_log_since_counts_what_limit_and_keep_left_out():
    log = Log(keep=4)
    for k in range(10):
        log.add(str(k + 1))
    newest = log.since(0, 2)
    assert [s.split(" ", 1)[1] for s in newest["lines"]] == ["9", "10"]
    assert newest["skipped"] == 8  # 6 dropped by keep, 2 by limit
    assert log.since(0, 100)["skipped"] == 6


def test_log_since_past_the_last_line_reads_from_the_start():
    log = Log()
    log.add("a")
    got = log.since(40, 100)  # a reader of a server that restarted
    assert got["last"] == 1 and len(got["lines"]) == 1 and got["skipped"] == 0


def test_clean_makes_json_values():
    got = clean({"a": [np.float32(1.5), math.nan], 2: (np.int64(3), np.array([1.0, np.inf]))})
    assert got == {"a": [1.5, None], "2": [3, [1.0, None]]}
    assert type(got["2"][0]) is int


def test_binned_averages_runs_and_skips_nan_bins():
    q = np.arange(10.0)
    row = np.array([1, 3, np.nan, np.nan, 5, np.nan, 0, 2, 4, 4], float)
    qb, (rb,) = binned(q, [row], 5)
    assert list(qb) == [0.5, 2.5, 4.5, 6.5, 8.5]
    assert rb[0] == 2 and math.isnan(rb[1]) and rb[2] == 5 and rb[3] == 1 and rb[4] == 4


def test_binned_drops_the_short_tail_and_keeps_short_axes():
    qb, (rb,) = binned(np.arange(7.0), [np.ones(7)], 3)  # runs of 3: the 7th bin drops
    assert list(qb) == [1.0, 4.0] and list(rb) == [1.0, 1.0]
    qb, (rb,) = binned([1.0, 2.0], [[3.0, 4.0]], 200)
    assert list(qb) == [1.0, 2.0] and list(rb) == [3.0, 4.0]


def test_insitu_row_is_the_tables_row():
    assert set(InsituRow.model_fields) == set(items.NEW_ROW)
    defaults = InsituRow().model_dump()
    assert defaults == dict(items.NEW_ROW, th_list=list(items.NEW_ROW["th_list"]))


def test_insitu_row_queues_what_the_table_queues():
    rows = [InsituRow(T=150, ramp=5, wait=True, name="a"), InsituRow(th_list=[], align=False, soak_n=10, soak_t=1)]
    sent = [r.model_dump(exclude_unset=True) for r in rows]
    table = [dict(items.NEW_ROW, T=150, ramp=5, wait=True, name="a"),
             dict(items.NEW_ROW, th_list=[], align=False, soak_n=10, soak_t=1)]
    seq = ("Thin film", sequences.get("Thin film"))
    assert items.insitu_items(sent, "s", seq) == items.insitu_items(table, "s", seq)


def test_insitu_row_refuses_unknown_columns():
    with pytest.raises(ValueError):
        InsituRow(temperature=150)


def test_tools_and_their_arguments():
    bl = mcpserver.Beamline("tcp://127.0.0.1:1", "tcp://127.0.0.1:2", "tcp://127.0.0.1:3", "tcp://127.0.0.1:4",
                            "test")
    tools = {t.name: t for t in anyio.run(mcpserver.build(bl).list_tools)}
    assert set(tools) == {
        "status", "queue", "history", "motors", "scans", "scan", "alignment", "messages", "spec_log", "commands",
        "integration", "stacks", "stack", "frame_image", "spec_result", "alignment_sequences", "alignment_sequence",
        "save_alignment_sequence", "queue_alignment", "queue_scan", "queue_mar_series",
        "queue_insitu", "queue_remove", "queue_move", "queue_clear", "queue_start", "queue_stop",
        "queue_autostart", "environment_open", "environment_close", "pause", "resume", "stop", "abort",
        "stop_after_frame", "tweak", "spec_command", "spec_abort", "stop_all", "integration_set"}
    mar = tools["queue_mar_series"].input_schema
    assert set(mar["required"]) == {"name", "texpo", "beamstop_in_place"}
    assert tools["queue_alignment"].input_schema["required"] == ["sequence"]
    assert set(tools["queue_scan"].input_schema["required"]) == {"motor", "start", "finish", "intervals", "time"}
    assert set(tools["save_alignment_sequence"].input_schema["required"]) == {"name", "blocks"}
    assert tools["alignment_sequences"].annotations.read_only_hint
    assert tools["messages"].description.startswith("What the GUI's Messages tab shows")
    assert set(tools["messages"].input_schema["properties"]) == {"since", "limit"}
    assert tools["status"].annotations.read_only_hint
    assert tools["abort"].annotations.destructive_hint
    assert tools["stop_all"].annotations.destructive_hint


class FakePV:
    """repics.pv.PV as Beamline uses it: a put returns nothing and calls
    back once done, a failure to send raises `fails`; the server fails a
    sent put with `refused`, which only a put with wait=True raises."""

    def __init__(self, value=None, connected=True, fails=None, refused=None):
        self.pvname = "fake"
        self.value, self.connected, self.fails, self.refused, self.puts = value, connected, fails, refused, []

    def put(self, value, wait=False, timeout=30.0, callback=None, callback_data=None):
        if self.fails is not None:
            raise self.fails
        self.puts.append(value)
        if callback is not None:
            callback(pvname=self.pvname, data=callback_data)
        if wait and self.refused is not None:
            raise self.refused

    def get(self, timeout, use_monitor, as_string=False):
        if self.fails is not None:
            raise self.fails
        return self.value


def beamline(qs):
    """A Beamline with SPEC free and qs reporting `qs` with its environment open."""
    bl = mcpserver.Beamline("tcp://127.0.0.1:1", "tcp://127.0.0.1:2", "tcp://127.0.0.1:3", "tcp://127.0.0.1:4",
                            "test")
    bl.sync = lambda: None
    bl.qs = None if qs is None else {"worker_environment_exists": True, **qs}
    bl.spec = {"Connected": "Connected", "Busy": "Ready", "Cmd:Busy": "Idle"}
    return bl


def stop_all(qs, **abort):
    """Beamline.stop_all with SPEC free, qs reporting `qs` and Cmd:Abort a
    FakePV(**abort)."""
    bl = beamline(qs)
    bl.after_frame.asked = True
    bl.pvs[config.SPEC + "Cmd:Abort"] = pv = FakePV(**abort)
    aborts = []
    bl.RE = types.SimpleNamespace(abort=lambda: aborts.append(1))
    return bl, bl.stop_all(), pv.puts, aborts


def test_stop_all_sends_spec_ctrl_c_whatever_qs_does():
    for qs in ({"re_state": "idle"}, None):
        _, got, puts, aborts = stop_all(qs)
        assert got == {"spec": "^C sent", "plan": "none running or paused"}, qs
        assert puts == [1] and aborts == [], qs


def test_stop_all_aborts_a_running_or_paused_plan():
    for state in ("executing_queue", "paused"):
        bl, got, puts, aborts = stop_all({"re_state": state, "running_item_name": "mar_series"})
        assert got == {"spec": "^C sent", "plan": "abort requested"}, state
        assert puts == [1] and aborts == [1], state
        assert not bl.after_frame.asked, state


@pytest.mark.parametrize("abort", [{"connected": False}, {"fails": CaDisconnected("disconnected", 24)}])
def test_stop_all_without_cmd_abort_still_aborts_and_says_so(abort):
    bl, got, puts, aborts = stop_all({"re_state": "executing_queue"}, **abort)
    assert puts == [] and aborts == [1]
    assert "SPEC got no ^C" in got["spec"] and got["plan"] == "abort requested"
    assert "SPEC got no ^C" in bl.messages.since(0, 10)["lines"][-1]


def tweak(msta, **failing):
    """Beamline.tweak of tth with SPEC free, the record's MSTA `msta` after;
    `failing` maps a field to the FakePV kwargs it fails with."""
    bl = beamline({"re_state": "idle"})
    base = config.SPEC + "tth"
    pvs = {f: FakePV(v, **failing.get(f, {})) for f, v in ((".TWV", None), (".TWF", None), (".MSTA", msta),
                                                           (".RBV", 81.0))}
    bl.pvs.update({base + f: pv for f, pv in pvs.items()})
    return bl, pvs


def test_tweak_returns_the_readback_of_an_accepted_move():
    bl, pvs = tweak(0x2)
    got = bl.tweak("tth", 0.5, "forward")
    assert got["rbv"] == 81.0 and pvs[".TWV"].puts == [0.5] and pvs[".TWF"].puts == [1]
    assert bl.spec_job is None


def test_tweak_of_a_refused_move_is_an_error():
    bl, _ = tweak(0x2 | mcpserver.MSTA_PROBLEM)
    with pytest.raises(mcpserver.ToolError, match="SPEC refused the tth move"):
        bl.tweak("tth", 0.5, "forward")
    assert bl.spec_job is None


@pytest.mark.parametrize("field, fails, error", [
    (".TWV", {"fails": CaTimeout("channel not found: tth.TWV", 80)}, r"tth\.TWV: channel not found"),
    (".TWF", {"fails": CaTimeout("timed out after 300 s", 80)}, r"tth\.TWF: timed out after 300 s$"),
    (".MSTA", {"fails": CaTimeout("timed out after 2 s", 80)}, r"tth\.MSTA: timed out after 2 s$"),
])
def test_tweak_whose_channel_fails_is_an_error(field, fails, error):
    bl, _ = tweak(0x2, **{field: fails})
    with pytest.raises(mcpserver.ToolError, match=error):
        bl.tweak("tth", 0.5, "forward")
    assert bl.spec_job is None


def test_spec_command_ends_once_its_put_does():
    bl = beamline({"re_state": "idle"})
    bl.pvs[config.SPEC_CMD] = cmd = FakePV()
    for s in ("Cmd:Status", "Cmd:Reply"):
        bl.pvs[config.SPEC + s] = FakePV(fails=CaDisconnected("disconnected", 24))
    got = bl.spec_command("wa", 5.0)
    assert cmd.puts == ["wa"]
    assert got["cmd"] == "wa" and got["status"] == "no status" and bl.spec_job is None


def test_spec_command_puts_a_line_past_39_characters_whole():
    # To Cmd.VAL$, which repics's PV.put sends a str to as chars.
    bl = beamline({"re_state": "idle"})
    bl.pvs[config.SPEC_CMD] = cmd = FakePV()
    for s in ("Cmd:Status", "Cmd:Reply"):
        bl.pvs[config.SPEC + s] = FakePV("")
    line = "wm martran marsdd marz marshut marx ipbsx ipbsy"
    bl.spec_command(line, 5.0)
    assert cmd.puts == [line]


def test_spec_command_whose_put_the_server_fails_is_an_error_and_logged(caplog):
    # Cmd:Status is then the previous command's.
    bl = beamline({"re_state": "idle"})
    bl.pvs[config.SPEC_CMD] = FakePV(refused=CaError("Put operation failed", 160))
    for s in ("Cmd:Status", "Cmd:Reply"):
        bl.pvs[config.SPEC + s] = FakePV("OK")
    with pytest.raises(mcpserver.ToolError, match=r"Cmd\.VAL\$: Put operation failed$"):
        bl.spec_command("wa", 5.0)
    assert bl.spec_job is None
    assert "'wa'" in caplog.text and "Put operation failed" in caplog.text
    got = bl.command_result()
    assert got["error"].endswith("Put operation failed") and "status" not in got


def test_spec_command_to_an_unconnected_cmd_is_an_error():
    bl = beamline({"re_state": "idle"})
    bl.pvs[config.SPEC_CMD] = FakePV(fails=CaTimeout("channel not found: SPEC:Cmd.VAL$", 80))
    with pytest.raises(mcpserver.ToolError, match=r"Cmd\.VAL\$: channel not found"):
        bl.spec_command("wa", 5.0)
    assert bl.spec_job is None
