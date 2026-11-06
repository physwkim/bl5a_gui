//! The CCP4 "pck" (version 1) packed image that a mar345 scan writes: a port
//! of `put_pck` in ADmar345's `mar3xx_pck.c`, the inverse of its `get_pck`.
//!
//! Every pixel is stored as its difference from the decoder's predictor —
//! raw for the first pixel, the left neighbour through the end of the first
//! row, the rounded mean of four neighbours after it — in chunks of 2^k
//! pixels that share one bit width. A difference below 65536 in magnitude
//! goes in 16 bits and relies on `get_pck`'s 16-bit wrap; the 32-bit width
//! never occurs for a 16-bit image, and fabio misreads it.

/// The widths a chunk descriptor's 3-bit code selects.
const WIDTHS: [u32; 8] = [0, 4, 5, 6, 7, 8, 16, 32];
/// C `DIFFBUFSIZ`: `put_pck` packs the differences in blocks this long, and
/// no chunk crosses a block.
const DIFF_BLOCK: usize = 16384;

/// The identifier line `get_pck` scans for, then the packed stream.
pub fn encode(img: &[i16], x: usize, y: usize) -> Vec<u8> {
    assert_eq!(img.len(), x * y, "image is not {x}x{y}");
    let mut out = format!("\nCCP4 packed image, X: {x:04}, Y: {y:04}\n").into_bytes();
    let diffs = differences(img, x);
    let mut bits = Bits::new(&mut out);
    for block in diffs.chunks(DIFF_BLOCK) {
        let mut at = 0;
        while at < block.len() {
            let (n, width) = chunk(&block[at..]);
            let code = WIDTHS.iter().position(|&w| w == width).expect("a C width");
            bits.put(n.trailing_zeros(), 3);
            bits.put(code as u32, 3);
            if width > 0 {
                for &d in &block[at..at + n] {
                    bits.put(d as u32, width);
                }
            }
            at += n;
        }
    }
    bits.finish();
    out
}

/// Each pixel minus `get_pck`'s prediction of it.
fn differences(img: &[i16], x: usize) -> Vec<i32> {
    (0..img.len())
        .map(|p| {
            // The decoder's buffer at `p`: zeros from `p` on, which a
            // one-pixel-wide image's `p - x + 1` reads.
            let seen = |i: usize| if i < p { i32::from(img[i]) } else { 0 };
            let prediction = if p > x {
                (seen(p - 1) + seen(p - x + 1) + seen(p - x) + seen(p - x - 1) + 2) / 4
            } else if p > 0 {
                seen(p - 1)
            } else {
                0
            };
            i32::from(img[p]) - prediction
        })
        .collect()
}

/// C `bits`: the bits `chunk` takes at the width its largest magnitude
/// needs.
fn bits(chunk: &[i32]) -> u32 {
    let width = match chunk.iter().map(|d| d.unsigned_abs()).max().unwrap_or(0) {
        0 => 0,
        1..8 => 4,
        8..16 => 5,
        16..32 => 6,
        32..64 => 7,
        64..128 => 8,
        128..65536 => 16,
        _ => 32,
    };
    width * chunk.len() as u32
}

/// `put_pck`'s chunk at the head of `rest`, the remainder of a block: it
/// doubles while the doubled chunk costs less than the two halves with a
/// second descriptor, up to 128 pixels. (pixel count, width).
fn chunk(rest: &[i32]) -> (usize, u32) {
    let last = rest.len() - 1;
    let mut n = 1;
    let mut nbits = bits(&rest[..1]);
    loop {
        if last <= 2 * n {
            return (n, nbits / n as u32);
        }
        let next = bits(&rest[n..2 * n]);
        let doubled = 2 * nbits.max(next);
        if doubled >= nbits + next + 6 {
            return (n, nbits / n as u32);
        }
        nbits = doubled;
        if n == 64 {
            return (128, nbits / 128);
        }
        n *= 2;
    }
}

