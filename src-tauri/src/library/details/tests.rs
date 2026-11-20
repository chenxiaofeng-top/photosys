use std::fs;
use std::io::{Cursor, Write};

use image::{ImageEncoder, ImageFormat, RgbImage, codecs::jpeg::JpegEncoder};
use tempfile::tempdir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use super::*;
use crate::duplicates::Root;

const MP4: &[u8] = include_bytes!("../../../tests/fixtures/video-preview.mp4");

fn check_details(path: &Path) -> MediaDetails {
    let scan = Scan::read(1, Root::new(path).unwrap(), |_| {});
    assert_eq!(scan.items.len(), 1);
    scan.media_details(0).unwrap()
}

fn write_ifd(data: &mut Vec<u8>, entries: &[(u16, u16, u32, &[u8])]) {
    let payload_start = data.len() + 2 + 12 * entries.len() + 4;
    let mut payload = Vec::new();
    data.extend((entries.len() as u16).to_le_bytes());
    for &(tag, format, count, value) in entries {
        data.extend(tag.to_le_bytes());
        data.extend(format.to_le_bytes());
        data.extend(count.to_le_bytes());
        if value.len() <= 4 {
            let mut field = [0; 4];
            field[..value.len()].copy_from_slice(value);
            data.extend(field);
        } else {
            data.extend(((payload_start + payload.len()) as u32).to_le_bytes());
            payload.extend_from_slice(value);
        }
    }
    data.extend([0; 4]);
    data.extend_from_slice(&payload);
}

#[test]
fn reads_original_photo_properties_and_exif_without_using_preview_size() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("portrait.jpg");
    let mut data = b"II*\0\x08\0\0\0".to_vec();
    write_ifd(
        &mut data,
        &[
            (0x010f, 2, 5, b"ACME\0"),
            (0x0110, 2, 8, b"ACME X1\0"),
            (0x0112, 3, 1, &6_u16.to_le_bytes()),
            (0x8769, 4, 1, &0_u32.to_le_bytes()),
        ],
    );
    let exif_offset = (data.len() as u32).to_le_bytes();
    let offset_field = 8 + 2 + 12 * 3 + 8;
    data[offset_field..offset_field + 4].copy_from_slice(&exif_offset);
    write_ifd(
        &mut data,
        &[
            (0x829a, 5, 1, &[1, 0, 0, 0, 250, 0, 0, 0]),
            (0x829d, 5, 1, &[28, 0, 0, 0, 10, 0, 0, 0]),
            (0x8827, 3, 1, &400_u16.to_le_bytes()),
            (0x9003, 2, 20, b"2026:03:02 13:14:15\0"),
            (0x9011, 2, 7, b"+08:00\0"),
            (0x920a, 5, 1, &[50, 0, 0, 0, 1, 0, 0, 0]),
            (0x9291, 2, 4, b"123\0"),
            (0xa434, 2, 10, b"ACME 50mm\0"),
        ],
    );
    let mut jpeg = JpegEncoder::new(File::create(path).unwrap());
    jpeg.set_exif_metadata(data).unwrap();
    jpeg.encode_image(&RgbImage::new(1800, 32)).unwrap();
    let details = check_details(directory.path());
    assert!(!details.partial && details.modified_at.is_some());
    let photo = details.photo.unwrap();
    assert_eq!(photo.dimensions, Dimensions::new((1800, 32)));
    assert_eq!(
        photo.captured_at.as_deref(),
        Some("2026-03-02T13:14:15.123+08:00")
    );
    assert_eq!(photo.make.as_deref(), Some("ACME"));
    assert_eq!(photo.model.as_deref(), Some("ACME X1"));
    assert_eq!(photo.lens.as_deref(), Some("ACME 50mm"));
    assert_eq!(photo.focal_length_mm, Some(50.0));
    assert_eq!(photo.aperture, Some(2.8));
    assert_eq!(photo.exposure_seconds, Some(1.0 / 250.0));
    assert_eq!(photo.iso, Some(400));
}

#[test]
fn reads_dimensions_without_exif_and_rejects_replaced_files() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("plain.png");
    RgbImage::new(64, 32).save(&path).unwrap();
    let scan = Scan::read(1, Root::new(directory.path()).unwrap(), |_| {});
    let details = scan.media_details(0).unwrap();
    assert!(!details.partial);
    let photo = details.photo.unwrap();
    assert_eq!(photo.dimensions, Dimensions::new((64, 32)));
    assert!(photo.captured_at.is_none() && photo.iso.is_none());
    fs::write(&path, b"replacement").unwrap();
    assert!(matches!(scan.media_details(0), Err(error) if error == CHANGED));
}

