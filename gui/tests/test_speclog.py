"""speclog.LogLines on the Log PV values spec-ioc posts: each the newest
lines, numbered "<run>:<seq>"."""

import unittest

from bl5a_gui.speclog import LogLines


def block(run, first, last):
    return "".join("{}:{} line {}\n".format(run, n, n) for n in range(first, last + 1))


def lines(first, last):
    return ["line {}".format(n) for n in range(first, last + 1)]


class LogLinesTest(unittest.TestCase):
    def test_the_first_value_is_history(self):
        self.assertEqual(LogLines().new(block(5, 40, 42)), lines(40, 42))

    def test_only_unseen_lines(self):
        log = LogLines()
        log.new(block(5, 1, 3))
        self.assertEqual(log.new(block(5, 2, 4)), lines(4, 4))
        self.assertEqual(log.new(block(5, 2, 4)), [])

    def test_missed_updates_lose_nothing_still_in_the_tail(self):
        log = LogLines()
        log.new(block(5, 1, 3))
        self.assertEqual(log.new(block(5, 2, 9)), lines(4, 9))

    def test_lines_gone_from_the_tail_are_counted(self):
        log = LogLines()
        log.new(block(5, 1, 3))
        self.assertEqual(log.new(block(5, 7, 8)), ["--- 3 lines not shown ---"] + lines(7, 8))

    def test_a_restarted_ioc_numbers_anew(self):
        log = LogLines()
        log.new(block(5, 1, 30))
        self.assertEqual(log.new(block(6, 1, 2)), ["--- spec-ioc restarted ---"] + lines(1, 2))
        log = LogLines()
        log.new(block(5, 1, 3))
        self.assertEqual(log.new(block(6, 4, 5)),
                         ["--- spec-ioc restarted ---", "--- 3 lines not shown ---"] + lines(4, 5))

    def test_text_keeps_its_spaces_and_an_empty_value_adds_nothing(self):
        log = LogLines()
        self.assertEqual(log.new(""), [])
        self.assertEqual(log.new("5:1 SPEC>  wm th\n5:2 \n"), ["SPEC>  wm th", ""])


if __name__ == "__main__":
    unittest.main()
