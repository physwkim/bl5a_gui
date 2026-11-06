from silx.gui import icons
from silx.gui.plot.PlotWindow import PlotWindow
from silx.gui.plot.CurvesROIWidget import ROI
from silx.gui.plot.actions.control import ZoomBackAction

import logging

_logger = logging.getLogger(__name__)

class Plot1DCustom(PlotWindow):
    """PlotWindow with tools specific for curves.

    This widgets provides the plot API of :class:`.PlotWidget`.

    :param parent: The parent of this widget
    :param backend: The backend to use for the plot (default: matplotlib).
                    See :class:`.PlotWidget` for the list of supported backend.
    :type backend: str or :class:`BackendBase.BackendBase`
    """

    def __init__(self, parent=None, backend=None):
        super(Plot1DCustom, self).__init__(parent=parent, backend=backend,
                                           resetzoom=True, autoScale=False,
                                           logScale=True, grid=True,
                                           curveStyle=True, colormap=False,
                                           aspectRatio=False, yInverted=False,
                                           copy=True, save=True, print_=True,
                                           control=True, position=True,
                                           roi=False, mask=False, fit=False)

        # Retrieve PlotWidget's plot area widget
        plotArea = self.getWidgetHandle()

        self.setDefaultPlotPoints(True)
        self.getGridAction().setChecked(True)
        self.setGraphGrid(True)

    def resetZoom(self, dataMargins=None):
        """Reset the plot limits to the bounds of the data and redraw the plot.

        It automatically scale limits of axes that are in autoscale mode
        (see :meth:`getXAxis`, :meth:`getYAxis` and :meth:`Axis.setAutoScale`).
        It keeps current limits on axes that are not in autoscale mode.

        Extra margins can be added around the data inside the plot area
        (see :meth:`setDataMargins`).
        Margins are given as one ratio of the data range per limit of the
        data (xMin, xMax, yMin and yMax limits).
        For log scale, extra margins are applied in log10 of the data.

        :param dataMargins: Ratios of margins to add around the data inside
                            the plot area for each side (default: no margins).
        :type dataMargins: A 4-tuple of float as (xMin, xMax, yMin, yMax).

        Changed zoom history to be deleted when resetZoom is called.
        """
        xLimits = self._xAxis.getLimits()
        yLimits = self._yAxis.getLimits()
        y2Limits = self._yRightAxis.getLimits()

        xAuto = self._xAxis.isAutoScale()
        yAuto = self._yAxis.isAutoScale()

        # With log axes, autoscale if limits are <= 0
        # This avoids issues with toggling log scale with matplotlib 2.1.0
        if self._xAxis.getScale() == self._xAxis.LOGARITHMIC and xLimits[0] <= 0:
            xAuto = True
        if self._yAxis.getScale() == self._yAxis.LOGARITHMIC and (yLimits[0] <= 0 or y2Limits[0] <= 0):
            yAuto = True

        if not xAuto and not yAuto:
            _logger.debug("Nothing to autoscale")
        else:  # Some axes to autoscale
            self._forceResetZoom(dataMargins=dataMargins)

            # Restore limits for axis not in autoscale
            if not xAuto and yAuto:
                self.setGraphXLimits(*xLimits)
            elif xAuto and not yAuto:
                if y2Limits is not None:
                    self.setGraphYLimits(
                        y2Limits[0], y2Limits[1], axis='right')
                if yLimits is not None:
                    self.setGraphYLimits(yLimits[0], yLimits[1], axis='left')

        if (xLimits != self._xAxis.getLimits() or
                yLimits != self._yAxis.getLimits() or
                y2Limits != self._yRightAxis.getLimits()):
            self._notifyLimitsChanged()

        # Changed Zoom history to be deleted when resetZoom is called.
        self._limitsHistory.clear()
