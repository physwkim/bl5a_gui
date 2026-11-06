"""The forms' items and checks, against qs's rules (qs/src/plans) and the
macro headers'."""

import math
import unittest

from bl5a_gui import items
from bl5a_gui.items import Invalid


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
                                "kwargs": {"name": "ZnFC_Pcell_1Vdelithi", "texpo": 3.0, "geometry": "standard",
                                           "n": 1, "md": {"series": "ZnFC_Pcell_1Vdelithi"}}})

    def test_repeat(self):
        kw = items.mar_series("AgFC", -2e5, "high_q_asymm", n=40, ti=3, series="cell_7")["kwargs"]
        self.assertEqual((kw["n"], kw["ti"], kw["texpo"], kw["geometry"], kw["md"]),
                         (40, 3.0, -2e5, "high_q_asymm", {"series": "cell_7"}))

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
            items.mar_series("f", 2, geometry="tall")


class InsituTest(unittest.TestCase):
    def test_defaults(self):
        kw = items.insitu_step("film_t1", series="film-t1")["kwargs"]
        self.assertEqual(kw, {"align": True, "th_list": [-0.05, 0.11, 0.13, 0.15], "texpo": -7e4,
                              "name": "film_t1", "md": {"series": "film-t1"}})

    def test_name_needed_only_with_frames(self):
        with self.assertRaises(Invalid):
            items.insitu_step(None)
        kw = items.insitu_step("ignored", th_list=[])["kwargs"]
        self.assertNotIn("name", kw)
        with self.assertRaises(Invalid):
            items.insitu_step(None, align=False, th_list=[])

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
        self.assertEqual(kw, {"align": False, "th_list": [], "texpo": -7e4, "T": 25.0, "ramp": 25.0,
                              "soak": [3600, 1.0]})

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
        kw = items.insitu_items([row], "")[0]["kwargs"]
        self.assertEqual(kw, {"align": True, "th_list": [-0.05, 0.11], "texpo": -7e4, "name": "f1",
                              "md": {"series": "f1"}})

    def test_left_out_keys_take_the_new_rows(self):
        self.assertEqual(items.insitu_items([{"name": "f1"}], "s"),
                         [items.insitu_step("f1", series="s")])

    def test_blank_cells_are_empty(self):
        kw = items.insitu_row({"name": "f1", "T": " ", "ramp": "", "soak_n": "", "soak_t": None}, None)["kwargs"]
        self.assertFalse({"T", "ramp", "soak"} & set(kw))

    def test_tolerance_counts_only_with_wait(self):
        kw = items.insitu_row({"name": "f", "T": 30, "tolerance": 0.5}, None)["kwargs"]
        self.assertNotIn("tolerance", kw)
        kw = items.insitu_row({"name": "f", "T": 30, "wait": True, "tolerance": "0.5"}, None)["kwargs"]
        self.assertEqual(kw["tolerance"], 0.5)

    def test_series(self):
        rows = [{"align": False, "th_list": [], "T": 30, "name": ""}, {"name": "f2"}]
        self.assertEqual([i["kwargs"]["md"] for i in items.insitu_items(rows)], [{"series": "f2"}] * 2)
        self.assertEqual(items.insitu_items(rows, " s ")[0]["kwargs"]["md"], {"series": "s"})
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
                items.insitu_items([{"name": "f"}, row, {"name": ""}], "s")


class DescribeTest(unittest.TestCase):
    def test_cells(self):
        item = items.insitu_step("film_t1", series="film-t1")
        item["kwargs"]["md"]["user"] = "bl5a"
        self.assertEqual(items.describe(item),
                         ("insitu_step", "film_t1", "align=True texpo=-70000 th_list=-0.05/0.11/0.13/0.15",
                          "film-t1", "bl5a"))
        self.assertEqual(items.describe({"name": "align_film"}), ("align_film", "", "", "align_film", ""))


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
            items.align("align_coin", "a/b")
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
        bare = items.placed(items.align("align_film"), "/d")
        self.assertEqual(bare["kwargs"]["md"], {"data_root": "/d"})
        self.assertEqual(items.folder(bare), "/d/align_film")
        for bad in ["rel/dir", "~/data", "/data/u 1", "/data/u\t1"]:
            with self.assertRaises(Invalid, msg=bad):
                items.placed(item, bad)
        # "/d/" and the series: DIR_MAX bytes.
        items.placed(items.align("align_film", "x" * 197), "/d")
        with self.assertRaises(Invalid):
            items.placed(items.align("align_film", "x" * 198), "/d")


def archived(uid, name, **result):
    return {"item_type": "plan", "name": name, "item_uid": uid, "result": result}


class FinishedTest(unittest.TestCase):
    """bl5a-qs's history as `history_get` returns it: each item with the
    result the server archived (qs/server.rs)."""

    def test_first_read_is_the_baseline(self):
        f = items.Finished()
        self.assertEqual(f.new([archived("a", "align_film", exit_status="success")]), [])

    def test_only_items_not_read_before(self):
        f = items.Finished()
        a = archived("a", "align_film", exit_status="success")
        b = archived("b", "mar_series", exit_status="fail", reason="spec: refused")
        f.new([a])
        self.assertEqual(f.new([a, b]), [b])
        self.assertEqual(f.new([a, b]), [])

    def test_cleared_history(self):
        f = items.Finished()
        f.new([archived("a", "align_film", exit_status="success")])
        self.assertEqual(f.new([]), [])
        c = archived("c", "align_coin", exit_status="abort")
        self.assertEqual(f.new([c]), [c])

    def test_ended_line(self):
        self.assertEqual(items.ended(archived("a", "align_film", exit_status="success", run_uid="r", reason=None)),
                         "align_film ended: success")
        # An item that failed before its plan opened a run has no run_uid.
        self.assertEqual(items.ended(archived("b", "mar_series", exit_status="fail",
                                              reason="plan build failed: texpo")),
                         "mar_series ended: fail: plan build failed: texpo")


if __name__ == "__main__":
    unittest.main()
