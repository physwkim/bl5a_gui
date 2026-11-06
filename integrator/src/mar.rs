//! A `.mar<size>` file: a 4096-byte header (16 little-endian ints, then
//! ASCII keyword lines), the overflow records, then the 16-bit pck image.
//! A pixel above 65535 is 65535 in the pck stream; its true value is in an
//! (index + 1, value) record, the records padded to a multiple of eight.

const HEADER: usize = 4096;
/// The byte-order mark the header starts with.
const MAGIC: i32 = 1234;

/// A decoded frame, row-major (slow, fast).
#[derive(Debug)]
pub struct Image {
    pub pixels: usize,
    /// µm.
    pub pixel_um: u32,
    pub data: Vec<f32>,
}

pub fn decode(bytes: &[u8]) -> Result<Image, String> {
    if bytes.len() < HEADER {
        return Err(format!("{} bytes, shorter than the header", bytes.len()));
    }
    let int = |at: usize| i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    if int(0) != MAGIC {
        return Err(format!("header starts {}, not {MAGIC}", int(0)));
    }
    let (n, high, um) = (int(4), int(8), int(24));
    if n <= 0 || high < 0 || um <= 0 {
        return Err(format!("header size {n}, overflows {high}, pixel {um} µm"));
    }
    let (n, high) = (n as usize, high as usize);
    if bytes.len() < HEADER + 8 * high.next_multiple_of(8) {
        return Err(format!("{high} overflow records run past the file"));
    }
    // The ad-mar345 driver's decoder: frames read here as mar345-ioc reads them.
    let mut data: Vec<f32> = ad_mar345::pck::get_pck(bytes, n, n)
        .into_iter()
        .map(|v| f32::from(v as u16))
        .collect();
    for k in 0..high {
        let (index, value) = (int(HEADER + 8 * k), int(HEADER + 8 * k + 4));
        let pixel = (index as usize).wrapping_sub(1);
        let slot = data
            .get_mut(pixel)
            .ok_or_else(|| format!("overflow record {k} names pixel {index}"))?;
        *slot = value as f32;
    }
    Ok(Image {
        pixels: n,
        pixel_um: um as u32,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fabio's frame (tests/fixtures/make_mar.py): pixel k is 5k mod 70001,
    /// so it crosses the pck sign bit and needs 893 overflow records.
    #[test]
    fn a_frame_with_overflows_decodes_to_its_counts() {
        let img = decode(include_bytes!("../tests/fixtures/overflow.mar128")).unwrap();
        assert_eq!((img.pixels, img.pixel_um), (128, 150));
        let want: Vec<f32> = (0..128 * 128).map(|k| (5 * k % 70001) as f32).collect();
        assert_eq!(img.data, want);
    }

    #[test]
    fn a_truncated_overflow_table_is_refused() {
        let bytes = include_bytes!("../tests/fixtures/overflow.mar128");
        assert!(
            decode(&bytes[..HEADER + 100])
                .unwrap_err()
                .contains("overflow")
        );
    }
}
