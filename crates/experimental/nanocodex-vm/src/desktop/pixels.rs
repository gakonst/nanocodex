use super::{Result, invalid};

// ZPixmap rows are padded to scanline_pad bits; pixel byte order is negotiated
// separately from the client's native endianness. Visual masks are authoritative.
pub(super) fn decode(
    data: &[u8],
    width: u32,
    height: u32,
    bits: u8,
    pad: u8,
    little: bool,
    masks: [u32; 3],
) -> Result<Vec<u8>> {
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || !matches!(bits, 16 | 24 | 32)
        || !matches!(pad, 8 | 16 | 32)
    {
        return Err(invalid(
            "unsupported or excessive X image dimensions/format",
        ));
    }
    if masks
        .iter()
        .any(|&mask| mask == 0 || (bits < 32 && mask >> bits != 0))
        || masks[0] & masks[1] != 0
        || masks[0] & masks[2] != 0
        || masks[1] & masks[2] != 0
    {
        return Err(invalid("invalid X visual masks"));
    }
    for mask in masks {
        let shifted = mask >> mask.trailing_zeros();
        if shifted & shifted.wrapping_add(1) != 0 {
            return Err(invalid("noncontiguous X visual mask"));
        }
    }
    let stride =
        (width as usize * usize::from(bits)).div_ceil(usize::from(pad)) * usize::from(pad) / 8;
    let expected = stride * height as usize;
    if expected > 8_000_000 || data.len() < expected || data.len() > expected + 3 {
        return Err(invalid("invalid or excessive X image buffer"));
    }
    let mut output = Vec::with_capacity(width as usize * height as usize * 3);
    for row in data[..expected].chunks_exact(stride) {
        for pixel in
            row[..width as usize * usize::from(bits) / 8].chunks_exact(usize::from(bits) / 8)
        {
            let mut value = 0u32;
            for (index, &byte) in pixel.iter().enumerate() {
                let shift = if little {
                    index
                } else {
                    pixel.len() - 1 - index
                };
                value |= u32::from(byte) << (shift * 8);
            }
            for mask in masks {
                let shift = mask.trailing_zeros();
                let maximum = u64::from(mask >> shift);
                let channel = u64::from((value & mask) >> shift);
                output.push(((channel * 255 + maximum / 2) / maximum) as u8);
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_both_byte_orders_and_visual_masks() {
        let masks = [0xff0000, 0xff00, 0xff];
        assert_eq!(
            decode(&[3, 2, 1, 0], 1, 1, 32, 32, true, masks).unwrap(),
            [1, 2, 3]
        );
        assert_eq!(
            decode(&[0, 1, 2, 3], 1, 1, 32, 32, false, masks).unwrap(),
            [1, 2, 3]
        );
        assert_eq!(
            decode(&[1, 2, 3, 0], 1, 1, 32, 32, true, [0xff, 0xff00, 0xff0000]).unwrap(),
            [1, 2, 3]
        );
    }
    #[test]
    fn honors_row_padding_and_scales_rgb565() {
        assert_eq!(
            decode(
                &[1, 2, 3, 99, 4, 5, 6, 99],
                1,
                2,
                24,
                32,
                false,
                [0xff0000, 0xff00, 0xff]
            )
            .unwrap(),
            [1, 2, 3, 4, 5, 6]
        );
        assert_eq!(
            decode(
                &[0, 0xf8, 0xe0, 7, 0x1f, 0],
                3,
                1,
                16,
                16,
                true,
                [0xf800, 0x7e0, 0x1f]
            )
            .unwrap(),
            [255, 0, 0, 0, 255, 0, 0, 0, 255]
        );
    }
    #[test]
    fn rejects_short_excessive_and_invalid_buffers() {
        assert!(decode(&[0; 3], 1, 1, 32, 32, true, [0xff0000, 0xff00, 0xff]).is_err());
        assert!(decode(&[], u32::MAX, 1, 32, 32, true, [1, 2, 4]).is_err());
        assert!(decode(&[0; 4], 1, 1, 32, 32, true, [1, 1, 4]).is_err());
        assert!(decode(&[0; 4], 1, 1, 32, 32, true, [5, 2, 8]).is_err());
        assert!(decode(&[0; 4], 1, 1, 8, 32, true, [1, 2, 4]).is_err());
    }
}
