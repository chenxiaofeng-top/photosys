use std::fs::File;
use std::io::{BufReader, Seek};
use std::path::Path;

use chrono::{DateTime, FixedOffset, SecondsFormat, Timelike, Utc};
use image::ImageReader;
use nom_exif::{EntryValue, Exif, ExifIter, ExifTag, MediaParser, TrackInfo, TrackInfoTag};
use serde::Serialize;
use tokio::sync::Semaphore;

use super::{STATE_ERROR, Scan, live, previews::CHANGED, scan};
use crate::{
    duplicates::{Candidate, Root},
    media_codec::{self, probe},
    media_date::{
        EmbeddedMetadata, Media, read_embedded_metadata, valid_video_creation_time, valid_year,
    },
};

pub(crate) static DETAILS_SLOTS: Semaphore = Semaphore::const_new(2);

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaDetails {
    modified_at: Option<String>,
    photo: Option<PhotoDetails>,
    video: Option<VideoDetails>,
    live_video: Option<VideoDetails>,
    partial: bool,
}

#[derive(Debug, PartialEq, Serialize)]
struct Dimensions {
    width: u32,
    height: u32,
}

impl Dimensions {
    fn new((width, height): (u32, u32)) -> Option<Self> {
        (width > 0 && height > 0).then_some(Self { width, height })
    }
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PhotoDetails {
    dimensions: Option<Dimensions>,
    captured_at: Option<String>,
    make: Option<String>,
    model: Option<String>,
    lens: Option<String>,
    focal_length_mm: Option<f64>,
    aperture: Option<f64>,
    exposure_seconds: Option<f64>,
    iso: Option<u32>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoDetails {
    dimensions: Option<Dimensions>,
    created_at: Option<String>,
    duration_seconds: Option<f64>,
    average_frame_rate: Option<f64>,
    codec: Option<String>,
    bit_rate: Option<u64>,
    has_audio: Option<bool>,
    audio_codec: Option<String>,
    sample_rate: Option<u32>,
    channels: Option<u32>,
}

impl Scan {
    fn media_details(&self, file_id: usize) -> Result<MediaDetails, String> {
        read_media_details(&self.root, &self.item(file_id)?.file)
    }
}

pub(crate) fn read_media_details(
    root: &Root,
    candidate: &Candidate,
) -> Result<MediaDetails, String> {
    let mut file = root.open_matching(candidate).map_err(|_| CHANGED)?;
    let path = &candidate.relative;
    let mut details = MediaDetails {
        modified_at: file
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(|time| DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Millis, true)),
        ..Default::default()
    };
    let mut parser = MediaParser::new();
    match Media::from_path(path) {
        Some(Media::Photo) => {
            let is_isobmff = media_codec::is_heif(path)
                || path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("avif"));
            let photo = photo_details(&mut file, is_isobmff, &mut parser);
            details.partial = photo.dimensions.is_none();
            details.photo = Some(photo);
        }
        Some(Media::Video) => {
            let (video, partial) = video_details(&mut file, path, &mut parser);
            details.video = Some(video);
            details.partial = partial;
        }
        Some(Media::LivePhoto) => live_details(&mut file, &mut parser, &mut details),
        None => return Err("无法识别媒体格式。".into()),
    }
    root.verify_read(&file, candidate).map_err(|_| CHANGED)?;
    Ok(details)
}

fn live_details(file: &mut File, parser: &mut MediaParser, details: &mut MediaDetails) {
    if let Ok(cover) = live::cover_member(file)
        && let Ok(mut image) = File::open(&cover.path)
    {
        details.photo = Some(photo_details(&mut image, cover.is_heif(), parser));
    }
    details.partial = details
        .photo
        .as_ref()
        .is_none_or(|photo| photo.dimensions.is_none());
    if let Ok(member) = live::video_member(file)
        && let Ok(mut video) = File::open(&member.path)
    {
        let (video, partial) = video_details(&mut video, &member.path, parser);
        details.live_video = Some(video);
        details.partial |= partial;
    } else {
        details.partial = true;
    }
}

fn photo_details(file: &mut File, is_isobmff: bool, parser: &mut MediaParser) -> PhotoDetails {
    let dimensions = if is_isobmff {
        probe::photo_dimensions(file)
    } else {
        image_dimensions(file)
    };
    let mut details = PhotoDetails {
        dimensions: dimensions.and_then(Dimensions::new),
        ..Default::default()
    };
    if let Some(EmbeddedMetadata::Photo(exif)) = read_embedded_metadata(parser, file, Media::Photo)
    {
        read_photo_exif(exif, &mut details);
    }
    details
}

