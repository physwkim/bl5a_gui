"""The forms' items and checks, against qs's rules (qs/src/plans) and the
macro headers'."""

import math
import re
import unittest

from bl5a_gui import items, sequences
from bl5a_gui.items import Invalid

FILM = sequences.get("Thin film")
COIN = sequences.get("Coin cell")
SEQ = ("Thin film", FILM)


class FileNameTest(unittest.TestCase):
    def test_qs_rule(self):
        # The same cases as qs's file_names_mar345expose_takes.
        self.assertEqual(items.file_name("in-situ_Neat_ITIC-Th_CF_240"), "in-situ_Neat_ITIC-Th_CF_240")
        items.file_name("a" * 27)
        for bad in ["", "a b", 'a"b', "a'b", "a" * 28, "a\tb", None]:
            with self.assertRaises(Invalid, msg=repr(bad)):
                items.file_name(bad)

    def test_bytes_not_characters(self):
        # qs counts bytes (Rust str::len).
        items.file_name("é" * 13)
        with self.assertRaises(Invalid):
            items.file_name("é" * 14)


class MarSeriesTest(unittest.TestCase):
    def test_single_shot(self):
        item = items.mar_series("ZnFC_Pcell_1Vdelithi", 3)
        self.assertEqual(item, {"item_type": "plan", "name": "mar_series", "args": [],
                                "kwargs": {"name": "ZnFC_Pcell_1Vdelithi", "texpo": 3.0, "mode": "static",
                                           "n": 1, "md": {"series": "ZnFC_Pcell_1Vdelithi"}}})

    def test_repeat(self):
        kw = items.mar_series("AgFC", -2e5, "rocking1", n=40, ti=3, series="cell_7")["kwargs"]
        self.assertEqual((kw["n"], kw["ti"], kw["texpo"], kw["mode"], kw["md"]),
                         (40, 3.0, -2e5, "rocking1", {"series": "cell_7"}))

    def test_macro_limits(self):
        for texpo in (0, 0.5, 20.5, math.nan, math.inf, "x"):
            with self.assertRaises(Invalid, msg=texpo):
                items.mar_series("f", texpo)
        items.mar_series("f", 1)
        items.mar_series("f", 20)
        items.mar_series("f", -1)
        for ti in (2.5, 0, -1):
            with self.assertRaises(Invalid, msg=ti):
                items.mar_series("f", 2, n=3, ti=ti)
        with self.assertRaises(Invalid):
            items.mar_series("f", 2, n=3)  # no period
        for n in (0, 1.5, True, -1):
            with self.assertRaises(Invalid, msg=n):
                items.mar_series("f", 2, n=n, ti=3)
        with self.assertRaises(Invalid):
            items.mar_series("f", 2, mode="tall")


