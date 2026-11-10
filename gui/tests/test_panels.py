"""panels: the status line's qs label."""

import os
import tempfile
import unittest
from unittest import mock

from silx.gui import qt

from bl5a_gui import config, editor, panels, sequences

app = qt.QApplication.instance() or qt.QApplication([])


class QsLabelTest(unittest.TestCase):
    def test_environment_closed_shows_the_manager_state(self):
        bar = panels.StatusBar()
        bar.setQs({"re_state": "null", "manager_state": "environment_closed", "items_in_queue": 2,
                   "worker_environment_exists": False})
        text = bar.labels["qs"][1].text()
        self.assertIn("environment_closed, 2 queued, environment closed", text)
        self.assertNotIn("null", text)


class BlankUserTest(unittest.TestCase):
    """The User box left empty is config.USER's sequences everywhere."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        patch = mock.patch.object(config, "SETTINGS", (os.path.join(tmp.name, "bl5a-gui.conf"),
                                                       qt.QSettings.IniFormat))
        patch.start()
        self.addCleanup(patch.stop)
        self.root = os.path.join(tmp.name, "data")
        os.mkdir(self.root)

    def test_load_lists_the_default_users_sequences(self):
        box = panels.AlignBox()
        box.user.setEditText("")

        def menu():
            box._savedMenu()
            return [a.text() for a in box.load.menu().actions()]

        self.assertEqual(menu(), ["Set the Data folder first"])
        qt.QSettings(*config.SETTINGS).setValue("data_root", self.root)
        self.assertEqual(menu(), ["No sequences saved by {!r}".format(config.USER)])
        sequences.save("mine", [{"kind": "erase"}], self.root, config.USER)
        self.assertEqual(menu(), ["mine"])

    def test_save_as_asks_before_replacing_the_default_users_sequence(self):
        qt.QSettings(*config.SETTINGS).setValue("data_root", self.root)
        sequences.save("mine", [{"kind": "erase"}], self.root, config.USER)
        ed = editor.SequenceEditor()
        ed.load(sequences.DEFAULT, sequences.get(sequences.DEFAULT))
        with mock.patch.object(qt.QInputDialog, "getText", return_value=("mine", True)), \
                mock.patch.object(qt.QMessageBox, "question", return_value=qt.QMessageBox.No) as ask:
            self.assertFalse(ed._saveAs())
        ask.assert_called_once()
        self.assertEqual(sequences.get("mine", self.root, ""), [{"kind": "erase"}])


if __name__ == "__main__":
    unittest.main()