fn image_dimensions(file: &mut File) -> Option<(u32, u32)> {
    file.rewind().ok()?;
    ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn read_photo_exif(entries: ExifIter, details: &mut PhotoDetails) {
    let exif: Exif = entries.into();
    let get = |tag| exif.get(tag);
    let number = |tag| positive(get(tag)?.as_urational()?.to_f64()?);
    let subseconds = get(ExifTag::SubSecTimeOriginal).and_then(EntryValue::as_str);
    details.captured_at =
        get(ExifTag::DateTimeOriginal).and_then(|value| photo_time(value, subseconds));
    details.make = clean_text(get(ExifTag::Make).and_then(EntryValue::as_str));
    details.model = clean_text(get(ExifTag::Model).and_then(EntryValue::as_str));
    details.lens = clean_text(get(ExifTag::LensModel).and_then(EntryValue::as_str));
    details.focal_length_mm = number(ExifTag::FocalLength);
    details.aperture = number(ExifTag::FNumber);
    details.exposure_seconds = number(ExifTag::ExposureTime);
    details.iso = get(ExifTag::ISOSpeedRatings)
        .and_then(|value| {
            value
                .try_as_integer()
                .or_else(|| value.as_u16_slice()?.first().copied().map(i64::from))
        })
        .and_then(|iso| u32::try_from(iso).ok())
        .filter(|&iso| iso > 0);
}

fn photo_time(value: &EntryValue, subseconds: Option<&str>) -> Option<String> {
    let nanoseconds = subseconds.and_then(|text| {
        let text = text.trim();
        let digits = text.get(..text.len().min(9))?;
        digits
            .parse::<u32>()
            .ok()
            .map(|number| number * 10_u32.pow(9 - digits.len() as u32))
    });
    match value {
        EntryValue::DateTime(date) if valid_year(date.date_naive()) => Some(
            nanoseconds
                .and_then(|value| date.with_nanosecond(value))
                .unwrap_or(*date)
                .to_rfc3339_opts(SecondsFormat::AutoSi, false),
        ),
        EntryValue::NaiveDateTime(date) if valid_year(date.date()) => Some(
            nanoseconds
                .and_then(|value| date.with_nanosecond(value))
                .unwrap_or(*date)
                .format("%Y-%m-%dT%H:%M:%S%.f")
                .to_string(),
        ),
        _ => None,
    }
}

fn positive(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

fn clean_text(value: Option<&str>) -> Option<String> {
    let value =
        value?.trim_matches(|character: char| character.is_whitespace() || character == '\0');
    (!value.is_empty()).then(|| value.chars().take(256).collect())
}

fn video_details(file: &mut File, path: &Path, parser: &mut MediaParser) -> (VideoDetails, bool) {
    let mut details = VideoDetails::default();
    if let Some(EmbeddedMetadata::Video(track)) = read_embedded_metadata(parser, file, Media::Video)
    {
        details.created_at = track_creation_time(&track).map(|date| date.to_rfc3339());
        details.duration_seconds = track
            .get(TrackInfoTag::DurationMs)
            .and_then(EntryValue::as_u64)
            .and_then(|milliseconds| positive(milliseconds as f64 / 1000.0));
    }
    let Some(probe) = probe::video(file, path) else {
        return (details, true);
    };
    let format = &probe.format;
    details.created_at = details.created_at.or_else(|| {
        [
            &format.tags.quicktime_creation_date,
            &format.tags.creation_time,
        ]
        .into_iter()
        .filter_map(|tag| DateTime::parse_from_rfc3339(tag.as_deref()?.trim()).ok())
        .find(|date| valid_video_creation_time(*date))
        .map(|date| date.to_rfc3339())
    });
    details.duration_seconds = format
        .duration
        .as_deref()
        .and_then(|value| positive(value.parse().ok()?))
        .or(details.duration_seconds);
    details.bit_rate = format
        .bit_rate
        .as_deref()
        .and_then(|rate| rate.parse().ok())
        .filter(|&rate| rate > 0);
    if let Some(stream) = probe.streams.iter().find(|stream| stream.is_video()) {
        details.dimensions = stream.width.zip(stream.height).and_then(Dimensions::new);
        details.average_frame_rate = stream.avg_frame_rate.as_deref().and_then(frame_rate);
        details.codec = clean_text(stream.codec_name.as_deref());
    }
    let audio = probe
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"));
    details.has_audio = Some(audio.is_some());
    if let Some(stream) = audio {
        details.audio_codec = clean_text(stream.codec_name.as_deref());
        details.sample_rate = stream
            .sample_rate
            .as_deref()
            .and_then(|rate| rate.parse().ok())
            .filter(|&rate| rate > 0);
        details.channels = stream.channels.filter(|&count| count > 0);
    }
    (details, false)
}

fn track_creation_time(track: &TrackInfo) -> Option<DateTime<FixedOffset>> {
    match track.get(TrackInfoTag::CreateDate)? {
        EntryValue::DateTime(date) if valid_video_creation_time(*date) => Some(*date),
        _ => None,
    }
}

fn frame_rate(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/')?;
    positive(positive(numerator.parse().ok()?)? / positive(denominator.parse().ok()?)?)
}

#[tauri::command]
pub(crate) async fn library_media_details(
    scan_id: u64,
    file_id: usize,
) -> Result<MediaDetails, String> {
    let permit = DETAILS_SLOTS.acquire().await.map_err(|_| STATE_ERROR)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        let details = scan(scan_id)?.media_details(file_id)?;
        scan(scan_id)?;
        Ok(details)
    })
    .await
    .map_err(|error| format!("无法读取媒体详情。{error}"))?
}

#[cfg(test)]
mod tests;
