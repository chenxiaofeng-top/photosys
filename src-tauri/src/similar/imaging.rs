use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{FixedOffset, TimeZone, Timelike};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, RgbImage};
use image_hasher::{HashAlg, Hasher, HasherConfig};
use nom_exif::{EntryValue, ExifTag, MediaParser, MediaSource};

use super::matching::{CaptureTime, Features};

pub(super) const MAX_PIXELS: u64 = 64_000_000;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn supported(path: &Path) -> bool {
    matches!(
        ImageFormat::from_path(path),
        Ok(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP | ImageFormat::Tiff)
    )
}

pub(super) fn recognizes_format(path: &Path) -> bool {
    ImageFormat::from_path(path).is_ok()
}

pub(super) fn hasher() -> Hasher<[u8; 8]> {
    HasherConfig::with_bytes_type()
        .hash_size(8, 8)
        .hash_alg(HashAlg::Median)
        .preproc_dct()
        .to_hasher()
}

fn scaled(file: &mut File, size: u64, edge: u32) -> Result<(RgbImage, u32, u32), String> {
    if size > MAX_FILE_BYTES {
        return Err("文件超过 128 MiB，已跳过。".into());
    }
    let mut reader = ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP | ImageFormat::Tiff)
    ) {
        return Err("文件内容不是可解码的 JPG、PNG、WebP 或 TIFF。".into());
    }

    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let (mut width, mut height) = decoder.dimensions();
    if width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > MAX_PIXELS
        || decoder.total_bytes() > MAX_DECODE_BYTES
    {
        return Err("照片超过解码大小限制。".into());
    }
    let orientation = decoder.orientation().map_err(|error| error.to_string())?;
    let image = DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    let mut image = if width > edge || height > edge {
        image.thumbnail(edge, edge)
    } else {
        image
    };
    image.apply_orientation(orientation);
    if matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    ) {
        std::mem::swap(&mut width, &mut height);
    }

    let rgb =
        if image.color().has_alpha() {
            let rgba = image.into_rgba8();
            RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
                let [r, g, b, alpha] = rgba.get_pixel(x, y).0;
                let alpha = u16::from(alpha);
                image::Rgb([r, g, b].map(|value| {
                    ((u16::from(value) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
                }))
            })
        } else {
            image.into_rgb8()
        };
    Ok((rgb, width, height))
}

fn original_time(file: &mut File, size: u64, parser: &mut MediaParser) -> Option<CaptureTime> {
    file.rewind().ok()?;
    let entries = if size <= 1024 {
        let mut bytes = Vec::new();
        file.take(1024).read_to_end(&mut bytes).ok()?;
        parser
            .parse_exif(MediaSource::from_memory(bytes).ok()?)
            .ok()?
    } else {
        parser.parse_exif(MediaSource::seekable(file).ok()?).ok()?
    };
    let mut original = None;
    let mut offset = None;
    let mut nanos = None;
    for entry in entries {
        let Some(value) = entry.value() else { continue };
        match entry.tag().tag() {
            Some(ExifTag::DateTimeOriginal) if original.is_none() => {
                original = match value {
                    EntryValue::DateTime(date) => Some(date.naive_local()),
                    EntryValue::NaiveDateTime(date) => Some(*date),
                    _ => None,
                };
            }
            Some(ExifTag::OffsetTimeOriginal) if offset.is_none() => {
                offset = value
                    .as_str()
                    .and_then(|value| value.trim().parse::<FixedOffset>().ok());
            }
            Some(ExifTag::SubSecTimeOriginal) if nanos.is_none() => {
                nanos = value.as_str().and_then(|value| {
                    let digits: String = value.trim().chars().take(9).collect();
                    let count = digits.len() as u32;
                    digits
                        .parse::<u32>()
                        .ok()
                        .map(|value| value * 10_u32.pow(9 - count))
                });
            }
            _ => {}
        }
    }
    let mut original = original?;
    if let Some(nanos) = nanos {
        original = original.with_nanosecond(nanos).unwrap_or(original);
    }
    if let Some(date) = offset.and_then(|value| value.from_local_datetime(&original).single()) {
        Some(CaptureTime {
            label: date.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false),
            millis: date.timestamp_millis(),
            has_offset: true,
        })
    } else {
        Some(CaptureTime {
            label: original.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
            millis: original.and_utc().timestamp_millis(),
            has_offset: false,
        })
    }
}

pub(super) fn features(
    file: &mut File,
    size: u64,
    parser: &mut MediaParser,
    hasher: &Hasher<[u8; 8]>,
) -> Result<Features, String> {
    let (image, width, height) = scaled(file, size, 256)?;
    let hash = hasher.hash_image(&image);
    let rgb = image::imageops::resize(&image, 16, 16, FilterType::Triangle);
    Ok(Features {
        width,
        height,
        hash: u64::from_le_bytes(hash.as_bytes().try_into().expect("eight-byte image hash")),
        rgb: rgb
            .into_raw()
            .into_boxed_slice()
            .try_into()
            .expect("16 by 16 RGB"),
        captured: original_time(file, size, parser),
    })
}

pub(super) fn preview(file: &mut File, size: u64, detail: bool) -> Result<String, String> {
    let (image, _, _) = scaled(file, size, if detail { 1600 } else { 240 })?;
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, if detail { 90 } else { 80 })
        .encode_image(&image)
        .map_err(|error| error.to_string())?;
    Ok(format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg)))
}
