use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use zip::ZipArchive;

use super::{Scan, previews::CHANGED};
use crate::media_codec::{self, CONVERSION_LIMIT};
use crate::media_date::LIVE_MEMBER_LIMIT;
use crate::similar::{encode_preview, supports_preview};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Member {
    Image,
    Heif,
    Video,
}

impl Member {
    fn from_name(name: &str) -> Option<Self> {
        let mut components = name.rsplit(['/', '\\']);
        let filename = components.next()?;
        if filename.is_empty()
            || filename.starts_with('.')
            || components.any(|component| {
                component.starts_with('.') || component.eq_ignore_ascii_case("__MACOSX")
            })
        {
            return None;
        }
        let path = Path::new(filename);
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "mov" | "mp4" => Some(Self::Video),
            _ if media_codec::is_heif(path) => Some(Self::Heif),
            _ if supports_preview(path) => Some(Self::Image),
            _ => None,
        }
    }
}

struct ExtractedMember {
    directory: TempDir,
    path: PathBuf,
    kind: Member,
}

fn extract(
    file: &mut File,
    accepts: impl Fn(Member) -> bool,
    missing: &str,
) -> Result<ExtractedMember, String> {
    let mut archive = ZipArchive::new(file).map_err(|_| "实况照片包损坏或无法读取。")?;
    // Match names and types without decompressing unrelated entries.
    let (index, kind) = (0..archive.len())
        .find_map(|index| {
            let entry = archive.by_index_raw(index).ok()?;
            let kind = Member::from_name(entry.name())?;
            (entry.is_file() && accepts(kind)).then_some((index, kind))
        })
        .ok_or(missing)?;

    let entry = archive
        .by_index(index)
        .map_err(|error| format!("无法读取实况照片包内的文件。{error}"))?;
    if entry.size() > LIVE_MEMBER_LIMIT {
        return Err("实况照片包内的文件超过 128 MiB。".into());
    }
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    // Always use our own filename. ZIP member names never become filesystem paths.
    let path = directory.path().join("input");
    let expected_size = entry.size();
    let mut output = File::create(&path).map_err(|error| error.to_string())?;
    let size = io::copy(&mut entry.take(LIVE_MEMBER_LIMIT + 1), &mut output)
        .map_err(|error| format!("无法解压实况照片包内的文件。{error}"))?;
    if size > LIVE_MEMBER_LIMIT {
        return Err("实况照片包内的文件超过 128 MiB。".into());
    }
    if size != expected_size {
        return Err("实况照片包内的文件不完整。".into());
    }
    Ok(ExtractedMember {
        directory,
        path,
        kind,
    })
}

pub(super) fn cover(file: &mut File, detail: bool) -> Result<String, String> {
    let member = extract(
        file,
        |kind| kind != Member::Video,
        "实况照片包内没有可预览的封面。",
    )?;
    let (mut image, size) = if member.kind == Member::Heif {
        media_codec::heif_jpeg(
            &member.path,
            &member.directory.path().join("cover.jpg"),
            None,
        )?
    } else {
        let file = File::open(&member.path).map_err(|error| error.to_string())?;
        let size = file.metadata().map_err(|error| error.to_string())?.len();
        (file, size)
    };
    encode_preview(&mut image, &member.path, size, detail)
}

