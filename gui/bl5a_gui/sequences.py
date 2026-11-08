"""Alignment sequences: the macros' two (templates/) and each user's own,
kept as <data folder>/alignments/<user>/<name>.json, `{"blocks": [...]}`
as the templates are. A sequence is checked with items.sequence_blocks
when read and when saved. No Qt.

A block's place is its path: its index in the top list, then in each
loop's `blocks` down to it."""

import copy
import json
import os
import pathlib
import tempfile
import unicodedata

from . import items
from .items import Invalid

HERE = pathlib.Path(__file__).resolve().parent / "templates"
# The macros' sequences: coin_align's and align_in.mac's. Read-only.
TEMPLATES = {"Coin cell": HERE / "coin.json", "Thin film": HERE / "film.json"}
# What an in-situ row runs when no other sequence is picked.
DEFAULT = "Thin film"
FOLDER = "alignments"
NAME_MAX = 64


def checked_name(text, field):
    """A user's or a sequence's name as one file name."""
    text = (text or "").strip()
    if not text or text.startswith(".") or len(text) > NAME_MAX \
            or any(c in "/\\" or unicodedata.category(c).startswith("C") for c in text):
        raise Invalid("{}: 1 to {} characters, not starting with '.', without '/', '\\' or control "
                      "characters, got {!r}".format(field, NAME_MAX, text))
    return text


def folder(root, user):
    """Where `user`'s sequences are kept under the data folder `root`."""
    root = items.data_root(root)
    if root is None:
        raise Invalid("Data folder: not set; a user's sequences are kept in <data folder>/{}".format(FOLDER))
    return pathlib.Path(root, FOLDER, checked_name(user, "user"))


def _read(path, label):
    try:
        with open(path, encoding="utf-8") as f:
            doc = json.load(f)
    except (OSError, ValueError) as e:
        raise Invalid("{}: {}".format(label, e))
    if not isinstance(doc, dict) or set(doc) != {"blocks"}:
        raise Invalid("{}: not {{\"blocks\": [...]}}".format(label))
    try:
        return items.sequence_blocks(doc["blocks"])
    except Invalid as e:
        raise Invalid("{}: {}".format(label, e))


def users(root):
    """The users with a folder under `root`, sorted; none when `root` is
    unset or has no alignments yet."""
    try:
        root = items.data_root(root)
        return sorted(p.name for p in pathlib.Path(root, FOLDER).iterdir() if p.is_dir()) if root else []
    except (Invalid, OSError):
        return []


def saved(root, user):
    """`user`'s sequence names, sorted; none when `root` or `user` is
    unset or the folder is not there."""
    try:
        if items.data_root(root) is None or not (user or "").strip():
            return []
        return sorted(p.stem for p in folder(root, user).glob("*.json") if p.is_file())
    except (Invalid, OSError):
        return []


def names(root, user):
    """The templates, then `user`'s saved sequences."""
    return list(TEMPLATES) + saved(root, user)


def get(name, root=None, user=None):
    """The blocks of the template or of `user`'s sequence `name`."""
    if name in TEMPLATES:
        return _read(TEMPLATES[name], name)
    path = folder(root, user) / (checked_name(name, "sequence") + ".json")
    if not path.is_file():
        raise Invalid("sequence {!r}: not a template, nor saved by {}".format(name, user))
    return _read(path, str(path))


