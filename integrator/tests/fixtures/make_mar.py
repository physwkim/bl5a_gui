"""Writes overflow.mar128 with fabio, a mar345 writer independent of the
ad-mar345 driver. Pixel k = 5k mod 70001 crosses 32767 (the pck sign bit)
and 65535 (893 overflow records, so the eight-record padding is used).
fabio's writer garbles neighbour steps beyond 16 bits, hence a ramp.

Usage: python -I make_mar.py OUT
"""
import sys

import fabio.mar345image
import numpy as np

n = 128
data = (np.arange(n * n, dtype=np.int64) * 5 % 70001).reshape(n, n).astype(np.int32)
img = fabio.mar345image.Mar345Image(data=data, header={"PIXEL_LENGTH": 150, "PIXEL_HEIGHT": 150})
# The default byte order "|" is not one get_stype accepts.
img.byteorder = "<"
img.write(sys.argv[1])
back = fabio.open(sys.argv[1]).data
assert np.array_equal(back.astype(np.int64), data), "fabio round trip"
print("overflow pixels", int((data > 65535).sum()))
