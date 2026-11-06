#******************************************************************************
#
#  @(#)DataStatistics.py	3.3  01/15/20 CSS
#
#  "splot" Release 3
#
#  Copyright (c) 2013,2014,2015,2016,2018,2020
#  by Certified Scientific Software.
#  All rights reserved.
#
#  Permission is hereby granted, free of charge, to any person obtaining a
#  copy of this software ("splot") and associated documentation files (the
#  "Software"), to deal in the Software without restriction, including
#  without limitation the rights to use, copy, modify, merge, publish,
#  distribute, sublicense, and/or sell copies of the Software, and to
#  permit persons to whom the Software is furnished to do so, subject to
#  the following conditions:
#
#  The above copyright notice and this permission notice shall be included
#  in all copies or substantial portions of the Software.
#
#  Neither the name of the copyright holder nor the names of its contributors
#  may be used to endorse or promote products derived from this software
#  without specific prior written permission.
#
#     * The software is provided "as is", without warranty of any   *
#     * kind, express or implied, including but not limited to the  *
#     * warranties of merchantability, fitness for a particular     *
#     * purpose and noninfringement.  In no event shall the authors *
#     * or copyright holders be liable for any claim, damages or    *
#     * other liability, whether in an action of contract, tort     *
#     * or otherwise, arising from, out of or in connection with    *
#     * the software or the use of other dealings in the software.  *
#
#******************************************************************************


"""
This module implements several mathematical functions:
    - peak search
    - center of mass search
    - fwhm search
"""
from CSSLogger import log

import numpy

def calc_peak(xdata, ydata):

    maxv = ydata.max()

    if not numpy.isnan(maxv):
        imax = ydata.tolist().index(maxv)
        xmax = xdata[imax]
        return xmax, maxv, imax
    else:
        return [0, ] * 3


def calc_com(xdata, ydata):

    ysum = ydata.sum()

    if ysum:
        com = (xdata * ydata).sum() / ysum
    else:
        com = 0

    return com


def calc_fwhm(xdata, ydata):

    lhmx = calc_hmx(xdata, ydata, -1)
    uhmx = calc_hmx(xdata, ydata, 1)

    xpk,ypk,ipk = calc_peak(xdata,ydata)

    fwhm = abs(uhmx - lhmx)

    if lhmx != xpk and uhmx != xpk:
       cfwhm = (uhmx + lhmx) / 2
    elif uhmx == xpk:
       cfwhm = lhmx
    else:
       cfwhm = uhmx

    return fwhm, cfwhm


def calc_lhmx(xdata, ydata, hm=None, ihm=None):

    if hm is None or ihm is None:
        xmax, hm, ihm = calc_peak(xdata, ydata)

    hm2 = hm / 2
    idx = ihm

    # Going left from the maximum

    while ydata[idx] >= hm2 and idx > -1:
        idx = idx - 1

    if idx > -1:
        x0 = xdata[idx]
        x1 = xdata[idx + 1]
        y0 = ydata[idx]
        y1 = ydata[idx + 1]

        if y1 == y0:
            lhmx = 0
        else:
           # interpolate
            lhmx = (hm2 * (x1 - x0) - (y0 * x1) + (y1 * x0)) / (y1 - y0)
    else:
        lhmx = xdata[0]

    return lhmx


def calc_uhmx(xdata, ydata, hm=None, ihm=None):

    if hm is None or ihm is None:
        xmax, hm, ihm = calc_peak(xdata, ydata)

    hm2 = hm / 2
    idx = ihm

    no_ys = len(ydata)

    while idx < no_ys and ydata[idx] >= hm2:
        idx = idx + 1

    if idx < no_ys:
        x0 = xdata[idx]
        x1 = xdata[idx + 1]
        y0 = ydata[idx]
        y1 = ydata[idx + 1]

        if y1 == y0:
            uhmx = 0
        else:
            uhmx = (hm2 * (x1 - x0) - (y0 * x1) + (y1 * x0)) / (y1 - y0)
    else:
        uhmx = xdata[-1]

    return uhmx


def calc_hmx(xdata, ydata, direction):

    npts = len(ydata)

    y1 = ydata.max()
    i = _calc_index(y1, ydata) + direction

    # Maximum is last and going positive
    if i == npts:
        return xdata[-1]

    # Maximum is first and going negative
    if i == -1:
        return xdata[0]

    x0 = x1 = xdata[i]

    half = y1 / 2
    start = 0

    while(1):

        if direction > 0 and i >= npts:  # we could not find half the height of max
            return(x0)

        if direction < 0 and i < start:
            return(x0)

        x = xdata[i]
        y = ydata[i]

        if y <= half:  # found it
            dy = y1 - y
            if dy != 0:
                x1 -= (x1 - x) * (y1 - half) / dy

            return(x1)

        x1 = x
        y1 = y

        i += direction


def _calc_index(elem, array):
    """
    Return the index of elem in array
    """
    if numpy.isnan(elem):
        return 0
    mylist = array.tolist()
    return mylist.index(elem)


if __name__ == '__main__':

    import sys

    if len(sys.argv) < 2:
        print("Usage: %s spec cnt_num" % sys.argv[0])
        sys.exit(0)

    spec = sys.argv[1]
    cntnum = int(sys.argv[2])
    from SpecClient import SpecVariable

    scand = SpecVariable.SpecVariable("SCAN_D", spec)
    data = scand.getValue()
    npts = SpecVariable.SpecVariable("NPTS", spec).getValue()

    xdata = data[0:npts, 0]
    ydata = data[0:npts, cntnum + 1]

    com = calc_com(xdata, ydata)
    print(" COM: ", com)
    print("PEAK: ", str(calc_peak(xdata, ydata)))
    print("FWHM: ", str(calc_fwhm(xdata, ydata)))
