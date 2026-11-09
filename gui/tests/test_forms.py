"""The scan form's widgets as the step_scan item they queue."""

import unittest

from silx.gui import qt

from bl5a_gui import forms, items

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
        f.det.setCurrentText("ic1")
        f.series.setText("s1")
        self.assertEqual(f.item(), items.step_scan("th", 1, 2, 5, 0.5, relative=False, det="ic1", series="s1"))

    def test_add_emits_only_a_valid_item(self):
        f = forms.ScanForm()
        got = []
        f.queueRequested.connect(got.append)
        f.add.click()
        self.assertEqual(got, [f.item()])
        f.time.setText("0")
        self.assertRaises(items.Invalid, f.item)


if __name__ == "__main__":
    unittest.main()