#[test]
fn reads_primary_heic_grid_dimensions_instead_of_tiles_or_rotation() {
    for (width, height, fixture) in [
        (
            2048,
            1024,
            include_bytes!("../../../tests/fixtures/portrait-grid.heic").as_slice(),
        ),
        (
            8064,
            6048,
            include_bytes!("../../../tests/fixtures/portrait-48mp-grid.heic").as_slice(),
        ),
    ] {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("grid.heic"), fixture).unwrap();
        let details = check_details(directory.path());
        assert!(!details.partial);
        let photo = details.photo.unwrap();
        assert_eq!(photo.dimensions, Dimensions::new((width, height)));
    }
}

#[test]
fn probes_supported_video_containers_and_unrotated_media_dimensions() {
    for (name, fixture, codec, dimensions) in [
        ("clip.mp4", MP4, "h264", (320, 180)),
        (
            "clip.mov",
            include_bytes!("../../../tests/fixtures/video-rotated.mov").as_slice(),
            "h264",
            (1800, 600),
        ),
        (
            "clip.mkv",
            include_bytes!("../../../tests/fixtures/video-preview.mkv").as_slice(),
            "mpeg4",
            (320, 180),
        ),
        (
            "clip.webm",
            include_bytes!("../../../tests/fixtures/video-preview.webm").as_slice(),
            "vp9",
            (128, 72),
        ),
        (
            "clip.3gp",
            include_bytes!("../../../tests/fixtures/video-preview.3gp").as_slice(),
            "h263",
            (176, 144),
        ),
    ] {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join(name), fixture).unwrap();
        let details = check_details(directory.path());
        assert!(!details.partial, "{name}");
        let video = details.video.unwrap();
        assert_eq!(video.dimensions, Dimensions::new(dimensions), "{name}");
        assert_eq!(video.codec.as_deref(), Some(codec), "{name}");
        assert_eq!(video.average_frame_rate, Some(10.0), "{name}");
        assert!(
            video.duration_seconds.is_some_and(|value| value > 0.0),
            "{name}"
        );
        assert!(video.bit_rate.is_some_and(|value| value > 0), "{name}");
        assert_eq!(video.has_audio, Some(false));
    }
}

#[test]
fn distinguishes_audio_only_from_an_unreadable_video() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("audio.mp4");
    fs::write(
        &path,
        include_bytes!("../../../tests/fixtures/video-audio-only.mp4"),
    )
    .unwrap();
    let details = check_details(directory.path());
    assert!(!details.partial);
    let audio = details.video.unwrap();
    assert!(audio.dimensions.is_none() && audio.average_frame_rate.is_none());
    assert_eq!(audio.has_audio, Some(true));
    assert_eq!(audio.audio_codec.as_deref(), Some("aac"));
    assert_eq!(audio.sample_rate, Some(44_100));
    assert_eq!(audio.channels, Some(1));

    fs::write(&path, b"broken MP4").unwrap();
    let details = check_details(directory.path());
    assert!(details.partial && details.modified_at.is_some());
    assert_eq!(details.video.unwrap().has_audio, None);
}

#[test]
fn reads_the_live_cover_and_clip_as_distinct_media() {
    let directory = tempdir().unwrap();
    let mut package = ZipWriter::new(File::create(directory.path().join("live.livp")).unwrap());
    let mut cover = Cursor::new(Vec::new());
    RgbImage::new(52, 24)
        .write_to(&mut cover, ImageFormat::Png)
        .unwrap();
    for (name, bytes) in [
        ("__MACOSX/ignored.jpg", b"ignored".as_slice()),
        ("cover.png", cover.get_ref()),
        ("video.mp4", MP4),
    ] {
        package
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        package.write_all(bytes).unwrap();
    }
    package.finish().unwrap();
    let details = check_details(directory.path());
    assert!(!details.partial && details.video.is_none());
    assert_eq!(details.photo.unwrap().dimensions, Dimensions::new((52, 24)));
    assert_eq!(
        details.live_video.unwrap().dimensions,
        Dimensions::new((320, 180))
    );
}