def save(name, value, root, user):
    """Keep `value` as `user`'s sequence `name`, replacing one saved so;
    the path written."""
    name = checked_name(name, "sequence")
    if name in TEMPLATES:
        raise Invalid("sequence: {!r} is a template's name; save under another".format(name))
    value = items.sequence_blocks(value)
    where = folder(root, user)
    where.mkdir(parents=True, exist_ok=True)
    path = where / (name + ".json")
    fd, tmp = tempfile.mkstemp(dir=where, prefix=".", suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump({"blocks": value}, f, indent=1, ensure_ascii=False)
            f.write("\n")
        os.replace(tmp, path)
    except BaseException:
        os.unlink(tmp)
        raise
    return path


def delete(name, root, user):
    if name in TEMPLATES:
        raise Invalid("sequence: {!r} is a template".format(name))
    path = folder(root, user) / (checked_name(name, "sequence") + ".json")
    try:
        path.unlink()
    except FileNotFoundError:
        raise Invalid("sequence {!r}: not saved by {}".format(name, user))


def _g(v):
    return "{:g}".format(v)


def text(b):
    """One block as a line: what it does, in the macros' words."""
    kind = b["kind"]
    if kind == "scan":
        # As the Stages table names the scan: SPEC's argument order.
        if items.SCAN_TYPES[b["type"]]:
            how = "{} {}".format(_g(b["time"]), _g(b["sleep"]))
        else:
            how = "{} {}".format(b["intervals"], _g(b["time"]))
        line = "{} {} {} {} {}  {} → {}".format(b["type"], b["motor"], _g(b["start"]), _g(b["finish"]), how,
                                                b["counter"], b["pick"])
        if b.get("dip"):
            line += " dip"
        if b.get("set") is not None:
            line += ", set {}".format(_g(b["set"]))
    elif kind == "move":
        line = "umv " + " ".join("{} {}".format(m, _g(x)) for m, x in b["moves"])
    elif kind == "spec":
        line = b["command"].strip()
    elif kind == "wait":
        line = "wait {} s".format(_g(b["seconds"]))
    elif kind == "erase":
        line = "erase the mar345 plate"
    else:
        line = "loop × {}".format(b["times"])
    w = b.get("when")
    if w:
        line = "if {} {} {}: {}".format(w["motor"], w["op"], _g(w["value"]), line)
    return line


def place(path):
    """A path as the checker's errors count it, from 1: (2, 1) is "3.2",
    block 2 of block 3."""
    return ".".join(str(i + 1) for i in path)


def lines(blocks, path=()):
    """Every block as "<place>  <text>", a loop's own after it, indented."""
    out = []
    for i, b in enumerate(blocks):
        p = path + (i,)
        out.append("  " * len(path) + place(p) + "  " + text(b))
        if b["kind"] == "loop":
            out += lines(b["blocks"], p)
    return out


def siblings(blocks, parent):
    """The list of the blocks under the loop at `parent`; () is the top."""
    for i in parent:
        blocks = blocks[i]["blocks"]
    return blocks


# Edits of `blocks` in place: neighbours `lo` to `hi` (inclusive) in the
# list under `parent`. Each returns the paths to select after it.

def insert(blocks, parent, at, block):
    siblings(blocks, parent).insert(at, block)
    return [parent + (at,)]


def duplicate(blocks, parent, lo, hi):
    run = siblings(blocks, parent)
    run[hi + 1:hi + 1] = copy.deepcopy(run[lo:hi + 1])
    return [parent + (i,) for i in range(hi + 1, 2 * hi - lo + 2)]


def remove(blocks, parent, lo, hi):
    """A loop left with no blocks goes too: it would run nothing."""
    del siblings(blocks, parent)[lo:hi + 1]
    while parent and not siblings(blocks, parent):
        parent, lo = parent[:-1], parent[-1]
        del siblings(blocks, parent)[lo]
    left = len(siblings(blocks, parent))
    return [parent + (min(lo, left - 1),)] if left else []


def move(blocks, parent, lo, hi, step):
    """One place up (step -1) or down (+1) among their neighbours; at the
    end of the list they stay."""
    run = siblings(blocks, parent)
    if not (0 <= lo + step and hi + step < len(run)):
        return None
    if step < 0:
        run[lo - 1:hi + 1] = run[lo:hi + 1] + [run[lo - 1]]
    else:
        run[lo:hi + 2] = [run[hi + 1]] + run[lo:hi + 1]
    return [parent + (i + step,) for i in range(lo, hi + 1)]


def group(blocks, parent, lo, hi, times):
    """The blocks as one loop, run `times` over, where they were."""
    run = siblings(blocks, parent)
    run[lo:hi + 1] = [{"kind": "loop", "times": times, "blocks": run[lo:hi + 1]}]
    return [parent + (lo,)]


def ungroup(blocks, path):
    """The loop's blocks in its place; its times, when and note go."""
    parent, at = path[:-1], path[-1]
    run = siblings(blocks, parent)
    inner = run[at]["blocks"]
    run[at:at + 1] = inner
    return [parent + (i,) for i in range(at, at + len(inner))]
