//! Clipboard image conversions: PNG <-> RGBA and Windows device-independent
//! bitmaps (the contents of CF_DIB / CF_DIBV5) <-> RGBA. Pure Rust, so they
//! are tested on every OS. Malformed input is an error, never a panic.

use std::io::Cursor;

/// Most pixels accepted in one image (8K is 33 million).
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("image: {0}")]
pub struct ImageError(pub String);

fn err(msg: impl Into<String>) -> ImageError {
    ImageError(msg.into())
}

/// 8-bit RGBA pixels, rows top to bottom.
#[derive(Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgba({}x{})", self.width, self.height)
    }
}

fn check_size(width: u64, height: u64) -> Result<usize, ImageError> {
    if width == 0 || height == 0 {
        return Err(err("empty image"));
    }
    let pixels = width
        .checked_mul(height)
        .filter(|p| *p <= MAX_PIXELS)
        .ok_or_else(|| err(format!("{width}x{height} is too large")))?;
    Ok(pixels as usize * 4)
}

/// The size a PNG declares in its header, checked against [`MAX_PIXELS`]
/// without decoding it.
pub fn png_size(png: &[u8]) -> Result<(u32, u32), ImageError> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if png.get(..8) != Some(SIGNATURE) || png.get(12..16) != Some(b"IHDR") {
        return Err(err("not a PNG"));
    }
    let width = u32_be(png, 16)?;
    let height = u32_be(png, 20)?;
    check_size(u64::from(width), u64::from(height))?;
    Ok((width, height))
}

fn u32_be(b: &[u8], at: usize) -> Result<u32, ImageError> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| err("truncated PNG header"))
}

/// Decodes any PNG (first frame of an animation) to RGBA.
pub fn decode_png(png: &[u8]) -> Result<Rgba, ImageError> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(png),
        png::Limits {
            bytes: 512 * 1024 * 1024,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| err(e.to_string()))?;
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    check_size(u64::from(w), u64::from(h))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| err("image too large"))?;
    let mut buf = vec![0u8; size];
    let out = reader
        .next_frame(&mut buf)
        .map_err(|e| err(e.to_string()))?;
    let data = &buf[..out.buffer_size()];
    let px = out.width as usize * out.height as usize;
    let pixels = match out.color_type {
        png::ColorType::Rgba => data[..px * 4].to_vec(),
        png::ColorType::Rgb => data[..px * 3]
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => data[..px * 2]
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Grayscale => data[..px].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err(err("palette not expanded")),
    };
    Ok(Rgba {
        width: out.width,
        height: out.height,
        pixels,
    })
}

/// Encodes RGBA as PNG (fast compression: clipboard images are large and
/// travel on a LAN).
pub fn encode_png(img: &Rgba) -> Result<Vec<u8>, ImageError> {
    let expected = check_size(u64::from(img.width), u64::from(img.height))?;
    if img.pixels.len() != expected {
        return Err(err("pixel buffer does not match the size"));
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, img.width, img.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|e| err(e.to_string()))?;
    writer
        .write_image_data(&img.pixels)
        .map_err(|e| err(e.to_string()))?;
    writer.finish().map_err(|e| err(e.to_string()))?;
    Ok(out)
}

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_PNG: u32 = 5;
const BI_ALPHABITFIELDS: u32 = 6;

fn u16_at(b: &[u8], at: usize) -> Result<u16, ImageError> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| err("truncated bitmap header"))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, ImageError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| err("truncated bitmap header"))
}

/// Extracts a channel described by a bit mask and scales it to 8 bits.
fn channel(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    // The width of the mask's span: a malformed mask with holes cannot
    // produce a value above `max`.
    let bits = 32 - (mask >> shift).leading_zeros();
    let v = u64::from((value & mask) >> shift);
    if bits >= 8 {
        (v >> (bits - 8)) as u8
    } else {
        // Replicate the high bits into the low ones (31 -> 255 for 5 bits).
        let max = (1u64 << bits) - 1;
        ((v * 255 + max / 2) / max) as u8
    }
}

