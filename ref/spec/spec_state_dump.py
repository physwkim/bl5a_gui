"""Dump macro definitions from a SPEC state file (userfiles/<user>_ttyp#S).

The layout is undocumented; this follows what the BL5A files show:
  u32 flags, u32 strlen(body), u32 len(name)+1, name\\0, u32 len(body)+1, body\\0,
  u32 trailer
and, when flags & 4 (macro function), i32 link (-1: no arguments) and 8 zero
bytes, then one record per argument: u32 len(arg)+1, arg\\0, i32 -2, i32 link
(-1 on the last), 8 zero bytes. Variables share the name/value framing but
not the strlen field, so they are skipped. An entry is printed only when every length and terminator agrees;
function entries whose argument list does not parse are reported on stderr.

With --globals it prints the global variables instead, as SPEC source that
restores them. A variable record is
  u32 flags, f64 number, u32 len(name)+1, name\\0, value
where value is i32 -2 (no string) or u32 len(string)+1, string\\0. Flags:
0x01 number, 0x02 string (both: a string), 0x04 constant, 0x40 global,
0x200 unset, 0x400 associative array, 0x800 array element, 0x1000 data
array, 0x200000 shared. An associative array's elements (element records,
key in the name field) follow it up to i32 -1 and 8 zero bytes; a data
array keeps only u32 rows, u32 cols, u32 element size, u32 type, not its
data, so it comes back zeroed.
"""
import re
import struct
import sys

GLOBALS = sys.argv[1:2] == ["--globals"]
data = open(sys.argv[-1], "rb").read()
NAME = re.compile(rb"([\x02-\x7f])\x00\x00\x00([A-Za-z_][A-Za-z0-9_]*)\x00")


def u32(p):
    return struct.unpack_from("<I", data, p)[0]


def i32(p):
    return struct.unpack_from("<i", data, p)[0]


def parse_args(p):
    """Argument records starting at p, or None if they do not parse."""
    link = i32(p)
    if data[p + 4 : p + 12] != bytes(8) or link not in (0, -1):
        return None
    p += 12
    args = []
    if link == -1:
        return args
    while p + 4 <= len(data):
        alen = u32(p)
        if not 2 <= alen <= 64:
            return None
        arg = data[p + 4 : p + 4 + alen]
        if arg[-1] != 0 or not re.fullmatch(rb"[A-Za-z_][A-Za-z0-9_]*", arg[:-1]):
            return None
        q = p + 4 + alen
        if i32(q) != -2 or data[q + 8 : q + 16] != bytes(8):
            return None
        args.append(arg[:-1].decode())
        link = i32(q + 4)
        p = q + 16
        if link == -1:
            return args
        if link != 0:
            return None
    return None


def f64(p):
    return struct.unpack_from("<d", data, p)[0]


def cstr(p):
    """(bytes, next) of a u32 len+1, bytes, NUL field at p, or None."""
    n = u32(p)
    if n == 0 or n > 1 << 20 or p + 4 + n > len(data) or data[p + 3 + n] != 0:
        return None
    s = data[p + 4 : p + 3 + n]
    if b"\0" in s:
        return None
    return s, p + 4 + n


def var_value(p, flags, num):
    """(value, next) of the value field at p: num or a str, or None."""
    if i32(p) == -2:
        if flags & 2 and not flags & 0x200:
            return None
        return num, p + 4
    r = cstr(p)
    if r is None:
        return None
    return r[0].decode("latin-1"), r[1]


def var_record(p):
    """(name, flags, value) of the global variable record at p, or None.
    value: a number, a str, None (unset), [(key, value)] or (rows, cols)."""
    flags = u32(p)
    r = cstr(p + 12)
    if r is None:
        return None
    name, q = r
    name = name.decode()
    if flags & 0x1000:
        return name, flags, (u32(q), u32(q + 4))
    if flags & 0x400:
        elems = []
        while not (i32(q) == -1 and data[q + 4 : q + 12] == bytes(8)):
            ef = u32(q)
            if ef & 0xFFFFF000 or not ef & 0x800 or not ef & 3:
                return None
            num = f64(q + 4)
            r = cstr(q + 12)
            if r is None:
                return None
            key, q = r
            v = var_value(q, ef, num)
            if v is None:
                return None
            elems.append((key.decode("latin-1"), v[0]))
            q = v[1]
        return name, flags, elems
    v = var_value(q, flags, f64(p + 4))
    if v is None:
        return None
    return name, flags, None if flags & 0x200 else v[0]


def spec_str(s):
    out = []
    for c in s:
        if c in '"\\':
            out.append("\\" + c)
        elif c == "\n":
            out.append("\\n")
        elif c == "\t":
            out.append("\\t")
        elif not " " <= c <= "~":
            out.append("\\%03o" % ord(c))
        else:
            out.append(c)
    return '"' + "".join(out) + '"'


def spec_value(v):
    if isinstance(v, str):
        return spec_str(v)
    if v == int(v) and abs(v) < 1e15:
        return str(int(v))
    return repr(v)


def dump_globals():
    found = {}
    for m in NAME.finditer(data):
        name = m.group(2)
        if len(name) + 1 != m.group(1)[0] or m.start() < 12:
            continue
        p = m.start() - 12
        flags = u32(p)
        if not flags & 0x40 or flags & 0x800 or flags & ~0x201FFF:
            continue
        r = var_record(p)
        if r is None:
            print(f"# {name.decode()}: record not parsed", file=sys.stderr)
            continue
        found.setdefault(r[0], r)
    for name in sorted(found):
        _, flags, v = found[name]
        if flags & 0x1000:
            shared = "shared " if flags & 0x200000 else ""
            print(f"{shared}array {name}[{v[0]}][{v[1]}]")
        elif flags & 4 and flags & 0x400:
            elems = ", ".join(f"{spec_str(k)}:{spec_value(e)}" for k, e in sorted(v))
            print(f"constant {name} = [{elems}]")
        elif flags & 0x400:
            print(f"global {name}[]")
            for key, ev in sorted(v):
                print(f"{name}[{spec_str(key)}] = {spec_value(ev)}")
        elif flags & 4:
            print(f"constant {name} {spec_value(v)}")
        elif v is None:
            print(f"global {name}")
        else:
            print(f"global {name}\n{name} = {spec_value(v)}")
    print(f"# {len(found)} globals", file=sys.stderr)


if GLOBALS:
    dump_globals()
    sys.exit()

seen = {}
for m in NAME.finditer(data):
    name = m.group(2)
    if len(name) + 1 != m.group(1)[0] or m.start() < 8:
        continue
    flags, slen = u32(m.start() - 8), u32(m.start() - 4)
    p = m.end()
    if p + 4 > len(data):
        continue
    blen = u32(p)
    if blen == 0 or blen != slen + 1 or p + 4 + blen > len(data):
        continue
    body = data[p + 4 : p + 4 + blen]
    if body[-1] != 0 or b"\x00" in body[:-1]:
        continue
    args = None
    if flags & 4:
        args = parse_args(p + 4 + blen + 4)
        if args is None:
            print(f"# {name.decode()}: function argument list not parsed", file=sys.stderr)
            continue
    seen[name.decode()] = (args, body[:-1].decode("latin-1"))

for name in sorted(seen):
    args, body = seen[name]
    sig = f"{name}({', '.join(args)})" if args is not None else name
    print(f"def {sig} '{body}'\n")
print(f"# {len(seen)} entries", file=sys.stderr)
