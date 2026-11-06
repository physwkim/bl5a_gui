"""Start the BL5A MCP server on stdio: `python -I run_mcp.py [--help]`. With
-I Python leaves this directory off sys.path, so it is put back here."""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from bl5a_gui.mcpserver import main  # noqa: E402

sys.exit(main())
