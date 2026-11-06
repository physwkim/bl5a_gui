//! A `.mar<size>` file as the mar345 scanner writes it and fabio reads it:
//! a 4096-byte header (16 little-endian ints, then ASCII keyword lines),
//! the overflow records, then the pck image.
//!
//! The pck stream is 16-bit: a pixel above 65535 is stored there as 65535
//! and its true value as an (index + 1, value) int32 pair before it, the
//! pairs padded with zeros to a multiple of eight.

use super::image::Mode;
use super::pck;

const HEADER: usize = 4096;
const BINARY: usize = 64;

/// What the header records besides the image.
pub struct Meta {
    pub mode: Mode,
    /// Å.
    pub wavelength: f64,
    /// mm.
    pub distance: f64,
    /// Point of normal incidence, pixels (fast, slow).
    pub center: (f64, f64),
    /// Seconds the shutter was open.
    pub time: f64,
    /// `ctime` style.
    pub date: String,
}

pub fn encode(data: &[u32], meta: &Meta) -> Vec<u8> {
    let n = meta.mode.pixels;
    assert_eq!(data.len(), n * n, "image is not {n}x{n}");
    let high: Vec<(i32, i32)> = data
        .iter()
        .enumerate()
        .filter(|&(_, &v)| v > 0xffff)
        .map(|(i, &v)| (i as i32 + 1, v.min(i32::MAX as u32) as i32))
        .collect();

    let mut out = Vec::with_capacity(HEADER + 8 * high.len() + data.len());
    let um = meta.mode.pixel_um as i32;
    let binary = [
        1234,
        n as i32,
        high.len() as i32,
        1,
        1,
        (n * n) as i32,
        um,
        um,
        (meta.wavelength * 1e6).round() as i32,
        (meta.distance * 1e3).round() as i32,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    for v in binary {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(ascii(meta, n, high.len()).as_bytes());
    debug_assert_eq!(out.len(), HEADER);

    for &(i, v) in &high {
        out.extend_from_slice(&i.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
    }
    let padding = high.len().next_multiple_of(8) - high.len();
    out.resize(out.len() + 8 * padding, 0);

    let clipped: Vec<i16> = data.iter().map(|&v| v.min(0xffff) as u16 as i16).collect();
    out.extend_from_slice(&pck::encode(&clipped, n, n));
    out
}

/// fabio's keyword lines: 15-column key, 48-column value, `\n` after each
/// but the last, blank-padded to the end of the header.
fn ascii(meta: &Meta, n: usize, high: usize) -> String {
    let (cx, cy) = meta.center;
    let um = meta.mode.pixel_um;
    let fields = [
        ("PROGRAM", "bl5a-sim".to_string()),
        ("DATE", meta.date.clone()),
        ("FORMAT", format!("{n}  PCK {}", n * n)),
        ("HIGH", high.to_string()),
        ("PIXEL", format!("LENGTH {um}  HEIGHT {um}")),
        ("WAVELENGTH", format!("{:.5}", meta.wavelength)),
        ("DISTANCE", format!("{:.3}", meta.distance)),
        ("CENTER", format!("X {cx:.2}  Y {cy:.2}")),
        ("MODE", "TIME".to_string()),
        ("TIME", format!("{:.2}", meta.time)),
        ("REMARK", "simulated by bl5a-sim".to_string()),
    ];
    let mut lines = vec![format!("{:<63}", "mar research")];
    lines.extend(fields.iter().map(|(k, v)| format!("{k:<15}{v:<48}")));
    lines.push("END OF HEADER".into());
    format!("{:<width$}", lines.join("\n"), width = HEADER - BINARY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beam::Rng;

    fn meta(mode: Mode) -> Meta {
        Meta {
            mode,
            wavelength: 0.7749,
            distance: 300.0,
            center: (1719.0, 1731.0),
            time: 10.0,
            date: "Wed Nov  4 10:00:00 2026".into(),
        }
    }

    fn int(bytes: &[u8], i: usize) -> i32 {
        i32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().unwrap())
    }

    /// The driver reads the whole file with `get_pck`, which finds the pck
    /// stream past the header and overflow records; it sees the 16-bit
    /// pixels, so overflowed ones as 65535.
    #[test]
    fn the_driver_decodes_a_whole_file() {
        let mode = Mode {
            pixels: 64,
            pixel_um: 100,
        };
        let mut rng = Rng::new(9);
        let mut data: Vec<u32> = (0..64 * 64).map(|_| rng.poisson(50.0) as u32).collect();
        // Enough overflows that the records hold bytes 0x0a and the count is
        // not a multiple of eight.
        for (k, i) in (0..64 * 64).step_by(97).enumerate() {
            data[i] = 65_536 + 10 * k as u32;
        }
        data[5] = 40_000;
        let high = data.iter().filter(|&&v| v > 0xffff).count();
        assert_ne!(high % 8, 0);
        let bytes = encode(&data, &meta(mode));
        assert_eq!(int(&bytes, 0), 1234);
        assert_eq!(int(&bytes, 2) as usize, high);
        assert_eq!(&bytes[BINARY..BINARY + 12], b"mar research");
        let records = &bytes[HEADER..HEADER + 8 * high.next_multiple_of(8)];
        assert_eq!(int(records, 0), 1);
        assert_eq!(int(records, 1), 65_536);
        assert_eq!(int(records, 2 * high), 0);
        let want: Vec<i16> = data.iter().map(|&v| v.min(0xffff) as u16 as i16).collect();
        assert_eq!(ad_mar345::pck::get_pck(&bytes, 64, 64), want);
    }

    #[test]
    fn header_lines_sit_on_64_byte_columns() {
        let text = ascii(&meta(Mode::from_pixels(3450).unwrap()), 3450, 3);
        assert_eq!(text.len(), HEADER - BINARY);
        let lines: Vec<&str> = text.trim_end().split('\n').collect();
        assert!(lines[..lines.len() - 1].iter().all(|l| l.len() == 63));
        assert_eq!(
            lines[3],
            format!("{:<63}", "FORMAT         3450  PCK 11902500")
        );
        assert_eq!(*lines.last().unwrap(), "END OF HEADER");
    }
}
