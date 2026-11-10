"""Queue items for the bl5a-qs plans, and the checks a form runs before it
queues one. No Qt.

bl5a-qs accepts any item into its queue and checks the arguments only when
the item runs, so a bad item fails mid-queue; the forms check here first.
Each check is qs's own rule (qs/src/plans/{mod,mar,blocks,scan}.rs) or, where
noted, the macro's stricter one (mymar's header: Ti > 2.5 min, Texpo 1–20
s)."""

import math
import posixpath
import unicodedata

from . import config
from .docs import outcome

# mar_series's scan modes: mar345expose, mar345_lab6 (th and phi rock
# over ±1° during the exposure), mar345_lab6_wide (±5°).
MODES = ("static", "rocking1", "rocking2")
# mymar's `fi 0 1 0 0`: the PFCU0 filter blades the frames are taken behind.
FILTERS = (0, 1, 0, 0)
TH_LIST = (-0.05, 0.11, 0.13, 0.15)
IN_TEXPO = -7e4
# changec's DELTA_K in the sixc state; qs's default.
TOLERANCE = 0.3


class Invalid(ValueError):
    """A form value bl5a-qs (or the macro) would refuse; the message names
    the field."""


def plan_item(plan, /, **kwargs):
    # Positional-only: mar_series and insitu_step take a `name` kwarg.
    return {"item_type": "plan", "name": plan, "args": [], "kwargs": kwargs}


# A box left empty: its name, then what to put in it.
EMPTY = "{} is empty: {}"


def _blank(v):
    return v is None or (isinstance(v, str) and not v.strip())


def required(v, field, what):
    """`v`; Invalid saying `what` to put in the box `field` when it is
    blank."""
    if _blank(v):
        raise Invalid(EMPTY.format(field, what))
    return v


def file_name(name, field="name"):
    """mar345expose's file name: 1 to 27 bytes, no whitespace or quotes
    (qs file_name; the macros' "less than 28 characters, no 'space'")."""
    required(name, field, "type the mar345 file name, up to 27 characters without spaces or quotes")
    if not isinstance(name, str) or len(name.encode()) >= 28 \
            or any(c.isspace() or c in "\"'" for c in name):
        raise Invalid("{}: 1 to 27 characters without spaces or quotes, got {!r}".format(field, name))
    return name


def exposure(texpo, field="texpo"):
    """Seconds (> 0) or monitor counts (< 0), as qs's exposure."""
    texpo = number(texpo, field, "type seconds, or negative monitor counts")
    if texpo == 0.0:
        raise Invalid("{}: seconds (> 0) or monitor counts (< 0), got 0".format(field))
    return texpo


def number(v, field, what="type a number"):
    required(v, field, what)
    try:
        v = float(v)
    except (TypeError, ValueError):
        raise Invalid("{}: not a number: {!r}".format(field, v))
    if not math.isfinite(v):
        raise Invalid("{}: {} is not finite".format(field, v))
    return v


def count(v, field):
    """A whole number, at least 1."""
    n = number(v, field) if not isinstance(v, bool) else math.nan
    if not n >= 1 or n != int(n):
        raise Invalid("{}: a whole number, at least 1, got {!r}".format(field, v))
    return int(n)


def numbers(text, field):
    """A list of numbers from "a, b c"."""
    parts = [p for p in str(text).replace(",", " ").split() if p]
    return [number(p, field) for p in parts]


# qs's folder rule (qs/src/place.rs): a series names one folder under the
# data root, and mar345dtb takes the folder in one word of a 256-byte line.
DIR_MAX = 200


def one_word(text, field):
    """`text` as (part of) the folder of mar345 frames."""
    if len(text.encode()) > DIR_MAX or any(c.isspace() or unicodedata.category(c) == "Cc" for c in text):
        raise Invalid("{}: at most {} bytes without spaces or control characters, as mar345dtb takes "
                      "the frame's path in one word, got {!r}".format(field, DIR_MAX, text))
    return text


def series_md(series):
    series = (series or "").strip()
    if not series:
        return {}
    if series in (".", "..") or "/" in series:
        raise Invalid("series: not \".\" or \"..\", nor with '/', to name a folder, got {!r}".format(series))
    return {"series": one_word(series, "series")}


def data_root(text):
    """The Data folder setting as md.data_root: an absolute path, or None
    (empty) for bl5a-qs's --data-root."""
    text = (text or "").strip()
    if not text:
        return None
    if not posixpath.isabs(text):
        raise Invalid("data folder: an absolute path, got {!r}".format(text))
    return one_word(text, "data folder")


