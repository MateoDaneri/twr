//! Owned terminal image frames. BGRA matches the pinned GPUI upload format.
use std::sync::Arc;

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PLACEMENTS: usize = 64;

pub fn decode_png(bytes: &[u8]) -> Option<image::RgbaImage> {
    use image::ImageDecoder;
    if bytes.len() > 4 * 1024 * 1024 {
        return None;
    }
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || width as u64 * height as u64 > (MAX_FRAME_BYTES / 4) as u64 {
        return None;
    }
    Some(image::DynamicImage::from_decoder(decoder).ok()?.to_rgba8())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub generation: u64,
    pub crop: [u32; 4],
}
#[derive(Debug)]
pub struct Pixels {
    pub key: ImageKey,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}
impl Pixels {
    /// The pinned Metal atlas has no tile padding and samples linearly. Repeat
    /// edge texels so magnifying a 1px image cannot sample neighboring tiles.
    pub fn padded_bgra(&self) -> image::RgbaImage {
        let width = self.width + 2;
        let height = self.height + 2;
        let mut bytes = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            let source_y = y.saturating_sub(1).min(self.height - 1);
            for x in 0..width {
                let source_x = x.saturating_sub(1).min(self.width - 1);
                let start = ((source_y * self.width + source_x) * 4) as usize;
                bytes.extend_from_slice(&self.bgra[start..start + 4]);
            }
        }
        image::RgbaImage::from_raw(width, height, bytes).expect("validated padded pixel length")
    }
}
#[derive(Clone, Debug)]
pub struct Placement {
    pub pixels: Arc<Pixels>,
    pub col: f32,
    pub row: f32,
    pub width: f32,
    pub height: f32,
    pub layer: u32,
    pub z: i32,
}

/// Validate before reading a foreign pixel slice or allocating a crop.
/// The pinned Unicode renderer rounds floating source rectangles to integers.
/// A boundary can round one pixel past the image. Match edge-clamped sampling
/// before passing the explicit crop to the strict pixel-layout validator.
pub fn render_crop(width: u32, height: u32, crop: [u32; 4]) -> Option<[u32; 4]> {
    fn axis(limit: u32, start: u32, extent: u32) -> Option<(u32, u32)> {
        if limit == 0 || limit > 4096 || start > limit || start.checked_add(extent)? > limit + 1 {
            return None;
        }
        let start = start.min(limit - 1);
        Some((start, extent.max(1).min(limit - start)))
    }
    let (x, width) = axis(width, crop[0], crop[2])?;
    let (y, height) = axis(height, crop[1], crop[3])?;
    Some([x, y, width, height])
}
pub fn layout(
    width: u32,
    height: u32,
    format: u32,
    len: usize,
    crop: [u32; 4],
) -> Option<(usize, [u32; 4])> {
    let bpp = match format {
        0 => 3,
        1 => 4,
        3 => 2,
        4 => 1,
        _ => return None,
    };
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return None;
    }
    let pixels = (width as usize).checked_mul(height as usize)?;
    if pixels > MAX_FRAME_BYTES / 4 || pixels.checked_mul(bpp)? != len {
        return None;
    }
    let [x, y, w, h] = crop;
    let w = if w == 0 { width.checked_sub(x)? } else { w };
    let h = if h == 0 { height.checked_sub(y)? } else { h };
    if w == 0 || h == 0 || x.checked_add(w)? > width || y.checked_add(h)? > height {
        return None;
    }
    Some((bpp, [x, y, w, h]))
}

