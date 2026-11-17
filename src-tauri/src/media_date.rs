use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::Path;
use std::sync::LazyLock;

use chrono::{Datelike, NaiveDate};
use nom_exif::{EntryValue, ExifTag, MediaParser, MediaSource, TrackInfoTag};
use regex::Regex;
use zip::ZipArchive;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Photo,
    Video,
    LivePhoto,
}

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

pub fn from_metadata(parser: &mut MediaParser, path: &Path, media: Media) -> Option<NaiveDate> {
    from_open_file(parser, File::open(path).ok()?, media)
}

pub(crate) fn from_open_file(
    parser: &mut MediaParser,
    file: File,
    media: Media,
) -> Option<NaiveDate> {
    match media {
        Media::LivePhoto => live_photo_date(parser, file),
        _ => parse_file(parser, file, media),
    }
}

fn parse_file(parser: &mut MediaParser, mut file: File, media: Media) -> Option<NaiveDate> {
    // nom-exif's streaming parser can hit EOF after sniffing the whole of a small file.
    if file.metadata().ok()?.len() <= 1024 {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        return parse_date(parser, MediaSource::from_memory(bytes).ok()?, media);
    }
    parse_date(parser, MediaSource::seekable(file).ok()?, media)
}

fn parse_date(
    parser: &mut MediaParser,
    source: MediaSource<impl io::Read>,
    media: Media,
) -> Option<NaiveDate> {
    match media {
        Media::Photo => parser
            .parse_exif(source)
            .ok()?
            .filter(|entry| entry.tag().tag() == Some(ExifTag::DateTimeOriginal))
            .filter_map(|entry| value_date(entry.value()?))
            .find(|&date| valid_year(date)),
        Media::Video => {
            let track = parser.parse_track(source).ok()?;
            let value = track.get(TrackInfoTag::CreateDate)?;
            // An unset QuickTime movie-header timestamp is stored as zero.
            if matches!(value, EntryValue::DateTime(date) if date.timestamp() == -2_082_844_800) {
                return None;
            }
            let date = value_date(value)?;
            valid_year(date).then_some(date)
        }
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

fn valid_year(date: NaiveDate) -> bool {
    (1..=9999).contains(&date.year())
}

fn live_photo_date(parser: &mut MediaParser, file: File) -> Option<NaiveDate> {
    let mut archive = ZipArchive::new(file).ok()?;
    for media in [Media::Photo, Media::Video] {
        for index in 0..archive.len() {
            let Ok(mut member) = archive.by_index(index) else {
                continue;
            };
            if !member.is_file() || Media::from_path(Path::new(member.name())) != Some(media) {
                continue;
            }
            // MOV metadata can be at the end. Spool one entry at a time for seekable parsing.
            let mut file = tempfile::tempfile().ok()?;
            if io::copy(&mut member, &mut file).is_ok()
                && file.rewind().is_ok()
                && let Some(date) = parse_file(parser, file, media)
            {
                return Some(date);
            }
        }
    }
    None
}