fn webm(member: &ExtractedMember) -> Result<Vec<u8>, String> {
    let (file, size) = media_codec::convert(
        media_codec::Input::Mov(&member.path),
        &member.directory.path().join("live.webm"),
        &[
            "-map",
            "0:V:0",
            "-map",
            "0:a:0?",
            "-vf",
            "scale=w='max(2,min(iw,960))':h='max(2,min(ih,960))':force_original_aspect_ratio=decrease:force_divisible_by=2",
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libvpx",
            "-deadline",
            "realtime",
            "-cpu-used",
            "4",
            "-b:v",
            "2M",
            "-c:a",
            "libvorbis",
            "-ac",
            "2",
            "-b:a",
            "96k",
            "-f",
            "webm",
        ],
        None,
    )?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(CONVERSION_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

impl Scan {
    pub(super) fn live_video(&self, file_id: usize) -> Result<Vec<u8>, String> {
        let item = self.item(file_id)?;
        if !item.is_live_photo {
            return Err("此文件没有实况视频。".into());
        }
        let mut file = self.root.open_matching(&item.file).map_err(|_| CHANGED)?;
        let video = extract(
            &mut file,
            |kind| kind == Member::Video,
            "实况照片包内没有 MOV 或 MP4 视频。",
        )
        .and_then(|member| webm(&member));
        self.root
            .verify_read(&file, &item.file)
            .map_err(|_| CHANGED)?;
        video
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use base64::{Engine, engine::general_purpose::STANDARD};
    use image::{DynamicImage, ImageFormat, RgbImage};
    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::duplicates::Root;
    use crate::library::Kind;

    fn write_package(path: &Path, members: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        zip.add_symlink("link.jpg", "unrelated.jpg", SimpleFileOptions::default())
            .unwrap();
        for &(name, bytes) in members {
            zip.start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn shows_a_live_package_as_one_photo_with_a_cached_cover() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("IMG_2026-02-03.LIVP");
        let mut image = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::new(600, 300))
            .write_to(&mut image, ImageFormat::Jpeg)
            .unwrap();
        write_package(
            &path,
            &[
                ("__MACOSX/._cover.jpg", b"metadata"),
                ("__MACOSX/ignored.jpg", b"metadata"),
                ("photo/COVER.JPG", image.get_ref()),
            ],
        );
        let scan = Scan::read(1, Root::new(directory.path()).unwrap(), |_| {});
        assert_eq!(scan.items.len(), 1);
        let item = scan.item(0).unwrap().view(false, 0);
        assert_eq!(item.kind, Kind::Photo);
        assert_eq!(item.captured_on.as_deref(), Some("2026-02-03"));
        assert!(item.is_live_photo && item.preview_available);
        let sizes = [(false, (240, 120)), (true, (600, 300))];
        for (detail, dimensions) in sizes {
            let source = scan.preview(0, detail).unwrap();
            let bytes = STANDARD
                .decode(source.strip_prefix("data:image/jpeg;base64,").unwrap())
                .unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap();
            assert_eq!((decoded.width(), decoded.height()), dimensions);
        }
        assert!(scan.live_video(0).unwrap_err().contains("没有 MOV 或 MP4"));

        write_package(&path, &[("different.jpg", b"changed")]);
        assert_eq!(scan.preview(0, false).unwrap_err(), CHANGED);
        assert_eq!(scan.live_video(0).unwrap_err(), CHANGED);
    }

    #[test]
    fn reports_missing_covers_and_corrupt_live_packages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live.livp");
        write_package(&path, &[("nested\\CLIP.MP4", b"video")]);
        let extracted = extract(
            &mut File::open(&path).unwrap(),
            |kind| kind == Member::Video,
            "missing video",
        )
        .unwrap();
        assert_eq!(std::fs::read(&extracted.path).unwrap(), b"video");
        assert_eq!(extracted.path.file_name().unwrap(), "input");
        assert!(
            cover(&mut File::open(&path).unwrap(), false)
                .unwrap_err()
                .contains("没有可预览的封面")
        );

        std::fs::write(&path, b"broken ZIP").unwrap();
        assert!(
            cover(&mut File::open(&path).unwrap(), true)
                .unwrap_err()
                .contains("包损坏")
        );
    }

    #[test]
    fn rejects_oversized_live_members_before_decompressing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.livp");
        write_package(&path, &[("cover.jpg", b"small")]);
        let mut contents = std::fs::read(&path).unwrap();
        let name = contents
            .windows(b"cover.jpg".len())
            .rposition(|value| value == b"cover.jpg")
            .unwrap();
        // Patch the uncompressed size in the central-directory entry.
        let header = name - 46;
        assert_eq!(&contents[header..header + 4], b"PK\x01\x02");
        let size = (LIVE_MEMBER_LIMIT as u32 + 1).to_le_bytes();
        contents[header + 24..header + 28].copy_from_slice(&size);
        std::fs::write(&path, contents).unwrap();
        assert!(
            cover(&mut File::open(&path).unwrap(), false)
                .unwrap_err()
                .contains("超过 128 MiB")
        );
    }
}