/// Decodes a packed DIB (BITMAPINFOHEADER / V4 / V5, then masks and palette
/// as needed, then the bits), as found in CF_DIB and CF_DIBV5. Supports
/// 1/4/8-bit palettes, 16/24/32-bit RGB, bit fields and embedded PNG.
pub fn decode_dib(dib: &[u8]) -> Result<Rgba, ImageError> {
    let header_size = u32_at(dib, 0)? as usize;
    if header_size < 40 || header_size > dib.len() {
        return Err(err("unsupported bitmap header"));
    }
    let width = u32_at(dib, 4)? as i32;
    let height = u32_at(dib, 8)? as i32;
    let bit_count = u16_at(dib, 14)?;
    let compression = u32_at(dib, 16)?;
    let colors_used = u32_at(dib, 32)?;
    if compression == BI_PNG {
        let size = u32_at(dib, 20)? as usize;
        let png = dib
            .get(header_size..header_size.saturating_add(size))
            .ok_or_else(|| err("truncated embedded PNG"))?;
        return decode_png(png);
    }
    if width <= 0 || height == 0 || height == i32::MIN {
        return Err(err("invalid bitmap size"));
    }
    let top_down = height < 0;
    let (w, h) = (width as u64, u64::from(height.unsigned_abs()));
    let out_len = check_size(w, h)?;
    let mut offset = header_size;
    // Channel masks: inside a V4/V5 header, or after a plain one.
    let masks = match (compression, bit_count) {
        (BI_BITFIELDS | BI_ALPHABITFIELDS, 16 | 32) => {
            let (base, count) = if header_size >= 56 {
                (40, 4)
            } else {
                let count = if compression == BI_ALPHABITFIELDS {
                    4
                } else {
                    3
                };
                let base = offset;
                offset += count * 4;
                (base, count)
            };
            let alpha = if count == 4 {
                u32_at(dib, base + 12)?
            } else {
                0
            };
            Some([
                u32_at(dib, base)?,
                u32_at(dib, base + 4)?,
                u32_at(dib, base + 8)?,
                alpha,
            ])
        }
        (BI_RGB, 16) => Some([0x7C00, 0x03E0, 0x001F, 0]),
        (BI_RGB, 32) => None,
        (BI_RGB, 1 | 4 | 8 | 24) => None,
        _ => {
            return Err(err(format!(
                "unsupported bitmap {bit_count} bpp, compression {compression}"
            )));
        }
    };
    let palette: Vec<[u8; 4]> = if bit_count <= 8 {
        let n = if colors_used == 0 {
            1usize << bit_count
        } else {
            (colors_used as usize).min(1 << bit_count)
        };
        let table = dib
            .get(offset..offset + n * 4)
            .ok_or_else(|| err("truncated palette"))?;
        offset += n * 4;
        table
            .chunks_exact(4)
            .map(|c| [c[2], c[1], c[0], 255])
            .collect()
    } else {
        // An optional color table (for palette-based displays) may sit
        // before the bits of high-color bitmaps too.
        offset = offset.saturating_add((colors_used as usize).saturating_mul(4));
        Vec::new()
    };
    let stride = (w as usize * bit_count as usize).div_ceil(32) * 4;
    let bits = dib
        .get(offset..offset.saturating_add(stride * h as usize))
        .ok_or_else(|| err("truncated bitmap data"))?;
    let mut pixels = vec![0u8; out_len];
    let mut any_alpha = false;
    for y in 0..h as usize {
        let src_row = if top_down { y } else { h as usize - 1 - y };
        let row = &bits[src_row * stride..(src_row + 1) * stride];
        for x in 0..w as usize {
            let px: [u8; 4] = match bit_count {
                1 | 4 | 8 => {
                    let bpp = bit_count as usize;
                    let bit = x * bpp;
                    let byte = row[bit / 8];
                    let idx = (byte >> (8 - bpp - bit % 8)) & ((1u16 << bpp) - 1) as u8;
                    *palette.get(idx as usize).unwrap_or(&[0, 0, 0, 255])
                }
                16 => {
                    let v = u32::from(u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]));
                    let m = masks.unwrap_or([0x7C00, 0x03E0, 0x001F, 0]);
                    let a = if m[3] == 0 { 255 } else { channel(v, m[3]) };
                    [channel(v, m[0]), channel(v, m[1]), channel(v, m[2]), a]
                }
                24 => [row[x * 3 + 2], row[x * 3 + 1], row[x * 3], 255],
                _ => {
                    let v = u32::from_le_bytes([
                        row[x * 4],
                        row[x * 4 + 1],
                        row[x * 4 + 2],
                        row[x * 4 + 3],
                    ]);
                    let m = masks.unwrap_or([0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000]);
                    let a = channel(v, m[3]);
                    any_alpha |= a != 0;
                    [channel(v, m[0]), channel(v, m[1]), channel(v, m[2]), a]
                }
            };
            pixels[(y * w as usize + x) * 4..][..4].copy_from_slice(&px);
        }
    }
    // 32-bit BI_RGB bitmaps (or bitfields without an explicit alpha mask)
    // very often leave the alpha byte at 0: that means opaque.
    if bit_count == 32 && !any_alpha && masks.is_none_or(|m| m[3] == 0) {
        for a in pixels.iter_mut().skip(3).step_by(4) {
            *a = 255;
        }
    }
    Ok(Rgba {
        width: w as u32,
        height: h as u32,
        pixels,
    })
}