class InsituTest(unittest.TestCase):
    def test_defaults(self):
        kw = items.insitu_step("film_t1", series="film-t1")["kwargs"]
        self.assertEqual(kw, {"th_list": [-0.05, 0.11, 0.13, 0.15], "texpo": -7e4,
                              "name": "film_t1", "md": {"series": "film-t1"}})

    def test_the_sequence_goes_with_its_name(self):
        kw = items.insitu_step("f", blocks=FILM, sequence="Thin film")["kwargs"]
        self.assertEqual((kw["blocks"], kw["sequence"]), (FILM, "Thin film"))
        with self.assertRaisesRegex(Invalid, "no blocks"):
            items.insitu_step("f", blocks=[])

    def test_name_needed_only_with_frames(self):
        with self.assertRaises(Invalid):
            items.insitu_step(None)
        kw = items.insitu_step("ignored", blocks=FILM, th_list=[])["kwargs"]
        self.assertNotIn("name", kw)
        with self.assertRaisesRegex(Invalid, "does nothing"):
            items.insitu_step(None, th_list=[])

    def test_temperature(self):
        kw = items.insitu_step("f", T=120, ramp=5, wait=True, soak=(200, 1.0))["kwargs"]
        self.assertEqual((kw["T"], kw["ramp"], kw["wait"], kw["soak"]), (120.0, 5.0, True, [200, 1.0]))
        self.assertNotIn("tolerance", kw)
        kw = items.insitu_step("f", T=30, wait=True, tolerance="0.5")["kwargs"]
        self.assertEqual(kw["tolerance"], 0.5)
        for bad in ({"wait": True}, {"soak": (0, 1)}, {"soak": (2, 0)}, {"ramp": 0},
                    {"T": 30, "tolerance": 0.5}, {"T": 30, "wait": True, "tolerance": 0}):
            with self.assertRaises(Invalid, msg=bad):
                items.insitu_step("f", **bad)

    def test_cooling_row(self):
        kw = items.insitu_row(items.cooling_row(), None)["kwargs"]
        self.assertEqual(kw, {"th_list": [], "texpo": -7e4, "T": 25.0, "ramp": 25.0, "soak": [3600, 1.0]})

    def test_numbers(self):
        self.assertEqual(items.numbers("-0.05, 0.11 0.13,0.15", "th"), [-0.05, 0.11, 0.13, 0.15])
        with self.assertRaises(Invalid):
            items.numbers("0.1, x", "th")


class InsituTableTest(unittest.TestCase):
    """insitu_items: the table the form and the MCP server both send."""

    def test_columns_are_the_forms(self):
        from bl5a_gui import forms
        self.assertEqual(list(items.NEW_ROW), forms.KEYS)

    def test_a_new_row_as_the_form_sends_it(self):
        row = dict(items.NEW_ROW, th_list="-0.05, 0.11", texpo="-7e4", tolerance="0.3", name="f1")
        kw = items.insitu_items([row], "", SEQ)[0]["kwargs"]
        self.assertEqual(kw, {"blocks": FILM, "sequence": "Thin film", "th_list": [-0.05, 0.11], "texpo": -7e4,
                              "name": "f1", "md": {"series": "f1"}})

    def test_align_needs_a_sequence(self):
        with self.assertRaisesRegex(Invalid, "row 1: align: no alignment sequence"):
            items.insitu_items([{"name": "f1"}], "s")
        kw = items.insitu_items([{"name": "f1", "align": False}], "s")[0]["kwargs"]
        self.assertFalse({"blocks", "sequence"} & set(kw))

    def test_left_out_keys_take_the_new_rows(self):
        self.assertEqual(items.insitu_items([{"name": "f1"}], "s", SEQ),
                         [items.insitu_step("f1", blocks=FILM, sequence="Thin film", series="s")])

    def test_blank_cells_are_empty(self):
        kw = items.insitu_row({"name": "f1", "T": " ", "ramp": "", "soak_n": "", "soak_t": None}, None,
                              SEQ)["kwargs"]
        self.assertFalse({"T", "ramp", "soak"} & set(kw))

    def test_tolerance_counts_only_with_wait(self):
        kw = items.insitu_row({"name": "f", "T": 30, "tolerance": 0.5}, None, SEQ)["kwargs"]
        self.assertNotIn("tolerance", kw)
        kw = items.insitu_row({"name": "f", "T": 30, "wait": True, "tolerance": "0.5"}, None, SEQ)["kwargs"]
        self.assertEqual(kw["tolerance"], 0.5)

    def test_series(self):
        rows = [{"align": False, "th_list": [], "T": 30, "name": ""}, {"name": "f2"}]
        self.assertEqual([i["kwargs"]["md"] for i in items.insitu_items(rows, None, SEQ)], [{"series": "f2"}] * 2)
        self.assertEqual(items.insitu_items(rows, " s ", SEQ)[0]["kwargs"]["md"], {"series": "s"})
        with self.assertRaisesRegex(Invalid, "no row has a name"):
            items.insitu_items([{"align": False, "th_list": [], "T": 30}])
        with self.assertRaisesRegex(Invalid, "no rows"):
            items.insitu_items([], "s")

    def test_the_first_bad_row_is_named(self):
        for row, message in (({"name": "f", "soak_n": 10}, "row 2: soak: both"),
                             ({"name": "f", "colour": 1}, "row 2: unknown column.s. colour"),
                             ({"name": "f", "th_list": "0.1, x"}, "row 2: th list"),
                             ({"name": "f", "texpo": ""}, "row 2: texpo")):
            with self.assertRaisesRegex(Invalid, message):
                items.insitu_items([{"name": "f"}, row, {"name": ""}], "s", SEQ)


