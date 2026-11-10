"""The scan form's widgets as the scan item they queue, and the block
dialog's boxes."""

import unittest

from silx.gui import qt

from bl5a_gui import editor, forms, items, sequences

app = qt.QApplication.instance() or qt.QApplication([])

LUP = {"type": "lup", "motor": "sy", "start": -0.1, "finish": 0.1, "time": 1, "intervals": 10, "settle": 0,
       "counter": "det"}


class ScanFormTest(unittest.TestCase):
    def test_default_is_a_lup_of_the_first_motor_going_nowhere_after(self):
        f = forms.ScanForm()
        self.assertEqual(f.item(), items.scan(LUP))
        self.assertEqual(f.scan.pick.currentText(), editor.NO_PICK)
        self.assertFalse(f.scan.dip.isEnabled())
        self.assertFalse(f.scan.set.isEnabled())

    def test_a_clup_to_its_com_set_there(self):
        f = forms.ScanForm()
        s = f.scan
        s.type.setCurrentText("clup")
        s.motor.setCurrentText("th")
        s.start.setText(" -1 ")
        s.finish.setText("1")
        s.time.setText("5")
        s.sleep.setText("0.1")
        s.counter.setCurrentText("ic1")
        s.pick.setCurrentText("COM")
        self.assertTrue(s.dip.isEnabled() and s.set.isEnabled())
        s.dip.setChecked(True)
        s.set.setText("0")
        f.series.setText("s1")
        want = {"type": "clup", "motor": "th", "start": -1, "finish": 1, "time": 5, "sleep": 0.1, "counter": "ic1",
                "pick": "COM", "dip": True, "set": 0}
        self.assertEqual(f.item(), items.scan(want, series="s1"))
        self.assertEqual(f.item()["kwargs"]["md"], {"series": "s1"})

    def test_none_drops_what_a_pick_would_take(self):
        f = forms.ScanForm()
        f.scan.pick.setCurrentText("CEN")
        f.scan.dip.setChecked(True)
        f.scan.set.setText("1")
        f.scan.pick.setCurrentText(editor.NO_PICK)
        self.assertEqual(f.item(), items.scan(LUP))

    def test_an_empty_or_wrong_box_is_named(self):
        for box, text, message in (("start", " ", "^Start is empty: type a number$"),
                                   ("finish", "x", "^Finish: not a number: 'x'$"),
                                   ("time", "0", "^scan: time must be seconds > 0, got 0$")):
            f = forms.ScanForm()
            getattr(f.scan, box).setText(text)
            with self.subTest(box=box), self.assertRaisesRegex(items.Invalid, message):
                f.item()

    def test_add_emits_only_a_valid_item(self):
        f = forms.ScanForm()
        got = []
        f.queueRequested.connect(got.append)
        f.add.click()
        self.assertEqual(got, [f.item()])
        f.scan.time.setText("0")
        self.assertRaises(items.Invalid, f.item)


class BlockDialogTest(unittest.TestCase):
    def test_an_empty_box_is_named_as_its_label(self):
        for kind, box, message in (("wait", "seconds", "^Seconds is empty: type a number$"),
                                   ("move", "moves", "^umv is empty: type motor position pairs"),
                                   ("scan", "time", "^Count is empty: type a number$")):
            d = editor.BlockDialog(dict(editor.NEW[kind]))
            getattr(d.scan if kind == "scan" else d, box).setText(" ")
            with self.subTest(kind=kind), self.assertRaisesRegex(items.Invalid, message):
                d.block()


class InsituFormTest(unittest.TestCase):
    def test_a_long_sequence_name_does_not_widen_the_form(self):
        f = forms.InsituForm()
        self.addCleanup(f.deleteLater)
        f.show()
        app.processEvents()
        width = f.sizeHint().width()
        f.sequence.addItem("x" * sequences.NAME_MAX)
        app.processEvents()
        self.assertEqual(f.sizeHint().width(), width)


if __name__ == "__main__":
    unittest.main()
