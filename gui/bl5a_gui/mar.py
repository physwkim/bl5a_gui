"""mar345 frames (.mar3450/.mar2300/…) as numpy arrays, and the q of
their pixels under a PONI, the integrator's geometry (integrator/src/
engine.rs). No Qt."""

import math
import struct

import numpy as np
from fabio.ext.mar345_IO import uncompress_pck

# The plate's diameter, m; a smaller scan keeps its centre.
PLATE = 0.345


def read(path):
    """(image, pixel size m) of a mar345 file. fabio's own reader parses the
    whole ASCII header first and takes twice as long; the 4-byte header
    words are all the decoder needs."""
    with open(path, "rb") as f:
        raw = f.read()
    fs = "<i" if struct.unpack("<i", raw[:4])[0] == 1234 else ">i"
    word = lambda k: struct.unpack(fs, raw[4 * k:4 * k + 4])[0]  # noqa: E731
    dim1, high, npx, um = word(1), word(2), word(5), word(6)
    image = uncompress_pck(raw, dim1, npx // dim1, high, None, None, fs[0], use_CCP4=True)
    return image, um * 1e-6


def parse_poni(text):
    """The PONI keys the geometry needs, from a pyFAI v2 PONI text."""
    out = {"orientation": 3}
    for line in text.splitlines():
        if ":" not in line or line.lstrip().startswith("#"):
            continue
        key, value = (s.strip() for s in line.split(":", 1))
        if key in ("Distance", "Poni1", "Poni2", "Rot1", "Rot2", "Rot3", "Wavelength"):
            out[key.lower()] = float(value)
        elif key == "Detector_config" and '"orientation"' in value:
            out["orientation"] = int(value.split('"orientation"')[1].split(":")[1].split("}")[0].split(",")[0])
    missing = {"distance", "poni1", "poni2", "rot1", "rot2", "rot3", "wavelength"} - out.keys()
    if missing:
        raise ValueError("PONI without {}".format(", ".join(sorted(missing))))
    return out


def q_map(poni, shape, pixel, step=1):
    """q (Å⁻¹) at the centre of every `step`-th pixel of a `shape` scan with
    `pixel` m pixels: pyFAI's calc_pos_zyx with the integrator's inset
    origin. Orientation 3 puts pixel (0, 0) at p = (0.5, 0.5) pixel; 1, 2
    and 4 flip only the signs of t1/t2, which q does not see."""
    n1, n2 = shape
    inset = (PLATE - n1 * pixel) / 2.0
    i = (np.arange(0, n1, step) + 0.5) * pixel - (poni["poni1"] - inset)
    j = (np.arange(0, n2, step) + 0.5) * pixel - (poni["poni2"] - inset)
    p1, p2 = np.meshgrid(i, j, indexing="ij")
    L = poni["distance"]
    c1, s1 = math.cos(poni["rot1"]), math.sin(poni["rot1"])
    c2, s2 = math.cos(poni["rot2"]), math.sin(poni["rot2"])
    c3, s3 = math.cos(poni["rot3"]), math.sin(poni["rot3"])
    t1 = p1 * c2 * c3 + p2 * (c3 * s1 * s2 - c1 * s3) - L * (c1 * c3 * s2 + s1 * s3)
    t2 = p1 * c2 * s3 + p2 * (c1 * c3 + s1 * s2 * s3) - L * (-c3 * s1 + c1 * s2 * s3)
    t3 = p1 * s2 - p2 * c2 * s1 + L * c1 * c2
    tth = np.arctan2(np.hypot(t1, t2), t3)
    return 4e-10 * math.pi / poni["wavelength"] * np.sin(tth / 2.0)


def rings(poni, shape, pixel, qs, step=8):
    """Each q's ring as a list of (column, row) polylines in pixel
    coordinates, traced on every `step`-th pixel's q."""
    from silx.image.marchingsquares import find_contours

    q = q_map(poni, shape, pixel, step)
    out = {}
    for value in qs:
        lines = []
        for c in find_contours(q, value):
            # (row, col) in the coarse grid; its point k is pixel k*step,
            # whose centre an image drawn at origin 0, scale 1 has at +0.5.
            rows = c[:, 0] * step + 0.5
            cols = c[:, 1] * step + 0.5
            lines.append((cols, rows))
        out[value] = lines
    return out
