"""Where the GUI's services and PVs are (design.md, "GUI")."""

import os

import numpy as np

QS_CONTROL = "tcp://localhost:60715"
QS_DOCUMENTS = "tcp://localhost:60725"
INTEGRATOR_REP = "tcp://localhost:60735"
INTEGRATOR_PUB = "tcp://localhost:60745"
USER = "bl5a"

# The GUI's saved fields (QSettings organization, application); the MCP
# server reads the Data folder from it too.
SETTINGS = ("bl5a", "bl5a-gui")


def arguments(parser):
    """The service addresses, as the GUI and the MCP server take them."""
    parser.add_argument("--qs-control", default=QS_CONTROL)
    parser.add_argument("--qs-documents", default=QS_DOCUMENTS)
    parser.add_argument("--integrator-rep", default=INTEGRATOR_REP)
    parser.add_argument("--integrator-pub", default=INTEGRATOR_PUB)
    parser.add_argument("--user", default=USER)
    return parser

SPEC = "BL5A:SPEC:"
# The SPEC command line as a long string: Cmd itself is a DBF_STRING and
# holds 39 characters, as bl5a-qs's Cmd.VAL$ does not.
SPEC_CMD = SPEC + "Cmd.VAL$"
MAR = "BL5A:MAR345:cam1:"


def spec_line(cmd):
    """`cmd` as SPEC_CMD takes it, a NUL-ended char array: repics 0.4.3
    sends a str as a DBR_STRING, which holds 39 characters, even to a char
    array."""
    return np.frombuffer(cmd.encode() + b"\0", np.uint8)

# The ring current; the sim stack serves no such PV, so it reads "—" there.
RING_PV = "BL5A:userCalcOut1.VAL"
# Not known yet: the front-end shutter's PV, and the temperature
# controller's (bl5a-temp-ioc, design step 7). None reads "—".
SHUTTER_PV = None
TEMP_SP_PV = None
TEMP_RBV_PV = None
TEMP_RAMP_PV = None

# The spec-ioc motor records, in the panel's order.
MOTORS = ("sy", "shold", "th", "tth", "nu", "sx", "phi", "scell", "martran", "marsdd", "marshut")

# The ring lines the image tab draws by default: Al 111 and 200, Å⁻¹.
RINGS = (2.687, 3.103)

# The PONI the integrator was started with, when its `config` reply is not
# there to say.
PONI = os.path.join(os.path.dirname(__file__), "..", "..", "sim", "bl5a.poni")