def placed(item, root):
    """`item` with md.data_root set to the Data folder `root`; as it is
    when `root` is empty."""
    root = data_root(root)
    if root is None:
        return item
    kw = dict(item.get("kwargs") or {})
    kw["md"] = dict(kw.get("md") or {}, data_root=root)
    item = dict(item, kwargs=kw)
    one_word(folder(item), "data folder")
    return item


def folder(item):
    """Where bl5a-qs puts the item's data (its Place::of): the series, else
    the plan's name, under md.data_root, else under qs's --data-root."""
    kw = item.get("kwargs") or {}
    md = kw.get("md") if isinstance(kw.get("md"), dict) else {}
    name = str(md.get("series") or item.get("name") or "")
    root = md.get("data_root")
    return posixpath.join(str(root), name) if root else name


def blades(v, field):
    """`fi`'s four PFCU0 blades, 0 or 1 each, from "0 1 0 0" or a list."""
    if isinstance(v, str):
        values = numbers(required(v, field, "type the four blades, 0 or 1 each: 0 1 0 0"), field)
    else:
        values = list(v)
    if len(values) != 4 or any(isinstance(x, bool) or x not in (0, 1) for x in values):
        raise Invalid("{}: four blades, 0 or 1 each, got {!r}".format(field, v))
    return [int(x) for x in values]


def mar_series(name, texpo, mode="static", filters=FILTERS, n=1, ti=None, series=None):
    """mymar1 (n = 1, ti None) or mymar (n frames, one every ti minutes)."""
    kwargs = {"name": file_name(name, "File name")}
    texpo = exposure(texpo, "Texpo")
    if texpo > 0 and not 1.0 <= texpo <= 20.0:
        # mymar: "Exposure Time in [sec]: 1 ~ 20".
        raise Invalid("Texpo: seconds 1 to 20, or negative monitor counts, got {:g}".format(texpo))
    kwargs["texpo"] = texpo
    if mode not in MODES:
        raise Invalid("mode: one of {}, got {!r}".format(", ".join(MODES), mode))
    kwargs["mode"] = mode
    kwargs["filters"] = blades(filters, "Filters")
    kwargs["n"] = n = count(n, "N")
    if ti is not None:
        ti = number(ti, "Ti", "type the minutes from one frame's start to the next, more than 2.5")
        if not ti > 2.5:
            # mymar: "Time inteval in min > 2.5".
            raise Invalid("Ti: minutes, more than 2.5, got {:g}".format(ti))
        kwargs["ti"] = ti
    elif n > 1:
        raise Invalid("Ti: needed for more than one frame")
    md = series_md(series if series is not None else name)
    if md:
        kwargs["md"] = md
    return plan_item("mar_series", **kwargs)


def insitu_step(name=None, blocks=None, sequence=None, th_list=TH_LIST, texpo=IN_TEXPO, series=None, T=None,
                ramp=None, wait=False, tolerance=None, soak=None):
    """One row of the in-situ table: myinsitu's set_ramp → settemp (changec
    with wait) → loopscan → the alignment sequence `blocks` (none when
    None) → mymar_in. `tolerance` goes with wait only, as qs takes it."""
    kwargs = {}
    if blocks is not None:
        kwargs.update(alignment(blocks, sequence))
    th_list = [number(v, "th") for v in th_list]
    kwargs["th_list"] = th_list
    kwargs["texpo"] = exposure(texpo)
    if th_list:
        # qs ignores the name of a row with no frames.
        kwargs["name"] = file_name(name)
    temp = {}
    if T is not None:
        temp["T"] = number(T, "T")
    if ramp is not None:
        ramp = number(ramp, "ramp")
        if ramp <= 0:
            raise Invalid("ramp: °C/min, positive, got {:g}".format(ramp))
        temp["ramp"] = ramp
    if wait:
        if T is None:
            raise Invalid("wait: needs a T to wait for")
        temp["wait"] = True
        if tolerance is not None:
            tolerance = number(tolerance, "tolerance")
            if tolerance <= 0:
                raise Invalid("tolerance: °C, positive, got {:g}".format(tolerance))
            temp["tolerance"] = tolerance
    elif tolerance is not None:
        raise Invalid("tolerance: only with wait")
    if soak is not None:
        n, t = soak
        n = count(n, "soak n")
        t = number(t, "soak t")
        if t <= 0:
            raise Invalid("soak t: seconds per point, positive, got {:g}".format(t))
        temp["soak"] = [int(n), t]
    kwargs.update(temp)
    if blocks is None and not th_list and not temp:
        raise Invalid("the row does nothing: no alignment, no th list, no temperature")
    md = series_md(series)
    if md:
        kwargs["md"] = md
    return plan_item("insitu_step", **kwargs)


