import math

import anyio
import numpy as np
import pytest

from bl5a_gui import items, mcpserver
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
    assert items.insitu_items(sent, "s") == items.insitu_items(table, "s")


def test_insitu_row_refuses_unknown_columns():
    with pytest.raises(ValueError):
        InsituRow(temperature=150)


def test_tools_and_their_arguments():
    bl = mcpserver.Beamline("tcp://127.0.0.1:1", "tcp://127.0.0.1:2", "tcp://127.0.0.1:3", "tcp://127.0.0.1:4",
                            "test")
    tools = {t.name: t for t in anyio.run(mcpserver.build(bl).list_tools)}
    assert set(tools) == {
        "status", "queue", "history", "motors", "scans", "scan", "alignment", "messages", "spec_log", "commands",
        "integration", "stacks", "stack", "frame_image", "spec_result", "queue_alignment", "queue_mar_series",
        "queue_insitu", "queue_remove", "queue_move", "queue_clear", "queue_start", "queue_stop",
        "queue_autostart", "environment_open", "environment_close", "pause", "resume", "stop", "abort",
        "stop_after_frame", "tweak", "spec_command", "spec_abort", "integration_set"}
    mar = tools["queue_mar_series"].input_schema
    assert set(mar["required"]) == {"name", "texpo", "beamstop_in_place"}
    assert tools["messages"].description.startswith("What the GUI's Messages tab shows")
    assert set(tools["messages"].input_schema["properties"]) == {"since", "limit"}
    assert tools["status"].annotations.read_only_hint
    assert tools["abort"].annotations.destructive_hint
