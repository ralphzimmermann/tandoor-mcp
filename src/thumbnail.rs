//! Bounded recipe-photo decoding and privacy-preserving thumbnail encoding.

use std::io::Cursor;

use anyhow::{Context, Result};
use exif::{In, Reader as ExifReader, Tag};
use image::{
    codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage, ImageReader, Limits, Rgb,
    RgbImage,
};

const MAX_IMAGE_WIDTH: u32 = 12_000;
const MAX_IMAGE_HEIGHT: u32 = 12_000;
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;
const JPEG_QUALITY: u8 = 80;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decode one image, apply EXIF orientation, resize it, and emit metadata-free JPEG.
pub fn make_thumbnail(bytes: &[u8], max_edge: u32) -> Result<Thumbnail> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("Could not identify recipe image format")?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_WIDTH);
    limits.max_image_height = Some(MAX_IMAGE_HEIGHT);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);

    let decoded = reader.decode().context("Could not decode recipe image")?;
    let oriented = apply_orientation(decoded, exif_orientation(bytes));
    let resized = oriented.resize(max_edge, max_edge, FilterType::Lanczos3);
    let rgba = resized.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut flattened = RgbImage::new(width, height);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = u16::from(pixel[3]);
        let blend = |channel: u8| -> u8 {
            ((u16::from(channel) * alpha + 255 * (255 - alpha)) / 255) as u8
        };
        flattened.put_pixel(
            x,
            y,
            Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
        );
    }

    let mut output = Vec::new();
    JpegEncoder::new_with_quality(&mut output, JPEG_QUALITY)
        .encode_image(&DynamicImage::ImageRgb8(flattened))
        .context("Could not encode recipe thumbnail")?;
    Ok(Thumbnail {
        bytes: output,
        width,
        height,
    })
}

fn exif_orientation(bytes: &[u8]) -> u32 {
    ExifReader::new()
        .read_raw(bytes.to_vec())
        .ok()
        .and_then(|exif| {
            exif.get_field(Tag::Orientation, In::PRIMARY)
                .and_then(|field| field.value.get_uint(0))
        })
        .unwrap_or(1)
}

fn apply_orientation(image: DynamicImage, orientation: u32) -> DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};

    fn png(width: u32, height: u32, pixel: Rgba<u8>) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, pixel);
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn resizes_and_encodes_jpeg() {
        let thumbnail = make_thumbnail(&png(1200, 600, Rgba([20, 40, 60, 255])), 512).unwrap();
        assert_eq!((thumbnail.width, thumbnail.height), (512, 256));
        assert!(thumbnail.bytes.starts_with(&[0xff, 0xd8, 0xff]));
    }

    #[test]
    fn transparent_pixels_are_flattened_onto_white() {
        let thumbnail = make_thumbnail(&png(2, 2, Rgba([0, 0, 0, 0])), 512).unwrap();
        let decoded = image::load_from_memory(&thumbnail.bytes).unwrap().to_rgb8();
        assert!(decoded
            .pixels()
            .all(|pixel| pixel.0.iter().all(|value| *value > 245)));
    }

    #[test]
    fn applies_exif_rotation_orientation() {
        let image = DynamicImage::ImageRgb8(RgbImage::new(2, 3));
        let oriented = apply_orientation(image, 6);
        assert_eq!((oriented.width(), oriented.height()), (3, 2));
    }

    #[test]
    fn rejects_dimensions_above_decode_limit() {
        // PNG header with dimensions over the configured limit; no large allocation is made.
        let mut bytes = png(1, 1, Rgba([0, 0, 0, 255]));
        bytes[16..20].copy_from_slice(&12_001_u32.to_be_bytes());
        assert!(make_thumbnail(&bytes, 512).is_err());
    }
}
