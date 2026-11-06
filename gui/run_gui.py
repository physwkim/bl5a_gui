"""Start the BL5A GUI: `python -I run_gui.py [--help]`. With -I Python
leaves this directory off sys.path, so it is put back here."""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from bl5a_gui.main import main  # noqa: E402

sys.exit(main())