/// An LSB-first bit stream: the first bit put is bit 0 of the first byte.
struct Bits<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(out: &'a mut Vec<u8>) -> Bits<'a> {
        Bits { out, acc: 0, n: 0 }
    }

    /// The low `width` bits of `v`.
    fn put(&mut self, v: u32, width: u32) {
        let mask = if width == 32 {
            u64::from(u32::MAX)
        } else {
            (1u64 << width) - 1
        };
        self.acc |= (u64::from(v) & mask) << self.n;
        self.n += width;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// C's final flush writes the byte in progress even when it is empty.
    fn finish(self) {
        self.out.push(self.acc as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beam::Rng;

    fn round_trip(img: &[i16], x: usize, y: usize) {
        let bytes = encode(img, x, y);
        assert_eq!(ad_mar345::pck::get_pck(&bytes, x, y), img);
    }

    #[test]
    fn widths_follow_c_bits() {
        let width = |d: i32| bits(&[d]);
        assert_eq!(width(0), 0);
        assert_eq!(width(-7), 4);
        assert_eq!(width(-8), 5);
        assert_eq!(width(127), 8);
        assert_eq!(width(-128), 16);
        assert_eq!(width(32768), 16);
        assert_eq!(width(-65535), 16);
        assert_eq!(width(65536), 32);
    }

    /// The bytes C `put_pck` (ADmar345 `mar3xx_pck.c`) writes for these
    /// images, after the identifier line: 8x8 counts straddling 32768, whose
    /// differences need the 16-bit wrap, and two 16-pixel rows. (On a
    /// one-row image C's `diff_words` reads a pixel past the end.)
    const STRADDLE_IMG: &[i16] = &[
        30000, -27617, -19698, -31779, -23860, -15941, -28022, -20103, -32184, -24265, -16346,
        -28427, -20508, -32589, -24670, -16751, -28832, -20913, 32542, -25075, -17156, -29237,
        -21318, 32137, -25480, -17561, -29642, -21723, 31732, -25885, -17966, -30047, -22128,
        31327, -26290, -18371, -30452, -22533, 30922, -26695, -18776, -30857, -22938, 30517,
        -27100, -19181, -31262, -23343, 30112, -27505, -19586, -31667, -23748, -15829, -27910,
        -19991, -32072, -24153, -16234, -28315, -20396, -32477, -24558, -16639,
    ];
    const STRADDLE_PCK: &[u8] = &[
        53, 76, 221, 187, 199, 187, 199, 51, 244, 187, 199, 187, 199, 51, 244, 187, 199, 51, 116,
        99, 116, 69, 73, 159, 122, 99, 68, 189, 117, 129, 127, 69, 73, 159, 122, 99, 68, 189, 117,
        129, 111, 69, 73, 159, 122, 99, 68, 189, 117, 129, 111, 69, 121, 159, 106, 99, 116, 189,
        117, 129, 111, 69, 121, 159, 42, 221, 24, 93, 111, 93, 224, 91, 81, 222, 167, 218, 24, 93,
        111, 93, 224, 91, 81, 222, 167, 218, 24, 93, 111, 93, 224, 91, 81, 222, 167, 218, 24, 61,
        215, 91, 23, 248, 86, 148, 247, 169, 54, 70, 87, 148, 244, 169, 87, 148, 203, 125, 170,
        141, 209, 21, 37, 125, 234, 113, 99, 68, 189, 53, 92, 224, 15, 87, 148, 0,
    ];
    const ROWS_IMG: &[i16] = &[
        0, 4111, 8222, 12333, 16444, 20555, 24666, 28777, -32648, -28537, -24426, -20315, -16204,
        -12093, -7982, -3871, 240, 4351, 8462, 12573, 16684, 20795, 24906, 29017, -32408, -28297,
        -24186, -20075, -15964, -11853, -7742, -3631,
    ];
    const ROWS_PCK: &[u8] = &[
        0, 253, 0, 241, 0, 241, 0, 241, 0, 241, 0, 241, 0, 241, 0, 241, 0, 241, 0, 241, 0, 241, 0,
        241, 0, 241, 0, 241, 0, 241, 0, 241, 0, 49, 227, 18, 224, 18, 224, 18, 224, 18, 224, 18,
        224, 18, 224, 18, 221, 18, 202, 183, 4, 183, 4, 183, 4, 183, 4, 240, 45, 1, 124, 75, 0,
        223, 18, 0,
    ];

    #[test]
    fn writes_what_c_put_pck_writes() {
        for (img, x, y, want) in [
            (STRADDLE_IMG, 8, 8, STRADDLE_PCK),
            (ROWS_IMG, 16, 2, ROWS_PCK),
        ] {
            let bytes = encode(img, x, y);
            let head = format!("\nCCP4 packed image, X: {x:04}, Y: {y:04}\n").len();
            assert_eq!(&bytes[head..], want, "{x}x{y}");
            round_trip(img, x, y);
        }
    }

    #[test]
    fn decodes_through_get_pck() {
        // The images of the driver's C fixtures: varied 8x8, constant 4x4,
        // one 16-pixel row.
        let a: Vec<i16> = vec![
            -48, -41, -34, -27, -20, -13, -6, 1, 8, 15, 22, 29, 36, 43, -47, -40, -33, -26, -19,
            -12, -5, 2, 9, 16, 23, 30, 37, 44, -46, -39, 5000, -5000, -18, -11, -4, 3, 10, 17, 24,
            31, 38, 45, -45, -38, -31, -12345, 12345, -10, -3, 4, 11, 18, 25, 32, 39, 46, -44, -37,
            -30, -23, -16, -9, -2, 5,
        ];
        round_trip(&a, 8, 8);
        round_trip(&[1000; 16], 4, 4);
        let c: Vec<i16> = (0..16).map(|i| -30 + i * i).collect();
        round_trip(&c, 16, 1);
    }

    #[test]
    fn extremes_and_noise_survive() {
        let mut rng = Rng::new(3);
        // Full-scale jumps need the 32-bit width; u16 counts above 32767
        // arrive bit-cast to negative i16.
        let jumps: Vec<i16> = (0..64)
            .map(|i| if i % 3 == 0 { i16::MIN } else { i16::MAX })
            .collect();
        round_trip(&jumps, 8, 8);
        for (x, y) in [(1, 1), (1, 7), (7, 1), (13, 11), (130, 3)] {
            let img: Vec<i16> = (0..x * y)
                .map(|_| rng.between(0.0, 65535.0) as u16 as i16)
                .collect();
            round_trip(&img, x, y);
        }
        // A smooth frame, mostly short chunks of narrow widths.
        let smooth: Vec<i16> = (0..200 * 150)
            .map(|i| (100.0 + rng.poisson(40.0) + (i % 200) as f64 * 0.5) as i16)
            .collect();
        round_trip(&smooth, 200, 150);
        let packed = encode(&smooth, 200, 150).len();
        assert!(
            packed < smooth.len(),
            "{packed} bytes for {} pixels",
            smooth.len()
        );
    }
}
