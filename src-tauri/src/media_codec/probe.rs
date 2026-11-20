use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use super::{media_command, video_demuxer, wait};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT_BYTES: usize = 512 * 1024;
const VIDEO_FIELDS: &str = "format=duration,bit_rate:format_tags=creation_time,com.apple.quicktime.creationdate:stream=codec_type,codec_name,width,height,avg_frame_rate,sample_rate,channels:stream_disposition=attached_pic";
const PHOTO_FIELDS: &str = "stream=codec_type,width,height:stream_disposition=default,attached_pic:stream_group=type:stream_group_disposition=default:stream_group_component=width,height";

#[derive(Default, Deserialize)]
pub(crate) struct Probe {
    #[serde(default)]
    pub(crate) streams: Vec<Stream>,
    #[serde(default)]
    pub(crate) format: Format,
    #[serde(default)]
    stream_groups: Vec<StreamGroup>,
}

#[derive(Default, Deserialize)]
pub(crate) struct Format {
    pub(crate) duration: Option<String>,
    pub(crate) bit_rate: Option<String>,
    #[serde(default)]
    pub(crate) tags: Tags,
}

#[derive(Default, Deserialize)]
pub(crate) struct Tags {
    pub(crate) creation_time: Option<String>,
    #[serde(rename = "com.apple.quicktime.creationdate")]
    pub(crate) quicktime_creation_date: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Stream {
    pub(crate) codec_type: Option<String>,
    pub(crate) codec_name: Option<String>,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) avg_frame_rate: Option<String>,
    pub(crate) sample_rate: Option<String>,
    pub(crate) channels: Option<u32>,
    #[serde(default)]
    disposition: Disposition,
}

impl Stream {
    pub(crate) fn is_video(&self) -> bool {
        self.codec_type.as_deref() == Some("video") && self.disposition.attached_pic == 0
    }
}

#[derive(Deserialize)]
struct StreamGroup {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    disposition: Disposition,
    #[serde(default)]
    components: Vec<GridComponent>,
}

#[derive(Deserialize)]
struct GridComponent {
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Default, Deserialize)]
struct Disposition {
    #[serde(default)]
    default: u8,
    #[serde(default)]
    attached_pic: u8,
}

pub(crate) fn video(file: &mut File, path: &Path) -> Option<Probe> {
    read(file, video_demuxer(path), VIDEO_FIELDS, &[])
}

pub(crate) fn photo_dimensions(file: &mut File) -> Option<(u32, u32)> {
    let probe = read(file, "mov", PHOTO_FIELDS, &["-nofind_stream_info"])?;
    if let Some(grid) = probe
        .stream_groups
        .iter()
        .find(|group| group.kind.as_deref() == Some("Tile Grid") && group.disposition.default != 0)
    {
        let component = grid.components.first()?;
        return component.width.zip(component.height);
    }
    let primary = probe.streams.iter().find(|stream| {
        stream.codec_type.as_deref() == Some("video") && stream.disposition.default != 0
    })?;
    primary.width.zip(primary.height)
}

fn read(file: &mut File, demuxer: &str, fields: &str, args: &[&str]) -> Option<Probe> {
    file.rewind().ok()?;
    let mut command = media_command("ffprobe").ok()?;
    command.args(["-protocol_whitelist", "fd", "-f", demuxer]);
    if demuxer == "mov" {
        command.args(["-enable_drefs", "0"]);
    }
    // Inherit the checked file handle. fd: supports reading a trailing container index.
    let mut child = command
        .args([
            "-threads",
            "2",
            "-probesize",
            "4194304",
            "-analyzeduration",
            "2000000",
        ])
        .args(args)
        .args(["-show_entries", fields, "-of", "json", "-i", "fd:"])
        .stdin(Stdio::from(file.try_clone().ok()?))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take().expect("FFprobe stdout is piped");
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        stdout
            .take(MAX_OUTPUT_BYTES as u64 + 1)
            .read_to_end(&mut output)?;
        Ok::<_, std::io::Error>(output)
    });
    let status = wait(&mut child, None, PROBE_TIMEOUT);
    let bytes = reader.join().ok()?.ok()?;
    if !status.ok()?.success() || bytes.len() > MAX_OUTPUT_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}
