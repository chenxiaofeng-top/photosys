use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::Path;
use std::sync::LazyLock;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate};
use nom_exif::{EntryValue, ExifIter, ExifTag, MediaParser, MediaSource, TrackInfo, TrackInfoTag};
use regex::Regex;
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Photo,
    Video,
    LivePhoto,
}

pub(crate) const LIVE_MEMBER_LIMIT: u64 = 128 * 1024 * 1024;

impl Media {
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "jpg" | "jpeg" | "png" | "heic" | "heif" | "dng" | "tif" | "tiff" | "webp" | "avif" => {
                Some(Self::Photo)
            }
            "mov" | "mp4" | "m4v" | "3gp" | "mkv" | "webm" => Some(Self::Video),
            "livp" => Some(Self::LivePhoto),
            _ => None,
        }
    }
}

static DATE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9]{4}(?:-[0-9]{2}-[0-9]{2}|_[0-9]{2}_[0-9]{2}|[0-9]{4})").unwrap()
});

pub fn from_filename(path: &Path) -> Option<NaiveDate> {
    let stem = path.file_stem()?.to_string_lossy();
    let bytes = stem.as_bytes();
    // An invalid date can overlap a later valid one, so restart just past its start.
    let mut offset = 0;
    while let Some(candidate) = DATE_PATTERN.find_at(&stem, offset) {
        offset = candidate.start() + 1;
        if candidate.start() > 0 && bytes[candidate.start() - 1].is_ascii_digit()
            || bytes.get(candidate.end()).is_some_and(u8::is_ascii_digit)
        {
            continue;
        }

        let format = match bytes[candidate.start() + 4] {
            b'-' => "%Y-%m-%d",
            b'_' => "%Y_%m_%d",
            _ => "%Y%m%d",
        };
        if let Ok(date) = NaiveDate::parse_from_str(candidate.as_str(), format)
            && valid_year(date)
        {
            return Some(date);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Coordinates {
    pub latitude: f64,
    pub longitude: f64,
}

impl Coordinates {
    pub(crate) fn new(latitude: f64, longitude: f64) -> Option<Self> {
        (latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude))
        .then_some(Self {
            latitude,
            longitude,
        })
    }
    fn from_gps(gps: &nom_exif::GPSInfo) -> Option<Self> {
        Self::new(gps.latitude_decimal()?, gps.longitude_decimal()?)
    }
}

#[derive(Default, Clone, Debug)]
pub(crate) struct Metadata {
    pub date: Option<NaiveDate>,
    pub coordinates: Option<Coordinates>,
}

pub fn from_metadata(parser: &mut MediaParser, path: &Path, media: Media) -> Option<NaiveDate> {
    let file = File::open(path).ok()?;
    metadata_from_file(parser, file, media, None).date
}

pub(crate) fn read_metadata(parser: &mut MediaParser, file: File, media: Media) -> Metadata {
    metadata_from_file(parser, file, media, Some(LIVE_MEMBER_LIMIT))
}

fn metadata_from_file(
    parser: &mut MediaParser,
    file: File,
    media: Media,
    limit: Option<u64>,
) -> Metadata {
    if media == Media::LivePhoto {
        live_photo_metadata(parser, file, limit)
    } else {
        parse_file(parser, file, media).unwrap_or_default()
    }
}

fn parse_file(parser: &mut MediaParser, mut file: File, media: Media) -> Option<Metadata> {
    match read_embedded_metadata(parser, &mut file, media)? {
        EmbeddedMetadata::Photo(exif) => {
            // parse_gps borrows/clones the EXIF iterator; the media is opened and parsed once.
            let coordinates = exif
                .parse_gps()
                .ok()
                .flatten()
                .as_ref()
                .and_then(Coordinates::from_gps);
            let date = exif
                .filter(|entry| entry.tag().tag() == Some(ExifTag::DateTimeOriginal))
                .filter_map(|entry| value_date(entry.value()?))
                .find(|&date| valid_year(date));
            Some(Metadata { date, coordinates })
        }
        EmbeddedMetadata::Video(track) => {
            let coordinates = track.gps_info().and_then(Coordinates::from_gps);
            let date = track.get(TrackInfoTag::CreateDate).and_then(|value| {
                if matches!(value, EntryValue::DateTime(date) if !valid_video_creation_time(*date))
                {
                    return None;
                }
                value_date(value).filter(|&date| valid_year(date))
            });
            Some(Metadata { date, coordinates })
        }
    }
}

pub(crate) enum EmbeddedMetadata {
    Photo(ExifIter),
    Video(TrackInfo),
}

pub(crate) fn read_embedded_metadata(
    parser: &mut MediaParser,
    file: &mut File,
    media: Media,
) -> Option<EmbeddedMetadata> {
    file.rewind().ok()?;
    // The streaming parser can hit EOF after sniffing the whole of a small file.
    if file.metadata().ok()?.len() <= 1024 {
        let mut bytes = Vec::new();
        file.take(1024).read_to_end(&mut bytes).ok()?;
        return parse_embedded(parser, MediaSource::from_memory(bytes).ok()?, media);
    }
    parse_embedded(parser, MediaSource::seekable(file).ok()?, media)
}

fn parse_embedded(
    parser: &mut MediaParser,
    source: MediaSource<impl io::Read>,
    media: Media,
) -> Option<EmbeddedMetadata> {
    match media {
        Media::Photo => parser.parse_exif(source).ok().map(EmbeddedMetadata::Photo),
        Media::Video => parser.parse_track(source).ok().map(EmbeddedMetadata::Video),
        Media::LivePhoto => None,
    }
}

fn value_date(value: &EntryValue) -> Option<NaiveDate> {
    match value {
        EntryValue::NaiveDateTime(date) => Some(date.date()),
        EntryValue::DateTime(date) => Some(date.date_naive()),
        _ => None,
    }
}

pub(crate) fn valid_year(date: NaiveDate) -> bool {
    (1..=9999).contains(&date.year())
}

pub(crate) fn valid_video_creation_time(date: DateTime<FixedOffset>) -> bool {
    date.timestamp() != -2_082_844_800 && valid_year(date.date_naive())
}

fn live_photo_metadata(
    parser: &mut MediaParser,
    file: File,
    member_limit: Option<u64>,
) -> Metadata {
    let Ok(mut archive) = ZipArchive::new(file) else {
        return Metadata::default();
    };
    let mut result = Metadata::default();
    for media in [Media::Photo, Media::Video] {
        for index in 0..archive.len() {
            let Ok(mut member) = archive.by_index(index) else {
                continue;
            };
            if !member.is_file() || Media::from_path(Path::new(member.name())) != Some(media) {
                continue;
            }
            let expected = member.size();
            if member_limit.is_some_and(|limit| expected > limit) {
                continue;
            }
            // MOV metadata can be at the end. Spool one entry at a time for seekable parsing.
            let Ok(mut file) = tempfile::tempfile() else {
                continue;
            };
            let copied = match member_limit {
                Some(limit) => io::copy(&mut member.take(limit + 1), &mut file)
                    .is_ok_and(|size| size == expected),
                None => io::copy(&mut member, &mut file).is_ok(),
            };
            if copied
                && file.rewind().is_ok()
                && let Some(metadata) = parse_file(parser, file, media)
            {
                result.date = result.date.or(metadata.date);
                result.coordinates = result.coordinates.or(metadata.coordinates);
                if result.date.is_some() && result.coordinates.is_some() {
                    return result;
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod location_tests {
    use super::*;
    use image::{ImageEncoder, RgbImage, codecs::jpeg::JpegEncoder};
    use nom_exif::GPSInfo;
    use std::io::Write;

    fn entry(bytes: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32) {
        bytes.extend(tag.to_le_bytes());
        bytes.extend(kind.to_le_bytes());
        bytes.extend(count.to_le_bytes());
        bytes.extend(value.to_le_bytes());
    }
    fn jpeg(lat: [u32; 3], lon: [u32; 3], south: bool, west: bool, denominator: u32) -> Vec<u8> {
        // IFD0 points to independent EXIF and GPS directories.
        let mut exif = b"II*\0\x08\0\0\0".to_vec();
        exif.extend(2u16.to_le_bytes());
        entry(&mut exif, 0x8769, 4, 1, 38);
        entry(&mut exif, 0x8825, 4, 1, 76);
        exif.extend(0u32.to_le_bytes());
        exif.extend(1u16.to_le_bytes());
        entry(&mut exif, 0x9003, 2, 20, 56);
        exif.extend(0u32.to_le_bytes());
        exif.extend(b"2024:02:29 12:30:00\0");
        assert_eq!(exif.len(), 76);
        exif.extend(4u16.to_le_bytes());
        entry(&mut exif, 1, 2, 2, if south { b'S' } else { b'N' } as u32);
        entry(&mut exif, 2, 5, 3, 130);
        entry(&mut exif, 3, 2, 2, if west { b'W' } else { b'E' } as u32);
        entry(&mut exif, 4, 5, 3, 154);
        exif.extend(0u32.to_le_bytes());
        for coordinate in [lat, lon] {
            for value in coordinate {
                exif.extend(value.to_le_bytes());
                exif.extend(denominator.to_le_bytes());
            }
        }
        let mut bytes = Vec::new();
        let mut encoder = JpegEncoder::new(&mut bytes);
        encoder.set_exif_metadata(exif).unwrap();
        encoder.encode_image(&RgbImage::new(8, 8)).unwrap();
        bytes
    }
    fn read(bytes: &[u8], media: Media) -> Metadata {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        read_metadata(&mut MediaParser::new(), file, media)
    }
    fn atom(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend(name);
        b.extend(payload);
        b
    }
    fn video(location: &str) -> Vec<u8> {
        let original = include_bytes!("../tests/fixtures/video-preview.mp4");
        let mut xyz = (location.len() as u16).to_be_bytes().to_vec();
        xyz.extend([0, 0]);
        xyz.extend(location.as_bytes());
        let xyz = atom(b"\xa9xyz", &xyz);
        let mut output = Vec::new();
        let mut offset = 0;
        while offset + 8 <= original.len() {
            let len = u32::from_be_bytes(original[offset..offset + 4].try_into().unwrap()) as usize;
            if &original[offset + 4..offset + 8] == b"moov" {
                let mut payload = original[offset + 8..offset + len].to_vec();
                let mut child = 0;
                let mut merged = false;
                while child + 8 <= payload.len() {
                    let size =
                        u32::from_be_bytes(payload[child..child + 4].try_into().unwrap()) as usize;
                    if &payload[child + 4..child + 8] == b"udta" {
                        let mut udta = payload[child + 8..child + size].to_vec();
                        udta.extend(&xyz);
                        payload.splice(child..child + size, atom(b"udta", &udta));
                        merged = true;
                        break;
                    }
                    child += size;
                }
                if !merged {
                    payload.extend(atom(b"udta", &xyz));
                }
                output.extend(atom(b"moov", &payload));
            } else {
                output.extend(&original[offset..offset + len]);
            }
            offset += len;
        }
        output
    }
    #[test]
    fn exif_date_and_signed_gps_are_read_together_including_zero() {
        let data = read(&jpeg([33, 30, 0], [70, 15, 0], true, true, 1), Media::Photo);
        assert_eq!(data.date.unwrap().to_string(), "2024-02-29");
        assert_eq!(data.coordinates, Coordinates::new(-33.5, -70.25));
        assert_eq!(
            read(&jpeg([0, 0, 0], [0, 0, 0], false, false, 1), Media::Photo).coordinates,
            Coordinates::new(0., 0.)
        );
        assert!(
            read(&jpeg([90, 1, 0], [0, 0, 0], false, false, 1), Media::Photo)
                .coordinates
                .is_none()
        );
        let broken = read(
            &jpeg([33, 30, 0], [70, 15, 0], false, false, 0),
            Media::Photo,
        );
        assert!(broken.coordinates.is_none());
        assert!(broken.date.is_some());
        assert!(read(b"broken", Media::Photo).coordinates.is_none());
    }
    #[test]
    fn video_iso6709_and_live_photo_cover_precedence() {
        let bytes = video("-33.5000-070.2500/");
        let data = read(&bytes, Media::Video);
        assert_eq!(data.coordinates, Coordinates::new(-33.5, -70.25));
        let original = read(
            include_bytes!("../tests/fixtures/video-preview.mp4"),
            Media::Video,
        );
        assert_eq!(data.date, original.date);
        assert!(read(&video("invalid"), Media::Video).coordinates.is_none());
        let mut output = std::io::Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(&mut output);
        archive
            .start_file("live.mov", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(&bytes).unwrap();
        archive
            .start_file("cover.jpg", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(&jpeg([22, 30, 0], [114, 0, 0], false, false, 1))
            .unwrap();
        archive.finish().unwrap();
        let live = read(output.get_ref(), Media::LivePhoto);
        assert_eq!(live.coordinates, Coordinates::new(22.5, 114.));
        assert_eq!(live.date.unwrap().to_string(), "2024-02-29");
    }
    #[test]
    fn rational_coordinate_conversion_rejects_bad_ranges() {
        let gps: GPSInfo = "-33.5000-070.2500/".parse().unwrap();
        assert_eq!(Coordinates::from_gps(&gps), Coordinates::new(-33.5, -70.25));
        assert!(Coordinates::new(0., f64::INFINITY).is_none());
        assert!(Coordinates::new(91., 0.).is_none());
    }
}