# An in-situ table row, as a new row shows it: None is an empty cell, and
# th_list is a list or the "a, b c" text of its cell.
NEW_ROW = {"T": None, "ramp": None, "wait": False, "soak_n": None, "soak_t": None, "tolerance": TOLERANCE,
           "align": True, "th_list": TH_LIST, "texpo": IN_TEXPO, "name": ""}


def insitu_row(row, series, sequence=None):
    """One in-situ table row as its insitu_step item, running `sequence`
    ((name, blocks)) when its align is on. A key left out takes NEW_ROW's
    value, a blank cell is None, and the tolerance counts only with wait,
    as the table always shows one."""
    unknown = sorted(set(row) - set(NEW_ROW))
    if unknown:
        raise Invalid("unknown column(s) {}; the columns are {}".format(", ".join(unknown), ", ".join(NEW_ROW)))
    row = dict(NEW_ROW, **row)

    def get(key):
        v = row[key]
        return None if _blank(v) else v.strip() if isinstance(v, str) else v

    soak_n, soak_t = get("soak_n"), get("soak_t")
    if (soak_n is None) != (soak_t is None):
        raise Invalid("soak: both n and t, or neither")
    th = row["th_list"]
    wait = bool(row["wait"])
    picked, run = None, None
    if row["align"]:
        if sequence is None:
            raise Invalid("align: no alignment sequence picked")
        picked, run = sequence
    return insitu_step(
        name=get("name"), blocks=run, sequence=picked, th_list=numbers(th, "th list") if isinstance(th, str) else th,
        texpo=row["texpo"].strip() if isinstance(row["texpo"], str) else row["texpo"], series=series,
        T=get("T"), ramp=get("ramp"), wait=wait, tolerance=get("tolerance") if wait else None,
        soak=None if soak_n is None else (soak_n, soak_t))


def insitu_items(rows, series=None, sequence=None):
    """The in-situ table as items, in its order and all in one series:
    `series`, or when empty the first row name there is; the rows with
    align on run `sequence` ((name, blocks)). Invalid names the first bad
    row."""
    rows = list(rows)
    if not rows:
        raise Invalid("the table has no rows")
    series = (series or "").strip()
    if not series:
        series = next((n for n in (str(r.get("name") or "").strip() for r in rows) if n), "")
        if not series:
            raise Invalid(EMPTY.format("Series", "type the folder the frames go to, or a name in a row"))
    out = []
    for k, row in enumerate(rows, 1):
        try:
            out.append(insitu_row(row, series, sequence))
        except Invalid as e:
            raise Invalid("row {}: {}".format(k, e))
    return out


def cooling_row():
    """myinsitu's last row: set_ramp 25; settemp 025; loopscan 3600 — no
    alignment, no exposure."""
    return {"T": 25.0, "ramp": 25.0, "wait": False, "soak_n": 3600, "soak_t": 1.0, "align": False,
            "th_list": [], "name": ""}


# An alignment's blocks, as qs/src/plans/blocks.rs takes them.
KINDS = ("scan", "move", "spec", "wait", "erase", "loop")
# The scan types: type → fly (cscan, clup) or step (ascan, lup).
SCAN_TYPES = {"ascan": False, "lup": False, "cscan": True, "clup": True}
PICKS = ("CEN", "COM", "xMAX", "EDGE")
COUNTERS = ("icxrd", "det", "ic1")


class _Fields:
    """One block's fields, each taken once; what is left over is an error."""

    def __init__(self, at, block):
        self.at, self.block, self.taken = at, block, set()

    def err(self, what):
        return Invalid("{}: {}".format(self.at, what))

    def take(self, key):
        self.taken.add(key)
        return self.block.get(key)

    def need(self, key):
        v = self.take(key)
        if v is None:
            raise self.err("{} is missing".format(key))
        return v

    def num(self, key, ok=None, rule=None):
        v = self.need(key)
        if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v):
            raise self.err("{} must be a number, got {!r}".format(key, v))
        if ok is not None and not ok(v):
            raise self.err("{} must be {}, got {:g}".format(key, rule, v))
        return v

    def text(self, key):
        v = self.need(key)
        if not isinstance(v, str):
            raise self.err("{} must be a string, got {!r}".format(key, v))
        return v

    def one_of(self, key, names):
        v = self.need(key)
        if not isinstance(v, str) or v not in names:
            raise self.err("{} must be one of {}, got {!r}".format(key, ", ".join(names), v))
        return v

    def done(self):
        extra = sorted(set(self.block) - self.taken)
        if extra:
            raise self.err("unknown field {}".format(extra[0]))


