"""Queue items for the bl5a-qs plans, and the checks a form runs before it
queues one. No Qt.

bl5a-qs accepts any item into its queue and checks the arguments only when
the item runs, so a bad item fails mid-queue; the forms check here first.
Each check is qs's own rule (qs/src/plans/{mod,mar}.rs) or, where noted,
the macro's stricter one (mymar's header: Ti > 2.5 min, Texpo 1–20 s)."""

import math
import posixpath
import unicodedata

from .docs import outcome

GEOMETRIES = ("standard", "high_q_asymm", "low_q_symmetric")
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


def file_name(name, field="name"):
    """mar345expose's file name: 1 to 27 bytes, no whitespace or quotes
    (qs file_name; the macros' "less than 28 characters, no 'space'")."""
    if not isinstance(name, str) or not name or len(name.encode()) >= 28 \
            or any(c.isspace() or c in "\"'" for c in name):
        raise Invalid("{}: 1 to 27 characters without spaces or quotes, got {!r}".format(field, name))
    return name


def exposure(texpo, field="texpo"):
    """Seconds (> 0) or monitor counts (< 0), as qs's exposure."""
    texpo = number(texpo, field)
    if texpo == 0.0:
        raise Invalid("{}: seconds (> 0) or monitor counts (< 0), got 0".format(field))
    return texpo


def number(v, field):
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


def mar_series(name, texpo, geometry="standard", n=1, ti=None, series=None):
    """mymar1 (n = 1, ti None) or mymar (n frames, one every ti minutes)."""
    kwargs = {"name": file_name(name, "filename")}
    texpo = exposure(texpo, "Texpo")
    if texpo > 0 and not 1.0 <= texpo <= 20.0:
        # mymar: "Exposure Time in [sec]: 1 ~ 20".
        raise Invalid("Texpo: seconds 1 to 20, or negative monitor counts, got {:g}".format(texpo))
    kwargs["texpo"] = texpo
    if geometry not in GEOMETRIES:
        raise Invalid("geometry: one of {}, got {!r}".format(", ".join(GEOMETRIES), geometry))
    kwargs["geometry"] = geometry
    kwargs["n"] = n = count(n, "N")
    if ti is not None:
        ti = number(ti, "Ti")
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


def insitu_step(name=None, align=True, th_list=TH_LIST, texpo=IN_TEXPO, series=None, T=None, ramp=None,
                wait=False, tolerance=None, soak=None):
    """One row of the in-situ table: myinsitu's set_ramp → settemp (changec
    with wait) → loopscan → align_in.mac → mymar_in. `tolerance` goes with
    wait only, as qs takes it."""
    kwargs = {"align": bool(align)}
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
    if not align and not th_list and not temp:
        raise Invalid("the row does nothing: no alignment, no th list, no temperature")
    md = series_md(series)
    if md:
        kwargs["md"] = md
    return plan_item("insitu_step", **kwargs)


# An in-situ table row, as a new row shows it: None is an empty cell, and
# th_list is a list or the "a, b c" text of its cell.
NEW_ROW = {"T": None, "ramp": None, "wait": False, "soak_n": None, "soak_t": None, "tolerance": TOLERANCE,
           "align": True, "th_list": TH_LIST, "texpo": IN_TEXPO, "name": ""}


def _blank(v):
    return v is None or (isinstance(v, str) and not v.strip())


def insitu_row(row, series):
    """One in-situ table row as its insitu_step item. A key left out takes
    NEW_ROW's value, a blank cell is None, and the tolerance counts only
    with wait, as the table always shows one."""
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
    return insitu_step(
        name=get("name"), align=row["align"], th_list=numbers(th, "th list") if isinstance(th, str) else th,
        texpo=row["texpo"].strip() if isinstance(row["texpo"], str) else row["texpo"], series=series,
        T=get("T"), ramp=get("ramp"), wait=wait, tolerance=get("tolerance") if wait else None,
        soak=None if soak_n is None else (soak_n, soak_t))


def insitu_items(rows, series=None):
    """The in-situ table as items, in its order and all in one series:
    `series`, or when empty the first row name there is. Invalid names
    the first bad row."""
    rows = list(rows)
    if not rows:
        raise Invalid("the table has no rows")
    series = (series or "").strip()
    if not series:
        series = next((n for n in (str(r.get("name") or "").strip() for r in rows) if n), "")
        if not series:
            raise Invalid("Series: empty, and no row has a name to take it from")
    out = []
    for k, row in enumerate(rows, 1):
        try:
            out.append(insitu_row(row, series))
        except Invalid as e:
            raise Invalid("row {}: {}".format(k, e))
    return out


def cooling_row():
    """myinsitu's last row: set_ramp 25; settemp 025; loopscan 3600 — no
    alignment, no exposure."""
    return {"T": 25.0, "ramp": 25.0, "wait": False, "soak_n": 3600, "soak_t": 1.0, "align": False,
            "th_list": [], "name": ""}


def align(plan, series=None):
    if plan not in ("align_coin", "align_film"):
        raise Invalid("alignment: align_coin or align_film, got {!r}".format(plan))
    md = series_md(series)
    return plan_item(plan, md=md) if md else plan_item(plan)


def describe(item):
    """(plan, name, parameters, folder, user) for the queue's cells."""
    kw = item.get("kwargs") or {}
    md = kw.get("md") if isinstance(kw.get("md"), dict) else {}
    plan = str(item.get("name") or "")
    shown = []
    for k in ("T", "ramp", "wait", "tolerance", "soak", "align", "n", "ti", "texpo", "geometry", "th_list"):
        if k in kw:
            v = kw[k]
            if isinstance(v, list):
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
