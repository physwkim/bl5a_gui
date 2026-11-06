"""The peak statistics SPEC's macros read after a scan (CEN, pl_COM,
pl_xMAX): qs/src/peak.rs in Python, itself a port of splot's
DataStatistics.py 3.3, NaN behaviour included."""

import math
from collections import namedtuple

import numpy as np

Stats = namedtuple("Stats", "xpk ymax com cen fwhm")


def _max(y):
    """numpy max: NaN if any element is."""
    m = -math.inf
    for v in y:
        if math.isnan(m) or math.isnan(v):
            m = math.nan
        else:
            m = max(m, v)
    return m


def _index(v, y):
    """The first index of v, 0 for NaN."""
    return 0 if math.isnan(v) else y.index(v)


def _hmx(x, y, d):
    """The half-maximum crossing walking from the maximum in direction d,
    interpolated against the last point above half; the first neighbour
    when half is never reached."""
    npts = len(y)
    y1 = _max(y)
    i = _index(y1, y) + d
    if i == npts:
        return x[-1]
    if i == -1:
        return x[0]
    x0 = x1 = x[i]
    half = y1 / 2.0
    while True:
        if (d > 0 and i >= npts) or (d < 0 and i < 0):
            return x0
        xi, yi = x[i], y[i]
        if yi <= half:
            dy = y1 - yi
            if dy != 0.0:
                x1 -= (x1 - xi) * (y1 - half) / dy
            return x1
        x1, y1 = xi, yi
        i += d


def stats(x, y):
    """splot's statistics of y against x; None for an empty scan."""
    x = [float(v) for v in x]
    y = [float(v) for v in y]
    if len(x) != len(y):
        raise ValueError("x and y differ in length")
    if not y:
        return None
    m = _max(y)
    xpk, ymax = (0.0, 0.0) if math.isnan(m) else (x[_index(m, y)], m)
    lower, upper = _hmx(x, y, -1), _hmx(x, y, 1)
    if lower != xpk and upper != xpk:
        cen = (upper + lower) / 2.0
    elif upper == xpk:
        cen = lower
    else:
        cen = upper
    # numpy's pairwise sums, as splot's calc_com; a NaN sum is truthy.
    ax, ay = np.asarray(x), np.asarray(y)
    total = float(np.sum(ay))
    com = float(np.sum(ax * ay)) / total if total != 0.0 else 0.0
    return Stats(xpk, ymax, com, cen, abs(upper - lower))
