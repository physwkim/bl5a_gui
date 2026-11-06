"""spec-ioc's Log PV as lines, for the GUI's SPEC tab and the MCP server."""


class LogLines:
    """The lines of spec-ioc's Log PV not shown yet. Each value is the
    newest lines, each "<run>:<seq> <text>", run naming one IOC start and
    seq counting its lines from 1; CA delivers only the latest value, so
    the lines come from the numbers, not from the updates."""

    def __init__(self):
        self.run, self.seq = None, 0

    def new(self, block):
        out = []
        for line in block.split("\n"):
            key, sep, text = line.partition(" ")
            run, colon, seq = key.partition(":")
            if not (sep and colon and seq.isdigit()):
                continue
            seq = int(seq)
            if run != self.run:
                if self.run is not None:
                    out.append("--- spec-ioc restarted ---")
                # The first value finds the IOC under way: what precedes
                # it is history, not lost.
                self.seq = 0 if self.run is not None else seq - 1
                self.run = run
            if seq <= self.seq:
                continue
            if seq > self.seq + 1:
                out.append("--- {} lines not shown ---".format(seq - self.seq - 1))
            out.append(text)
            self.seq = seq
        return out