class BlockTest(unittest.TestCase):
    """The checker mirrors qs/src/plans/blocks.rs's; its errors read the same."""

    SCAN = {"kind": "scan", "type": "cscan", "motor": "sy", "start": -1, "finish": 1, "time": 5, "sleep": 0.1,
            "counter": "ic1", "pick": "COM"}

    # Counted to monitor counts on SPEC's MON, which only a step scan does.
    STEP = {"kind": "scan", "type": "ascan", "motor": "th", "start": -1, "finish": 1, "intervals": 20,
            "monitor": 10000, "settle": 0.1, "counter": "ic1", "pick": "CEN"}

    def with_(self, key, value):
        return dict(self.SCAN, **{key: value})

    def test_each_kind(self):
        for b in (self.SCAN, self.with_("dip", True), self.with_("dip", False), dict(self.SCAN, pick="EDGE", dip=False),
                  {"kind": "scan", "type": "lup", "motor": "th", "start": -0.6, "finish": 0.6, "intervals": 30,
                   "time": 0.5, "settle": 0.1, "counter": "det", "pick": "CEN", "set": 0, "note": "half-cut"},
                  self.STEP, dict(self.STEP, monitor=10000.0),
                  {"kind": "move", "moves": [["tth", 1], ["th", 0]]},
                  {"kind": "spec", "command": " atte 0 "},
                  {"kind": "wait", "seconds": 3},
                  {"kind": "erase", "when": {"motor": "martran", "op": ">", "value": 1}},
                  {"kind": "loop", "times": 3, "blocks": [
                      {"kind": "wait", "seconds": 1},
                      {"kind": "loop", "times": 2, "blocks": [{"kind": "erase"}]}]}):
            self.assertIs(items.block("block 1", b), b)

    def test_errors_name_the_block_and_field(self):
        cases = [
            (self.with_("motor", "zz"), "block 1 (scan): motor must be one of sy, shold"),
            (self.with_("time", 0), "block 1 (scan): time must be seconds > 0, got 0"),
            (self.with_("intervals", 10), "block 1 (scan): unknown field intervals"),
            (self.with_("monitor", 1000), "block 1 (scan): monitor is for ascan and lup; a cscan counts for time"),
            (dict(self.STEP, time=0.5), "block 1 (scan): time and monitor both given; a point counts to one"),
            ({k: v for k, v in self.STEP.items() if k != "monitor"}, "block 1 (scan): time or monitor is missing"),
            (dict(self.STEP, monitor=0.5), "monitor must be a whole number of counts >= 1, got 0.5"),
            (self.with_("counter", "sec"), "counter must be one of"),
            (self.with_("pick", "cen"), "pick must be one of"),
            (self.with_("dip", 1), "dip must be true or false, got 1"),
            (dict(self.SCAN, pick="EDGE", dip=True), "block 1 (scan): dip is for CEN, COM and xMAX, not EDGE"),
            (self.with_("set", "0"), "set must be a number"),
            ({"kind": "move", "moves": []}, "moves must be"),
            ({"kind": "move", "moves": [["th", 1], ["th", 2]]}, "th twice"),
            ({"kind": "move", "moves": [["th"]]}, "[motor, position]"),
            ({"kind": "spec", "command": "a\nb"}, "one line"),
            ({"kind": "spec", "command": "  "}, "one line"),
            ({"kind": "wait", "seconds": 0}, "seconds > 0"),
            ({"kind": "erase", "when": {"motor": "th", "op": "=", "value": 1}}, "op must be"),
            ({"kind": "erase", "when": {"motor": "th", "op": "<"}}, "when: value is missing"),
            ({"kind": "erase", "note": 3}, "note must be a string"),
            ("erase", "must be an object"),
            ({"kind": "loop", "times": 0, "blocks": [{"kind": "erase"}]}, "times must be a whole number >= 1, got 0"),
            ({"kind": "loop", "times": 1.5, "blocks": [{"kind": "erase"}]}, "a whole number >= 1"),
            ({"kind": "loop", "times": 2, "blocks": []}, "blocks must be a list of blocks"),
            ({"kind": "loop", "times": 2}, "blocks is missing"),
            # A block inside a loop is named by its place in each, as qs names it.
            ({"kind": "loop", "times": 2, "blocks": [
                {"kind": "erase"}, {"kind": "loop", "times": 2, "blocks": [{"kind": "wait"}]}]},
             "block 1 (loop) block 2 (loop) block 1 (wait): seconds is missing"),
        ]
        for b, why in cases:
            with self.subTest(b=b), self.assertRaisesRegex(Invalid, re.escape(why)):
                items.block("block 1", b)

    def test_sequence(self):
        with self.assertRaisesRegex(Invalid, "^block 2: kind must be one of"):
            items.sequence_blocks([{"kind": "erase"}, {"kind": "jump"}])
        with self.assertRaisesRegex(Invalid, "blocks must be a list"):
            items.sequence_blocks({"kind": "erase"})
        with self.assertRaisesRegex(Invalid, "no blocks"):
            items.align([])

    def test_templates_are_the_macros(self):
        self.assertEqual((len(COIN), len(FILM)), (11, 30))
        self.assertEqual(items.align(COIN, "Coin cell")["kwargs"], {"blocks": COIN, "sequence": "Coin cell"})


