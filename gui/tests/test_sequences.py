import json
import pathlib
import tempfile
import unittest

from bl5a_gui import config, sequences
from bl5a_gui.items import Invalid

ERASE = [{"kind": "erase"}]


def W(seconds):
    return {"kind": "wait", "seconds": seconds}


class SequencesTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = self.dir.name

    def tearDown(self):
        self.dir.cleanup()

    def test_templates_without_a_data_folder(self):
        self.assertEqual(sequences.names(None, "kim"), ["Coin cell", "Thin film"])
        self.assertEqual(sequences.users(None), [])
        self.assertTrue(sequences.get(sequences.DEFAULT))
        with self.assertRaisesRegex(Invalid, "^Data folder is empty: choose one in the Data box"):
            sequences.save("mine", ERASE, None, "kim")
        with self.assertRaisesRegex(Invalid, "^Data folder is empty: choose one in the Data box"):
            sequences.get("mine", None, "kim")

    def test_save_get_delete(self):
        path = sequences.save("my film", ERASE, self.root, "kim")
        self.assertEqual(path, pathlib.Path(self.root, "alignments", "kim", "my film.json"))
        self.assertEqual(json.loads(path.read_text()), {"blocks": ERASE})
        self.assertEqual(sequences.users(self.root), ["kim"])
        self.assertEqual(sequences.names(self.root, "kim"), ["Coin cell", "Thin film", "my film"])
        self.assertEqual(sequences.saved(self.root, "lee"), [])
        self.assertEqual(sequences.get("my film", self.root, "kim"), ERASE)
        sequences.save("my film", ERASE * 2, self.root, "kim")
        self.assertEqual(sequences.get("my film", self.root, "kim"), ERASE * 2)
        self.assertEqual(list(path.parent.iterdir()), [path])
        sequences.delete("my film", self.root, "kim")
        self.assertEqual(sequences.saved(self.root, "kim"), [])
        with self.assertRaisesRegex(Invalid, "not saved by kim"):
            sequences.delete("my film", self.root, "kim")
        with self.assertRaisesRegex(Invalid, "nor saved by kim"):
            sequences.get("my film", self.root, "kim")

    def test_no_user_is_the_default_user(self):
        for user in (None, "", "  "):
            with self.subTest(user=user):
                self.assertEqual(sequences.folder(self.root, user),
                                 pathlib.Path(self.root, "alignments", config.USER))
        path = sequences.save("mine", ERASE, self.root, "")
        self.assertEqual(path.parent.name, config.USER)
        self.assertEqual(sequences.users(self.root), [config.USER])

    def test_refused(self):
        with self.assertRaisesRegex(Invalid, "template's name"):
            sequences.save("Thin film", ERASE, self.root, "kim")
        with self.assertRaisesRegex(Invalid, "is a template"):
            sequences.delete("Coin cell", self.root, "kim")
        for name in ("", " ", None):
            with self.subTest(name=name), self.assertRaisesRegex(Invalid, "^sequence is empty: type a name$"):
                sequences.save(name, ERASE, self.root, "kim")
        for name in (".hidden", "a/b", "a\\b", "a\nb", "x" * 65):
            with self.subTest(name=name), self.assertRaisesRegex(Invalid, "^sequence: 1 to 64 characters"):
                sequences.save(name, ERASE, self.root, "kim")
        with self.assertRaisesRegex(Invalid, "^User: "):
            sequences.save("ok", ERASE, self.root, "../x")
        with self.assertRaisesRegex(Invalid, "block 1 \\(wait\\): seconds is missing"):
            sequences.save("ok", [{"kind": "wait"}], self.root, "kim")
        self.assertEqual(sequences.users(self.root), [])

    def test_bad_file(self):
        where = pathlib.Path(self.root, "alignments", "kim")
        where.mkdir(parents=True)
        (where / "a.json").write_text("{")
        (where / "b.json").write_text(json.dumps({"blocks": ERASE, "x": 1}))
        (where / "c.json").write_text(json.dumps({"blocks": [{"kind": "jump"}]}))
        for name, why in (("a", "a.json: Expecting"), ("b", "not {\"blocks\""), ("c", "block 1: kind must be")):
            with self.subTest(name=name), self.assertRaisesRegex(Invalid, why):
                sequences.get(name, self.root, "kim")

    def test_text(self):
        film = [sequences.text(b) for b in sequences.get("Thin film")]
        coin = [sequences.text(b) for b in sequences.get("Coin cell")]
        for line in film + coin:
            self.assertTrue(line)
        self.assertIn("if ", "".join(film))
        # A scan reads as the Stages table names it, SPEC's argument order.
        self.assertIn("ascan tth -0.04 0.04 24 0.3  det → CEN, set 0", film)
        self.assertIn("cscan shold -3 -1.5 5 0.05  det → EDGE", film)
        tth = next(b for b in sequences.get("Thin film") if b["kind"] == "scan")
        dip = dict(tth, dip=True)
        self.assertTrue(sequences.text(dip).endswith("det → CEN dip, set 0"), sequences.text(dip))
        # Monitor counts read as SPEC takes them, a negative count time.
        mon = {k: v for k, v in tth.items() if k != "time"}
        mon["monitor"] = 10000
        self.assertEqual(sequences.text(mon), "ascan tth -0.04 0.04 24 -10000  det → CEN, set 0")

    def test_lines_number_a_loop_s_blocks_inside_it(self):
        blocks = [{"kind": "erase"},
                  {"kind": "loop", "times": 3, "when": {"motor": "th", "op": ">", "value": 0},
                   "blocks": [W(1), {"kind": "loop", "times": 2, "blocks": [W(2)]}]}]
        self.assertEqual(sequences.lines(blocks), [
            "1  erase the mar345 plate",
            "2  if th > 0: loop × 3",
            "  2.1  wait 1 s",
            "  2.2  loop × 2",
            "    2.2.1  wait 2 s",
        ])

    def test_edits(self):
        def seconds(blocks):
            return [b["seconds"] if b["kind"] == "wait" else (b["times"], seconds(b["blocks"])) for b in blocks]

        b = [W(1), W(2), W(3), W(4)]
        self.assertEqual(sequences.group(b, (), 1, 2, 3), [(1,)])
        self.assertEqual(seconds(b), [1, (3, [2, 3]), 4])
        sequences.items.sequence_blocks(b)
        self.assertEqual(sequences.move(b, (1,), 0, 0, 1), [(1, 1)])
        self.assertEqual(seconds(b), [1, (3, [3, 2]), 4])
        # At the end of its list a block stays.
        self.assertIsNone(sequences.move(b, (1,), 1, 1, 1))
        self.assertIsNone(sequences.move(b, (), 0, 0, -1))
        self.assertEqual(sequences.move(b, (), 1, 2, -1), [(0,), (1,)])
        self.assertEqual(seconds(b), [(3, [3, 2]), 4, 1])
        self.assertEqual(sequences.duplicate(b, (0,), 0, 1), [(0, 2), (0, 3)])
        self.assertEqual(seconds(b), [(3, [3, 2, 3, 2]), 4, 1])
        self.assertEqual(sequences.insert(b, (0,), 4, W(5)), [(0, 4)])
        self.assertEqual(sequences.ungroup(b, (0,)), [(0,), (1,), (2,), (3,), (4,)])
        self.assertEqual(seconds(b), [3, 2, 3, 2, 5, 4, 1])
        # A loop left with no blocks goes with them.
        b = [W(1), {"kind": "loop", "times": 2, "blocks": [{"kind": "loop", "times": 2, "blocks": [W(2)]}]}]
        self.assertEqual(sequences.remove(b, (1, 0), 0, 0), [(0,)])
        self.assertEqual(seconds(b), [1])
        self.assertEqual(sequences.remove(b, (), 0, 0), [])
        self.assertEqual(b, [])


if __name__ == "__main__":
    unittest.main()
