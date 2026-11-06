import math
import pathlib
import unittest

import numpy as np

from bl5a_gui import mar

SIM_PONI = pathlib.Path(__file__).resolve().parents[2] / "sim" / "bl5a.poni"


def closed_form(poni, shape, pixel):
    """q of each pixel centre with no rotations: the beam is the plate's
    normal through the PONI, so 2θ = atan(r / L)."""
    inset = (mar.PLATE - shape[0] * pixel) / 2.0
    i = (np.arange(shape[0]) + 0.5) * pixel - (poni["poni1"] - inset)
    j = (np.arange(shape[1]) + 0.5) * pixel - (poni["poni2"] - inset)
    r = np.hypot(*np.meshgrid(i, j, indexing="ij"))
    return 4e-10 * math.pi / poni["wavelength"] * np.sin(np.arctan2(r, poni["distance"]) / 2.0)


class ParsePoniTest(unittest.TestCase):
    def test_sim_poni(self):
        p = mar.parse_poni(SIM_PONI.read_text())
        self.assertEqual(p, {"distance": 0.3, "poni1": 0.1731, "poni2": 0.1719, "rot1": 0.004, "rot2": -0.003,
                             "rot3": 0.0, "wavelength": 7.749e-11, "orientation": 3})

    def test_missing_key(self):
        text = "\n".join(line for line in SIM_PONI.read_text().splitlines() if not line.startswith("Wavelength"))
        with self.assertRaisesRegex(ValueError, "wavelength"):
            mar.parse_poni(text)


class QMapTest(unittest.TestCase):
    def setUp(self):
        self.poni = dict(mar.parse_poni(SIM_PONI.read_text()), rot1=0.0, rot2=0.0, rot3=0.0)

    def test_no_rotation_is_closed_form(self):
        # Both scan sizes: the inset moves the origin for the smaller one.
        for shape, pixel in (((3450, 3450), 1e-4), ((1200, 1200), 1.5e-4)):
            got = mar.q_map(self.poni, shape, pixel)
            want = closed_form(self.poni, shape, pixel)
            np.testing.assert_allclose(got, want, rtol=1e-12, atol=1e-12)

    def test_step_takes_every_step_th_pixel(self):
        full = mar.q_map(self.poni, (3450, 3450), 1e-4)
        np.testing.assert_array_equal(mar.q_map(self.poni, (3450, 3450), 1e-4, step=8), full[::8, ::8])

    def test_rings_lie_on_their_radius(self):
        shape, pixel = (3450, 3450), 1e-4
        L, lam = self.poni["distance"], self.poni["wavelength"]
        for q, lines in mar.rings(self.poni, shape, pixel, (2.687, 3.103)).items():
            radius = L * math.tan(2.0 * math.asin(q * lam * 1e10 / (4.0 * math.pi))) / pixel
            self.assertTrue(lines)
            for cols, rows in lines:
                # Pixel coordinates of the PONI: the image draws pixel k's
                # centre at k + 0.5.
                r = np.hypot(rows - self.poni["poni1"] / pixel, cols - self.poni["poni2"] / pixel)
                np.testing.assert_allclose(r, radius, atol=0.05)


if __name__ == "__main__":
    unittest.main()
