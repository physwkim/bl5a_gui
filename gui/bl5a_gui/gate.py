"""What may be done in the state qs and SPEC are in: the one rule the
GUI's buttons and the MCP tools both apply."""

# The PVs under config.SPEC whose text the gate reads.
SPEC_STATE = ("Connected", "Busy", "Cmd:Busy")


def allowed(qs, spec, after_frame):
    """The actions open now, by name. `qs` is the last status (None while
    qs does not answer), `spec` maps SPEC_STATE to each PV's text (None
    while disconnected), `after_frame` is StopAfterFrame.asked. SPEC (its
    command line and the motor tweaks) is free only when qs runs nothing
    and SPEC is connected and idle."""
    st = qs or {}
    state = st.get("re_state") if qs else None
    running = state is not None and state != "idle"
    return {
        "spec": (state == "idle" and not st.get("running_item_uid") and spec.get("Connected") == "Connected"
                 and spec.get("Busy") == "Ready" and spec.get("Cmd:Busy") == "Idle"),
        "pause": state == "executing_queue",
        "resume": state == "paused",
        "stop": running,
        "abort": running,
        "stop_after_frame": (state == "executing_queue" and not after_frame
                             and st.get("running_item_name") == "mar_series"),
    }


class StopAfterFrame:
    """"Stop after frame": a deferred pause lets the frame being taken
    finish, and a stop follows once qs reports paused. `asked` holds from
    the request until then; qs idle or silent drops it, and so does a
    refused request or an abort (the caller clears it)."""

    def __init__(self):
        self.asked = False

    def status(self, qs):
        """Read one status; True when the stop is due now."""
        if not self.asked:
            return False
        state = qs.get("re_state") if qs is not None else None
        if state == "paused":
            self.asked = False
            return True
        if qs is None or state == "idle":
            self.asked = False
        return False