class DescribeTest(unittest.TestCase):
    def test_cells(self):
        item = items.insitu_step("film_t1", blocks=FILM, sequence="Thin film", series="film-t1")
        item["kwargs"]["md"]["user"] = "bl5a"
        self.assertEqual(items.describe(item),
                         ("insitu_step", "film_t1",
                          "sequence='Thin film' texpo=-70000 th_list=-0.05/0.11/0.13/0.15", "film-t1", "bl5a"))
        self.assertEqual(items.describe(items.align(COIN, "mine")), ("align", "", "sequence=mine", "align", ""))
        self.assertEqual(items.describe(items.align(COIN))[2], "blocks=11")
        self.assertEqual(items.describe({"name": "align"}), ("align", "", "", "align", ""))


class FolderTest(unittest.TestCase):
    """Where qs puts an item's data (qs/src/place.rs), as the queue shows it."""

    def test_series_names_one_folder(self):
        # qs's a_series_names_one_folder, bounded here by the series alone:
        # qs bounds the whole folder, whose root the GUI may not know.
        for bad in [".", "..", "a/b", "a\0b", "cell 7", "cell\t7", "cell\u30007", "x" * 201, "é" * 101]:
            with self.assertRaises(Invalid, msg=repr(bad)):
                items.series_md(bad)
        for good in ["x" * 200, "é" * 100]:
            self.assertEqual(items.series_md(good), {"series": good})
        self.assertEqual(items.series_md("  "), {})
        with self.assertRaises(Invalid):
            items.align(COIN, "Coin cell", "a/b")
        with self.assertRaises(Invalid):
            # mar_series's series defaults to the file name, which may hold '/'.
            items.mar_series("a/b", 2)

    def test_the_root_is_stamped(self):
        item = items.mar_series("f", 2, series="cell_7")
        placed = items.placed(item, " /data/u1 ")
        self.assertEqual(placed["kwargs"]["md"], {"series": "cell_7", "data_root": "/data/u1"})
        self.assertNotIn("data_root", item["kwargs"]["md"], "the item queued is a copy")
        self.assertEqual(items.folder(placed), "/data/u1/cell_7")
        self.assertIs(items.placed(item, ""), item)
        self.assertEqual(items.folder(item), "cell_7")
        bare = items.placed(items.align(FILM), "/d")
        self.assertEqual(bare["kwargs"]["md"], {"data_root": "/d"})
        self.assertEqual(items.folder(bare), "/d/align")
        for bad in ["rel/dir", "~/data", "/data/u 1", "/data/u\t1"]:
            with self.assertRaises(Invalid, msg=bad):
                items.placed(item, bad)
        # "/d/" and the series: DIR_MAX bytes.
        items.placed(items.align(FILM, series="x" * 197), "/d")
        with self.assertRaises(Invalid):
            items.placed(items.align(FILM, series="x" * 198), "/d")