def _scan(f):
    fly = SCAN_TYPES[f.one_of("type", tuple(SCAN_TYPES))]
    f.one_of("motor", config.MOTORS)
    f.num("start")
    f.num("finish")
    if fly:
        if f.take("monitor") is not None:
            raise f.err("monitor is for ascan and lup; a {} counts for time".format(f.block["type"]))
        f.num("time", lambda t: t > 0, "seconds > 0")
        f.num("sleep", lambda s: s >= 0, "seconds >= 0")
    else:
        seconds, monitor = f.take("time"), f.take("monitor")
        if seconds is not None and monitor is not None:
            raise f.err("time and monitor both given; a point counts to one")
        if seconds is None and monitor is None:
            raise f.err("time or monitor is missing")
        if monitor is None:
            f.num("time", lambda t: t > 0, "seconds > 0")
        else:
            f.num("monitor", lambda n: n >= 1 and n == int(n), "a whole number of counts >= 1")
        f.num("intervals", lambda n: n >= 1 and n == int(n), "a whole number >= 1")
        f.num("settle", lambda s: s >= 0, "seconds >= 0")
    f.one_of("counter", COUNTERS)


def _then(f):
    """`pick` (of a dip given `dip`) and `set`; False without a pick, which
    leaves the motor where its scan did."""
    if f.take("pick") is None:
        for key in ("dip", "set"):
            v = f.take(key)
            if v is not None and v is not False:  # `set` 0 is not False here, as in qs
                raise f.err("{} is for a pick, and pick is missing".format(key))
        return False
    pick = f.one_of("pick", PICKS)
    dip = f.take("dip")
    if dip is not None and not isinstance(dip, bool):
        raise f.err("dip must be true or false, got {!r}".format(dip))
    if dip and pick == "EDGE":
        raise f.err("dip is for CEN, COM and xMAX, not EDGE")
    if f.block.get("set") is not None:
        f.num("set")
    f.take("set")
    return True


def _g(x):
    return "{:g}".format(x)


def scan_line(b):
    """A scan as the Stages table names it, SPEC's argument order, then
    where its motor goes after."""
    if SCAN_TYPES[b["type"]]:
        how = "{} {}".format(_g(b["time"]), _g(b["sleep"]))
    else:
        # SPEC's count time: monitor counts as their negative.
        how = "{} {}".format(b["intervals"], _g(-b["monitor"] if b.get("monitor") is not None else b["time"]))
    line = "{} {} {} {} {}  {}".format(b["type"], b["motor"], _g(b["start"]), _g(b["finish"]), how, b["counter"])
    if b.get("pick") is None:
        return line
    line += " → " + b["pick"]
    if b.get("dip"):
        line += " dip"
    if b.get("set") is not None:
        line += ", set {}".format(_g(b["set"]))
    return line


def _moves(f):
    pairs = f.need("moves")
    if not isinstance(pairs, list) or not pairs:
        raise f.err("moves must be [motor, position] pairs, got {!r}".format(pairs))
    seen = set()
    for p in pairs:
        m, x = p if isinstance(p, (list, tuple)) and len(p) == 2 else (None, None)
        if not isinstance(m, str) or isinstance(x, bool) or not isinstance(x, (int, float)) \
                or not math.isfinite(x):
            raise f.err("moves: [motor, position], got {!r}".format(p))
        if m not in config.MOTORS:
            raise f.err("moves: motor must be one of {}, got {!r}".format(", ".join(config.MOTORS), m))
        if m in seen:
            raise f.err("moves: {} twice".format(m))
        seen.add(m)


def _when(f):
    w = f.take("when")
    if w is None:
        return
    if not isinstance(w, dict):
        raise f.err("when must be {{motor, op, value}}, got {!r}".format(w))
    g = _Fields(f.at + " when", w)
    g.one_of("motor", config.MOTORS)
    g.one_of("op", ("<", ">"))
    g.num("value")
    g.done()


