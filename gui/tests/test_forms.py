"""The scan form's widgets as the step_scan item they queue."""

import unittest

from silx.gui import qt

from bl5a_gui import editor, forms, items, sequences

app = qt.QApplication.instance() or qt.QApplication([])


class ScanFormTest(unittest.TestCase):
    def test_default_is_a_lup_of_the_first_motor(self):
        f = forms.ScanForm()
        self.assertEqual(f.item(), items.step_scan("sy", -0.1, 0.1, 10, 1, relative=True))

    def test_ascan_with_its_fields(self):
        f = forms.ScanForm()
        f.ascan.setChecked(True)
        f.motor.setCurrentText("th")
        f.start.setText(" 1 ")
        f.finish.setText("2")
        f.intervals.setValue(5)
        f.time.setText("0.5")
        f.counter.setCurrentText("ic1")
        f.series.setText("s1")
        self.assertEqual(f.item(), items.step_scan("th", 1, 2, 5, 0.5, relative=False, counter="ic1", series="s1"))

    def test_add_emits_only_a_valid_item(self):
        f = forms.ScanForm()
        got = []
        f.queueRequested.connect(got.append)
        f.add.click()
        self.assertEqual(got, [f.item()])
        f.time.setText("0")
        self.assertRaises(items.Invalid, f.item)


class BlockDialogTest(unittest.TestCase):
    def test_an_empty_box_is_named_as_its_label(self):
        for kind, box, message in (("wait", "seconds", "^Seconds is empty: type a number$"),
                                   ("move", "moves", "^umv is empty: type motor position pairs"),
                                   ("scan", "time", "^Count is empty: type a number$")):
            d = editor.BlockDialog(dict(editor.NEW[kind]))
            getattr(d, box).setText(" ")
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