def archived(uid, name, **result):
    return {"item_type": "plan", "name": name, "item_uid": uid, "result": result}


class FinishedTest(unittest.TestCase):
    """bl5a-qs's history as `history_get` returns it: each item with the
    result the server archived (qs/server.rs)."""

    def test_first_read_is_the_baseline(self):
        f = items.Finished()
        self.assertEqual(f.new([archived("a", "align", exit_status="success")]), [])

    def test_only_items_not_read_before(self):
        f = items.Finished()
        a = archived("a", "align", exit_status="success")
        b = archived("b", "mar_series", exit_status="fail", reason="spec: refused")
        f.new([a])
        self.assertEqual(f.new([a, b]), [b])
        self.assertEqual(f.new([a, b]), [])

    def test_cleared_history(self):
        f = items.Finished()
        f.new([archived("a", "align", exit_status="success")])
        self.assertEqual(f.new([]), [])
        c = archived("c", "align", exit_status="abort")
        self.assertEqual(f.new([c]), [c])

    def test_ended_line(self):
        self.assertEqual(items.ended(archived("a", "align", exit_status="success", run_uid="r", reason=None)),
                         "align ended: success")
        # An item that failed before its plan opened a run has no run_uid.
        self.assertEqual(items.ended(archived("b", "mar_series", exit_status="fail",
                                              reason="plan build failed: texpo")),
                         "mar_series ended: fail: plan build failed: texpo")


class StepScanTest(unittest.TestCase):
    def test_lup(self):
        item = items.step_scan("th", "-0.1", 0.1, 4, "0.1", relative=True)
        self.assertEqual(item, {"item_type": "plan", "name": "step_scan", "args": [],
                                "kwargs": {"motor": "th", "start": -0.1, "finish": 0.1, "intervals": 4,
                                           "time": 0.1, "relative": True, "counter": "det"}})
        self.assertEqual(items.describe(item)[:3], ("step_scan", "",
                                                    "motor=th start=-0.1 finish=0.1 intervals=4 time=0.1 "
                                                    "relative=True counter=det"))

    def test_ascan_in_a_series(self):
        item = items.step_scan("shold", 1, -1, 10, 0.5, counter="ic1", series="cell1")
        self.assertEqual(item["kwargs"]["md"], {"series": "cell1"})
        self.assertFalse(item["kwargs"]["relative"])
        self.assertEqual(items.folder(item), "cell1")

    def test_qs_rules(self):
        # The same refusals as qs's refuses_what_the_scan_cannot_take.
        for args, kw in [(("chi", 0, 1, 2, 1), {}), (("th", 0, 1, 2, 1), {"counter": "pd"}),
                         (("th", 0, 1, 2, 1), {"counter": "sec"}), (("th", 0, 1, 0, 1), {}),
                         (("th", 0, 1, 2.5, 1), {}), (("th", 0, 1, 2, 0), {}), (("th", 0, 1, 2, -1), {}),
                         (("th", "x", 1, 2, 1), {}), (("th", 0, math.inf, 2, 1), {})]:
            with self.assertRaises(Invalid, msg=repr((args, kw))):
                items.step_scan(*args, **kw)


if __name__ == "__main__":
    unittest.main()