def block(at, b):
    """`b` as qs takes a block, or Invalid naming `at` and the field."""
    if not isinstance(b, dict):
        raise Invalid("{}: must be an object, got {!r}".format(at, b))
    f = _Fields(at, b)
    kind = f.one_of("kind", KINDS)
    f.at = "{} ({})".format(at, kind)
    note = f.take("note")
    if note is not None and not isinstance(note, str):
        raise f.err("note must be a string, got {!r}".format(note))
    _when(f)
    if kind == "scan":
        _scan(f)
        if not _then(f):
            raise f.err("pick is missing")
    elif kind == "move":
        _moves(f)
    elif kind == "spec":
        line = f.text("command").strip()
        if not line:
            raise f.err(EMPTY.format("command", "type one SPEC command line"))
        if "\n" in line or "\r" in line:
            raise f.err("command must be one line, got {!r}".format(line))
    elif kind == "wait":
        f.num("seconds", lambda s: s > 0, "seconds > 0")
    elif kind == "loop":
        f.num("times", lambda n: n >= 1 and n == int(n), "a whole number >= 1")
        inner = f.need("blocks")
        if not isinstance(inner, list) or not inner:
            raise f.err("blocks must be a list of blocks, got {!r}".format(inner))
        for k, x in enumerate(inner, 1):
            block("{} block {}".format(f.at, k), x)
    f.done()
    return b


def sequence_blocks(value):
    """An alignment sequence's blocks: a non-empty list each qs takes."""
    if not isinstance(value, list):
        raise Invalid("blocks must be a list, got {!r}".format(value))
    if not value:
        raise Invalid("the sequence has no blocks")
    return [block("block {}".format(k), b) for k, b in enumerate(value, 1)]


def alignment(value, sequence=None):
    """The kwargs an alignment sequence goes to qs as: its blocks, and its
    name to record."""
    kwargs = {"blocks": sequence_blocks(value)}
    if sequence:
        kwargs["sequence"] = str(sequence)
    return kwargs


def align(value, sequence=None, series=None):
    """An `align` item: the sequence `value` (its blocks) named `sequence`."""
    kwargs = alignment(value, sequence)
    md = series_md(series)
    if md:
        kwargs["md"] = md
    return plan_item("align", **kwargs)


# align_in.mac's `atte 11`: the filters its scans count the direct beam through.
SCAN_ATTE = 11


def scan(fields, atte, series=None):
    """A `scan` item: one scan, an alignment scan block's `fields` without
    its kind, the pick optional, counted through `atte n`'s filters (qs
    plans/scan.rs)."""
    f = _Fields("scan", fields)
    _scan(f)
    _then(f)
    f.done()
    kwargs = dict(fields)
    kwargs["atte"] = int(_Fields("scan", {"atte": atte}).num(
        "atte", lambda n: 0 <= n <= 63 and n == int(n), "a whole number 0 to 63"))
    md = series_md(series)
    if md:
        kwargs["md"] = md
    return plan_item("scan", **kwargs)


def describe(item):
    """(plan, name, parameters, folder, user) for the queue's cells."""
    kw = item.get("kwargs") or {}
    md = kw.get("md") if isinstance(kw.get("md"), dict) else {}
    plan = str(item.get("name") or "")
    shown = []
    if "blocks" in kw and "sequence" not in kw:
        shown.append("blocks={}".format(len(kw["blocks"]) if isinstance(kw["blocks"], list) else "?"))
    if plan == "scan":
        try:
            shown.append(scan_line(kw))
        except (KeyError, TypeError, ValueError):
            shown.append("?")
    for k in ("sequence", "T", "ramp", "wait", "tolerance", "soak", "n", "ti", "texpo", "mode", "filters", "atte",
              "th_list"):
        if k in kw:
            v = kw[k]
            if k == "sequence":
                v = repr(v) if " " in str(v) else v
            elif isinstance(v, list):
                v = "/".join("{:g}".format(x) for x in v) if all(isinstance(x, (int, float)) for x in v) else v
            elif isinstance(v, float):
                v = "{:g}".format(v)
            shown.append("{}={}".format(k, v))
    return (plan, str(kw.get("name") or ""), " ".join(shown), folder(item),
            str(md.get("user") or item.get("user") or ""))


def ended(item):
    """The Messages line for an item bl5a-qs archived in its history."""
    result = item.get("result") or {}
    return "{} ended: {}".format(item.get("name"), outcome(result.get("exit_status"), result.get("reason")))


class Finished:
    """The items of bl5a-qs's history not read before. The first history
    read is the baseline: what ended before the GUI started is not news."""

    def __init__(self):
        self.seen = None

    def new(self, history):
        uids = {i.get("item_uid") for i in history}
        fresh = [] if self.seen is None else [i for i in history if i.get("item_uid") not in self.seen]
        self.seen = uids
        return fresh