pub fn crop_bgra(
    key: ImageKey,
    width: u32,
    height: u32,
    format: u32,
    data: &[u8],
) -> Option<Pixels> {
    let (bpp, [x, y, w, h]) = layout(width, height, format, data.len(), key.crop)?;
    let mut bgra = Vec::with_capacity(w as usize * h as usize * 4);
    for row in y..y + h {
        let start = ((row * width + x) as usize) * bpp;
        for pixel in data.get(start..start + w as usize * bpp)?.chunks_exact(bpp) {
            let rgba = match format {
                0 => [pixel[0], pixel[1], pixel[2], 255],
                1 => [pixel[0], pixel[1], pixel[2], pixel[3]],
                3 => [pixel[0], pixel[0], pixel[0], pixel[1]],
                4 => [pixel[0], pixel[0], pixel[0], 255],
                _ => return None,
            };
            bgra.extend_from_slice(&[rgba[2], rgba[1], rgba[0], rgba[3]]);
        }
    }
    Some(Pixels {
        key,
        width: w,
        height: h,
        bgra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rounded_virtual_edges_sample_the_last_pixel_without_out_of_bounds_reads() {
        assert_eq!(render_crop(16, 8, [0, 8, 16, 1]), Some([0, 7, 16, 1]));
        assert_eq!(render_crop(16, 8, [16, 0, 1, 8]), Some([15, 0, 1, 8]));
        assert_eq!(render_crop(16, 8, [15, 7, 2, 2]), Some([15, 7, 1, 1]));
        assert_eq!(render_crop(16, 8, [16, 8, 0, 0]), Some([15, 7, 1, 1]));
        for crop in [
            [0, 9, 16, 1],
            [17, 0, 1, 8],
            [15, 0, 3, 8],
            [0, 0, u32::MAX, 8],
        ] {
            assert!(render_crop(16, 8, crop).is_none());
        }
        assert!(render_crop(0, 8, [0; 4]).is_none());
    }
    #[test]
    fn padded_single_pixel_and_cropped_edges_keep_color_and_alpha() {
        let key = ImageKey {
            generation: 1,
            crop: [0; 4],
        };
        let one = crop_bgra(key, 1, 1, 1, &[255, 0, 0, 127])
            .unwrap()
            .padded_bgra();
        assert_eq!(one.dimensions(), (3, 3));
        assert!(
            one.as_raw()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 255, 127])
        );
        let two = crop_bgra(key, 1, 2, 1, &[0, 255, 0, 255, 255, 255, 0, 255])
            .unwrap()
            .padded_bgra();
        assert_eq!(two.dimensions(), (3, 4));
        assert_eq!(&two.as_raw()[0..24], &[0, 255, 0, 255].repeat(6));
        assert_eq!(&two.as_raw()[24..], &[0, 255, 255, 255].repeat(6));
    }
    #[test]
    fn png_decoder_checks_real_pixels_and_rejects_large_or_invalid_inputs() {
        use image::ImageEncoder;
        let png = |width, height, bytes: &[u8]| {
            let mut encoded = Vec::new();
            image::codecs::png::PngEncoder::new(&mut encoded)
                .write_image(bytes, width, height, image::ExtendedColorType::Rgba8)
                .unwrap();
            encoded
        };
        let bytes = [0, 255, 0, 255, 0, 0, 255, 127];
        assert_eq!(decode_png(&png(2, 1, &bytes)).unwrap().as_raw(), &bytes);
        assert!(decode_png(b"not a PNG").is_none());
        assert!(decode_png(&vec![0; 4 * 1024 * 1024 + 1]).is_none());
        assert!(decode_png(&png(4097, 1, &vec![0; 4097 * 4])).is_none());
        assert!(decode_png(&png(2048, 1025, &vec![0; 2048 * 1025 * 4])).is_none());
    }
    #[test]
    fn crops_pixels_without_swapping_rows_and_preserves_straight_alpha() {
        let bytes = [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 11, 22, 33, 0];
        let key = ImageKey {
            generation: 7,
            crop: [1, 0, 1, 2],
        };
        let pixels = crop_bgra(key, 2, 2, 1, &bytes).unwrap();
        assert_eq!((pixels.width, pixels.height), (1, 2));
        assert_eq!(pixels.bgra, [0, 255, 0, 128, 33, 22, 11, 0]);
        assert_eq!(pixels.key, key);
    }
    #[test]
    fn rejects_impossible_lengths_dimensions_and_crops_before_allocating() {
        for (w, h, f, len, crop) in [
            (u32::MAX, 2, 1, 8, [0; 4]),
            (4096, 4096, 1, 67108864, [0; 4]),
            (2, 2, 1, 15, [0; 4]),
            (2, 2, 2, 16, [0; 4]),
            (2, 2, 1, 16, [2, 0, 1, 1]),
            (2, 2, 1, 16, [u32::MAX, 0, 2, 1]),
        ] {
            assert!(layout(w, h, f, len, crop).is_none());
        }
        let key = ImageKey {
            generation: 1,
            crop: [0; 4],
        };
        assert_eq!(
            crop_bgra(key, 1, 1, 0, &[1, 2, 3]).unwrap().bgra,
            [3, 2, 1, 255]
        );
        assert_eq!(
            crop_bgra(key, 1, 1, 3, &[9, 31]).unwrap().bgra,
            [9, 9, 9, 31]
        );
    }
}