/// Encodes RGBA as a packed bottom-up 32-bit BI_RGB DIB (BITMAPINFOHEADER),
/// the most widely understood CF_DIB form. Windows derives CF_DIBV5 and
/// CF_BITMAP from it.
pub fn encode_dib(img: &Rgba) -> Result<Vec<u8>, ImageError> {
    let expected = check_size(u64::from(img.width), u64::from(img.height))?;
    if img.pixels.len() != expected {
        return Err(err("pixel buffer does not match the size"));
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = Vec::with_capacity(40 + expected);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(img.width as i32).to_le_bytes());
    out.extend_from_slice(&(img.height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&BI_RGB.to_le_bytes());
    out.extend_from_slice(&(expected as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // resolution, colors used / important
    for y in (0..h).rev() {
        for px in img.pixels[y * w * 4..(y + 1) * w * 4].chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(w: u32, h: u32) -> Rgba {
        let pixels = (0..w * h)
            .flat_map(|i| {
                [
                    (i * 7) as u8,
                    (i * 13) as u8,
                    (i * 29) as u8,
                    (255 - i) as u8,
                ]
            })
            .collect();
        Rgba {
            width: w,
            height: h,
            pixels,
        }
    }

    /// A packed DIB with a 40-byte header.
    fn dib(
        width: i32,
        height: i32,
        bpp: u16,
        compression: u32,
        extra: &[u8],
        bits: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&bpp.to_le_bytes());
        out.extend_from_slice(&compression.to_le_bytes());
        out.extend_from_slice(&(bits.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(extra);
        out.extend_from_slice(bits);
        out
    }

    #[test]
    fn png_round_trip() {
        let img = sample(17, 5);
        let png = encode_png(&img).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(decode_png(&png).unwrap(), img);
    }

    #[test]
    fn png_gray_and_rgb_become_rgba() {
        let mut gray = Vec::new();
        {
            let mut e = png::Encoder::new(&mut gray, 2, 1);
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Eight);
            e.write_header()
                .unwrap()
                .write_image_data(&[10, 200])
                .unwrap();
        }
        assert_eq!(
            decode_png(&gray).unwrap().pixels,
            vec![10, 10, 10, 255, 200, 200, 200, 255]
        );
        let mut rgb = Vec::new();
        {
            let mut e = png::Encoder::new(&mut rgb, 1, 1);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Sixteen);
            e.write_header()
                .unwrap()
                .write_image_data(&[0xFF, 0, 0x80, 0, 0, 0])
                .unwrap();
        }
        assert_eq!(decode_png(&rgb).unwrap().pixels, vec![0xFF, 0x80, 0, 255]);
    }

    #[test]
    fn dib_round_trip_is_bottom_up_bgra() {
        let img = sample(3, 2);
        let d = encode_dib(&img).unwrap();
        assert_eq!(d.len(), 40 + 3 * 2 * 4);
        // The first stored row is the image's last row, in BGRA.
        let last_row_first_px = &img.pixels[3 * 4..3 * 4 + 4];
        assert_eq!(
            &d[40..44],
            &[
                last_row_first_px[2],
                last_row_first_px[1],
                last_row_first_px[0],
                last_row_first_px[3]
            ]
        );
        assert_eq!(decode_dib(&d).unwrap(), img);
    }

    #[test]
    fn dib_24_bit_with_row_padding() {
        // 1x2, bottom-up: stored row 0 is the bottom pixel (blue), row 1 the top (red).
        let bits = [255, 0, 0, 0, 0, 0, 255, 0];
        let d = dib(1, 2, 24, BI_RGB, &[], &bits);
        assert_eq!(
            decode_dib(&d).unwrap().pixels,
            vec![255, 0, 0, 255, 0, 0, 255, 255]
        );
        // Top-down: same bytes, rows in reading order.
        let d = dib(1, -2, 24, BI_RGB, &[], &bits);
        assert_eq!(
            decode_dib(&d).unwrap().pixels,
            vec![0, 0, 255, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn dib_32_bit_zero_alpha_means_opaque_and_bitfields_work() {
        let d = dib(1, 1, 32, BI_RGB, &[], &[1, 2, 3, 0]);
        assert_eq!(decode_dib(&d).unwrap().pixels, vec![3, 2, 1, 255]);
        let d = dib(1, 1, 32, BI_RGB, &[], &[1, 2, 3, 128]);
        assert_eq!(decode_dib(&d).unwrap().pixels, vec![3, 2, 1, 128]);
        // RGBA masks in a different order than BGRA.
        let mut masks = Vec::new();
        for m in [0x0000_00FFu32, 0x0000_FF00, 0x00FF_0000] {
            masks.extend_from_slice(&m.to_le_bytes());
        }
        let d = dib(1, 1, 32, BI_BITFIELDS, &masks, &[10, 20, 30, 0]);
        assert_eq!(decode_dib(&d).unwrap().pixels, vec![10, 20, 30, 255]);
    }

    #[test]
    fn dib_16_bit_and_palettes() {
        // 5-5-5: pure red = 0x7C00.
        let d = dib(1, 1, 16, BI_RGB, &[], &[0x00, 0x7C, 0, 0]);
        assert_eq!(decode_dib(&d).unwrap().pixels, vec![255, 0, 0, 255]);
        // 1 bpp, 2-entry palette (black, white), 8 pixels 10101010.
        let palette = [0, 0, 0, 0, 255, 255, 255, 0];
        let d = dib(8, 1, 1, BI_RGB, &palette, &[0b1010_1010, 0, 0, 0]);
        let px = decode_dib(&d).unwrap().pixels;
        assert_eq!(&px[..8], &[255, 255, 255, 255, 0, 0, 0, 255]);
        // 8 bpp with colors_used = 0 needs a full 256-entry palette.
        let short = dib(1, 1, 8, BI_RGB, &[0; 16], &[0, 0, 0, 0]);
        assert!(decode_dib(&short).is_err());
    }

    #[test]
    fn dib_masks_with_holes_and_high_color_tables() {
        // A non-contiguous mask must not overflow (debug builds check).
        let mut masks = Vec::new();
        for m in [0x8000_0001u32, 0x0000_FF00, 0x00FF_0000] {
            masks.extend_from_slice(&m.to_le_bytes());
        }
        let d = dib(1, 1, 32, BI_BITFIELDS, &masks, &[1, 0, 0, 0x80]);
        // Scaled over the mask's 32-bit span: the top byte is 0x80.
        assert_eq!(decode_dib(&d).unwrap().pixels[0], 0x80);
        // 24 bpp with two color table entries before the bits.
        let mut d = dib(1, 1, 24, BI_RGB, &[0xAA; 8], &[1, 2, 3, 0]);
        d[32..36].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(decode_dib(&d).unwrap().pixels, vec![3, 2, 1, 255]);
    }

    #[test]
    fn png_size_reads_the_header_only() {
        let png = encode_png(&sample(17, 5)).unwrap();
        assert_eq!(png_size(&png), Ok((17, 5)));
        let mut huge = png[..24].to_vec();
        huge[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        huge[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(png_size(&huge).is_err(), "over MAX_PIXELS");
        assert!(png_size(&png[..20]).is_err());
        assert!(png_size(b"GIF89a").is_err());
    }

    #[test]
    fn dib_with_embedded_png() {
        let img = sample(2, 2);
        let png = encode_png(&img).unwrap();
        let d = dib(2, 2, 0, BI_PNG, &[], &png);
        assert_eq!(decode_dib(&d).unwrap(), img);
    }

    #[test]
    fn bitfields_with_alpha_mask_preserve_full_transparency() {
        let mut masks = Vec::new();
        for m in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000] {
            masks.extend_from_slice(&m.to_le_bytes());
        }
        let d = dib(
            1,
            1,
            32,
            BI_ALPHABITFIELDS,
            &masks,
            &0x0011_2233u32.to_le_bytes(),
        );
        assert_eq!(decode_dib(&d).unwrap().pixels, [0x11, 0x22, 0x33, 0x00]);
    }

    #[test]
    fn malformed_and_oversized_inputs_are_errors() {
        assert!(decode_dib(&[]).is_err());
        assert!(decode_dib(&dib(0, 1, 24, BI_RGB, &[], &[])).is_err());
        assert!(
            decode_dib(&dib(1, 1, 24, BI_RGB, &[], &[1, 2])).is_err(),
            "truncated"
        );
        assert!(decode_dib(&dib(1, 1, 24, 1, &[], &[0; 4])).is_err(), "RLE");
        assert!(
            decode_dib(&dib(1 << 20, 1 << 20, 24, BI_RGB, &[], &[])).is_err(),
            "huge"
        );
        assert!(decode_png(b"\x89PNG\r\n\x1a\nnot really").is_err());
        assert!(
            encode_png(&Rgba {
                width: 2,
                height: 2,
                pixels: vec![0; 3]
            })
            .is_err()
        );
    }

    proptest::proptest! {
        #[test]
        fn prop_random_bytes_never_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..600)) {
            let _ = decode_dib(&bytes);
            let _ = decode_png(&bytes);
        }
    }
}
