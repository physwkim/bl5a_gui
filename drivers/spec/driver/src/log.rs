//! SPEC's log as the Log record carries it.

use std::collections::VecDeque;

/// The Log record's NELM. A read keeps at most `NELM - 1` bytes, cutting
/// the end (devAsynOctet `interruptCallback`), so the tail stays within it.
pub const LOG_NELM: usize = 8192;

/// The newest log lines that fit in the Log record, each
/// `"<run>:<seq> <text>\n"`: `run` is the driver's start in milliseconds
/// since the epoch and `seq` counts lines from 1 within it, so a line's
/// number names it across IOC restarts. Every post is the whole tail: a
/// CA monitor keeps only the latest array, and a reader that missed posts
/// still finds every line it has not seen that is still in the tail.
pub(crate) struct Tail {
    run: u64,
    seq: u64,
    lines: VecDeque<String>,
    /// The tail's length, the sum of `lines`' lengths.
    bytes: usize,
    cap: usize,
}

impl Tail {
    pub(crate) fn new(run: u64) -> Tail {
        Tail::with_cap(run, LOG_NELM - 1)
    }

    fn with_cap(run: u64, cap: usize) -> Tail {
        Tail {
            run,
            seq: 0,
            lines: VecDeque::new(),
            bytes: 0,
            cap,
        }
    }

    /// Adds each line of `text`, one number per line, and returns the
    /// tail. A line longer than the whole tail keeps its start.
    pub(crate) fn push(&mut self, text: &str) -> String {
        for line in text.lines() {
            self.seq += 1;
            let mut l = format!("{}:{} {line}", self.run, self.seq);
            l.truncate(l.floor_char_boundary(self.cap - 1));
            l.push('\n');
            self.bytes += l.len();
            self.lines.push_back(l);
            while self.bytes > self.cap {
                let oldest = self.lines.pop_front().expect("over the cap with no line");
                self.bytes -= oldest.len();
            }
        }
        self.lines.iter().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbers(tail: &str) -> Vec<&str> {
        tail.lines().map(|l| l.split_once(' ').unwrap().0).collect()
    }

    #[test]
    fn each_line_has_its_own_number() {
        let mut t = Tail::new(7);
        t.push("connected");
        assert_eq!(t.push("a\nb"), "7:1 connected\n7:2 a\n7:3 b\n");
    }

    #[test]
    fn the_oldest_lines_go_to_stay_within_the_cap() {
        // Each line is "1:N xxxx\n", 9 bytes.
        let mut t = Tail::with_cap(1, 20);
        assert_eq!(numbers(&t.push("xxxx\nxxxx")), ["1:1", "1:2"]);
        let tail = t.push("xxxx");
        assert_eq!(numbers(&tail), ["1:2", "1:3"]);
        assert_eq!(tail.len(), 18);
        // At the cap exactly, nothing goes.
        let mut t = Tail::with_cap(1, 18);
        assert_eq!(numbers(&t.push("xxxx\nxxxx")), ["1:1", "1:2"]);
    }

    #[test]
    fn a_line_longer_than_the_cap_keeps_its_start() {
        let mut t = Tail::with_cap(1, 10);
        t.push("ab");
        // "1:2 é…" cut on a char boundary within 9 bytes, then the newline.
        assert_eq!(t.push("ééééé"), "1:2 éé\n");
    }

    #[test]
    fn the_default_cap_fits_the_record() {
        let mut t = Tail::new(u64::MAX);
        let tail = t.push(&"x".repeat(3 * LOG_NELM));
        assert_eq!(tail.len(), LOG_NELM - 1);
        for _ in 0..1000 {
            assert!(t.push("a SPEC line").len() < LOG_NELM);
        }
    }
}
