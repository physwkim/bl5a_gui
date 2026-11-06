"""docs.Documents on document streams bl5a-qs sent against bl5a-sim
(recorded with qs_t/drive.py, one `["name", doc]` per line): the scans the
GUI rebuilds give the statistics qs picked its targets from, the history
row is what the run left in SPEC, and every exposure reaches its run."""

import gzip
import json
import math
import pathlib
import unittest

from bl5a_gui import docs

DATA = pathlib.Path(__file__).resolve().parent / "data"

# th's user offset change over each recorded run, read from SPEC rather
# than from the documents: `wm th` before and after prints th's low limit
# in user units, which moves by the offset change, to 4 decimals.
WM_DTH0 = {"insitu_step.jsonl.gz": 0.0840, "align_coin.jsonl.gz": None}


def load(name):
    with gzip.open(DATA / name, "rt") as f:
        return [json.loads(line) for line in f]


class Replay:
    def __init__(self, name):
        self.stream = load(name)
        self.stages, self.scans, self.stops, self.exposures = [], [], [], []
        d = docs.Documents(
            on_stage=lambda run, stage, scan: self.stages.append((run, stage, scan)),
            on_scan_stop=self.scans.append,
            on_exposure=lambda run, e: self.exposures.append((run, e)),
            on_run_stop=lambda run, row: self.stops.append((run, row)))
        for n, doc in self.stream:
            d.feed(n, doc)
        self.docs = d

    def raw(self, stream):
        """The data of every event of the stream named so, straight from
        the documents."""
        names = {doc["uid"]: doc.get("name") for n, doc in self.stream if n == "descriptor"}
        return [doc["data"] for n, doc in self.stream if n == "event" and names.get(doc["descriptor"]) == stream]

    def plan_run(self, plan):
        (run, row), = [(run, row) for run, row in self.stops if run.plan == plan]
        return run, row


def same(a, b):
    return (math.isnan(a) and math.isnan(b)) or math.isclose(a, b, rel_tol=1e-12, abs_tol=1e-12)


class Recorded:
    fixture = None
    plan = None

    @classmethod
    def setUpClass(cls):
        cls.r = Replay(cls.fixture)

    def test_every_run_closes(self):
        self.assertEqual(self.r.docs.runs, {})
        self.assertEqual(self.r.docs.descriptors, {})

    def test_exposures_reach_the_plan_run(self):
        run, _ = self.r.plan_run(self.plan)
        self.assertEqual([e for _, e in self.r.exposures], self.r.raw("exposures"))
        self.assertTrue(all(r is run for r, _ in self.r.exposures))
        self.assertEqual(run.exposures, self.r.raw("exposures"))


class Alignment(Recorded):
    def test_one_stage_per_scan(self):
        self.assertTrue(self.r.stages)
        self.assertEqual([s for _, s, _ in self.r.stages], self.r.raw("stages"))
        self.assertEqual(len(self.r.stages), len(self.r.scans))

    def test_stage_statistics_from_the_rebuilt_scan(self):
        for _, stage, scan in self.r.stages:
            with self.subTest(stage=stage["stage"]):
                self.assertEqual(scan.motor, stage["motor"])
                self.assertTrue(stage["stage"].startswith(scan.title), scan.title)
                got = scan.stats()
                for key, field in (("xmax", "xpk"), ("ymax", "ymax"), ("com", "com"), ("cen", "cen"),
                                   ("fwhm", "fwhm")):
                    self.assertTrue(same(getattr(got, field), stage[key]), (key, getattr(got, field), stage[key]))

    def test_history_row(self):
        run, row = self.r.plan_run(self.plan)
        last = {s["motor"]: s["after"] for _, s, _ in self.r.stages}
        final = (self.r.raw("final") or [{}])[-1]
        self.assertEqual(row["exit"], "success")
        self.assertEqual(row["series"], run.start["series"])
        for m in ("sy", "shold"):
            self.assertEqual(row[m], final.get(m, last.get(m)), m)
        want = WM_DTH0[self.fixture]
        if want is None:
            self.assertIsNone(row["dth0"])
        else:
            self.assertAlmostEqual(row["dth0"], want, delta=1e-4)


class InsituReplayTest(Alignment, unittest.TestCase):
    """insitu_step on the film: align_in's stages, then mymar_in's
    exposures."""
    fixture = "insitu_step.jsonl.gz"
    plan = "insitu_step"


class CoinReplayTest(Alignment, unittest.TestCase):
    fixture = "align_coin.jsonl.gz"
    plan = "align_coin"


class MarSeriesReplayTest(Recorded, unittest.TestCase):
    """A battery series has no stages: no history row."""
    fixture = "mar_series.jsonl.gz"
    plan = "mar_series"

    def test_no_history_row(self):
        _, row = self.r.plan_run(self.plan)
        self.assertIsNone(row)
        self.assertEqual(self.r.stages, [])


class FailedRunTest(unittest.TestCase):
    """A plan error closes every open run with the error as the reason
    (bsrs drain_and_close): the scan's and the plan run's stop carry it."""

    def test_reason_reaches_scan_run_and_history(self):
        stops, scans = [], []
        d = docs.Documents(on_scan_stop=scans.append, on_run_stop=lambda run, row: stops.append((run, row)))
        reason = "spec: Two Theta Angle is too low for UPWARD Geometry."
        d.feed("start", {"uid": "p", "plan_name": "align_film", "series": "s", "time": 1.0})
        d.feed("descriptor", {"uid": "pd", "name": "stages", "run_start": "p"})
        d.feed("event", {"descriptor": "pd", "data": {"stage": "lup sy", "motor": "sy", "after": 0.1}})
        d.feed("start", {"uid": "s", "plan_name": "lup", "motors": ["th"], "det": "det"})
        d.feed("stop", {"run_start": "s", "exit_status": "fail", "reason": reason})
        d.feed("stop", {"run_start": "p", "exit_status": "fail", "reason": reason})
        (scan,) = scans
        ((run, row),) = stops
        self.assertEqual((scan.exit_status, scan.reason), ("fail", reason))
        self.assertEqual((run.exit_status, run.reason), ("fail", reason))
        self.assertEqual((row["exit"], row["reason"]), ("fail", reason))
        self.assertEqual(docs.outcome(run.exit_status, run.reason), "fail: " + reason)

    def test_success_has_no_reason(self):
        r = Replay("align_coin.jsonl.gz")
        run, row = r.plan_run("align_coin")
        self.assertEqual((run.reason, row["reason"]), ("", ""))
        self.assertEqual(docs.outcome(run.exit_status, run.reason), "success")


if __name__ == "__main__":
    unittest.main()
