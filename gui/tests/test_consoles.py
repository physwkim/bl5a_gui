import types

from silx.gui import qt

from bl5a_gui import config
from bl5a_gui.consoles import SpecConsole

app = qt.QApplication.instance() or qt.QApplication([])


def console(error):
    """A SpecConsole that sent "wa", whose put ended with `error`;
    Cmd:Status and Cmd:Reply hold the previous command's."""
    read = []

    def get_text(name):
        read.append(name)
        return {config.SPEC + "Cmd:Status": "OK", config.SPEC + "Cmd:Reply": "previous reply"}[name]

    pvs = types.SimpleNamespace(changed=types.SimpleNamespace(connect=lambda f: None), watch=lambda name: None,
                                get_text=get_text, put=lambda name, value, callback=None: callback(error))
    c = SpecConsole(pvs)
    c.setAllowed(True)
    c.entry.setText("wa")
    c._send()
    return c, read


def test_a_command_whose_put_failed_shows_why_not_the_previous_reply():
    c, read = console("Put operation failed")
    text = c.out.toPlainText()
    assert text.endswith("'wa': Put operation failed") and "previous reply" not in text
    assert read == [] and c.pending is None and c.send.isEnabled()


def test_a_completed_command_shows_its_reply():
    c, read = console(None)
    assert c.out.toPlainText().endswith("previous reply") and len(read) == 2 and c.pending is None
