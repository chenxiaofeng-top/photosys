use std::collections::VecDeque;

use base64::{Engine, engine::general_purpose::STANDARD};

use super::{Kind, STATE_ERROR, Scan, live};
use crate::{media_codec, similar::encode_preview};

const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 24 * 1024 * 1024;
pub(super) const CHANGED: &str = "媒体文件已变化或已移除，请重新扫描。";

type Key = (usize, bool);

#[derive(Default)]
pub(super) struct Cache {
    entries: VecDeque<(Key, String)>,
    bytes: usize,
}

impl Cache {
    fn get(&mut self, key: Key) -> Option<String> {
        let index = self
            .entries
            .iter()
            .position(|(existing, _)| *existing == key)?;
        let entry = self.entries.remove(index)?;
        let image = entry.1.clone();
        self.entries.push_back(entry);
        Some(image)
    }

    fn insert(&mut self, key: Key, image: &str) {
        if image.len() > MAX_BYTES {
            return;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|(existing, _)| *existing == key)
            && let Some((_, previous)) = self.entries.remove(index)
        {
            self.bytes -= previous.len();
        }
        while self.entries.len() >= MAX_ENTRIES || self.bytes + image.len() > MAX_BYTES {
            let (_, oldest) = self.entries.pop_front().expect("cache is over capacity");
            self.bytes -= oldest.len();
        }
        self.entries.push_back((key, image.to_owned()));
        self.bytes += image.len();
    }
}

impl Scan {
    pub(super) fn preview(&self, file_id: usize, detail: bool) -> Result<String, String> {
        let item = self.item(file_id)?;
        if !item.preview_available {
            return Err("此文件可用系统默认应用打开。".into());
        }
        let mut file = self.root.open_matching(&item.file).map_err(|_| CHANGED)?;
        let key = (file_id, detail);
        if let Some(image) = self.previews.lock().map_err(|_| STATE_ERROR)?.get(key) {
            return Ok(image);
        }
        let image = if item.is_live_photo {
            live::cover(&mut file, detail)
        } else if item.kind == Kind::Video {
            let edge = if detail { 1600 } else { 240 };
            media_codec::video_jpeg(&file, &item.file.relative, edge)
                .map(|bytes| format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes)))
        } else {
            encode_preview(&mut file, &item.file.relative, item.file.stamp.size, detail)
        };
        self.root
            .verify_read(&file, &item.file)
            .map_err(|_| CHANGED)?;
        let image = image?;
        self.previews
            .lock()
            .map_err(|_| STATE_ERROR)?
            .insert(key, &image);
        Ok(image)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::Write;

    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageEncoder, RgbImage, RgbaImage};
    use tempfile::tempdir;

    use super::*;
    use crate::duplicates::Root;

    #[test]
    fn evicts_least_recently_used_entries_within_both_limits() {
        let mut cache = Cache::default();
        for id in 0..MAX_ENTRIES {
            cache.insert((id, false), "thumbnail");
        }
        assert_eq!(cache.get((0, false)).as_deref(), Some("thumbnail"));
        cache.insert((0, true), "detail");
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(cache.get((1, false)).is_none());
        assert!(cache.get((0, false)).is_some());

        let large = "x".repeat(MAX_BYTES / 2 + 1);
        cache.insert((0, true), &large);
        cache.insert((1, true), &large);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.bytes, large.len());
        assert!(cache.get((1, true)).is_some());
    }

    #[test]
    fn checks_the_file_before_reusing_a_cached_preview() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("image.png");
        RgbImage::new(64, 32).save(&path).unwrap();
        let scan = Scan::read(1, Root::new(directory.path()).unwrap(), |_| {});
        scan.previews.lock().unwrap().insert((0, false), "cached");
        assert_eq!(scan.preview(0, false).unwrap(), "cached");
        assert_ne!(scan.preview(0, true).unwrap(), "cached");

        RgbImage::new(32, 64).save(&path).unwrap();
        assert_eq!(scan.preview(0, false).unwrap_err(), CHANGED);
        let refreshed = Scan::read(2, Root::new(directory.path()).unwrap(), |_| {});
        assert_eq!(
            decode(&refreshed.preview(0, false).unwrap()).dimensions(),
            (32, 64)
        );

        fs::remove_file(&path).unwrap();
        assert_eq!(refreshed.preview(0, false).unwrap_err(), CHANGED);
    }

    #[test]
    fn preserves_preview_sizes_orientation_and_alpha_compositing() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("rotated.jpg");
        let mut encoder = JpegEncoder::new(File::create(path).unwrap());
        encoder
            .set_exif_metadata(
                b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec(),
            )
            .unwrap();
        encoder.encode_image(&RgbImage::new(1800, 600)).unwrap();
        RgbaImage::new(4, 2)
            .save(directory.path().join("transparent.png"))
            .unwrap();
        let scan = Scan::read(1, Root::new(directory.path()).unwrap(), |_| {});

        let thumbnail = decode(&scan.preview(0, false).unwrap());
        let detail = decode(&scan.preview(0, true).unwrap());
        assert_eq!(thumbnail.dimensions(), (80, 240));
        assert_eq!(detail.dimensions(), (533, 1600));
        let transparent = decode(&scan.preview(1, false).unwrap());
        assert!(transparent.pixels().all(|pixel| pixel.0 == [255, 255, 255]));
    }

    #[test]
    fn previews_videos_with_correct_sizes_colors_and_rotation() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let mp4 = include_bytes!("../../tests/fixtures/video-preview.mp4");
        let fixtures: [(&str, &[u8], (u32, u32)); 6] = [
            ("clip.mp4", mp4, (240, 135)),
            ("clip.M4V", mp4, (240, 135)),
            (
                "portrait.mov",
                include_bytes!("../../tests/fixtures/video-rotated.mov"),
                (80, 240),
            ),
            (
                "clip.mkv",
                include_bytes!("../../tests/fixtures/video-preview.mkv"),
                (240, 135),
            ),
            (
                "clip.webm",
                include_bytes!("../../tests/fixtures/video-preview.webm"),
                (128, 72),
            ),
            (
                "clip.3gp",
                include_bytes!("../../tests/fixtures/video-preview.3gp"),
                (176, 144),
            ),
        ];
        for (name, bytes, _) in fixtures {
            fs::write(root.join(name), bytes).unwrap();
        }

        // Put the trailing index far enough from the first frame that previewing requires seeking.
        let index = mp4.windows(4).position(|bytes| bytes == b"moov").unwrap() - 4;
        let pad_size = 128 * 1024_u32;
        let mut indexed = File::create(root.join("trailing-index.mp4")).unwrap();
        indexed.write_all(&mp4[..index]).unwrap();
        indexed.write_all(&pad_size.to_be_bytes()).unwrap();
        indexed.write_all(b"free").unwrap();
        indexed.write_all(&vec![0; pad_size as usize - 8]).unwrap();
        indexed.write_all(&mp4[index..]).unwrap();
        drop(indexed);

        let scan = Scan::read(1, Root::new(root).unwrap(), |_| {});
        for (name, dimensions) in fixtures
            .map(|(name, _, dimensions)| (name, dimensions))
            .into_iter()
            .chain([("trailing-index.mp4", (240, 135))])
        {
            let item = scan
                .items
                .iter()
                .find(|item| item.file.relative == std::path::Path::new(name))
                .unwrap();
            assert_eq!(item.kind, Kind::Video);
            assert!(item.preview_available);
            let id = item.file.id;
            let source = scan.preview(id, false).unwrap();
            let image = decode(&source);
            assert_eq!(image.dimensions(), dimensions, "{name}");
            assert_eq!(scan.previews.lock().unwrap().get((id, false)), Some(source));
            if name == "portrait.mov" {
                let detail = decode(&scan.preview(id, true).unwrap());
                assert_eq!(detail.dimensions(), (533, 1600));
                let [r, _, b] = detail.get_pixel(200, 200).0;
                assert!(u16::from(b) > u16::from(r) + 50);
                let [r, _, b] = detail.get_pixel(200, 1400).0;
                assert!(u16::from(r) > u16::from(b) + 50);
            } else if name == "trailing-index.mp4" {
                let [r, g, b] = image.get_pixel(120, 60).0;
                assert!(r > 200 && g < 30 && b < 30);
                fs::remove_file(root.join(name)).unwrap();
                fs::write(root.join(name), b"replaced").unwrap();
                assert_eq!(scan.preview(id, false).unwrap_err(), CHANGED);
            }
        }
    }

    #[test]
    fn rejects_corrupt_and_audio_only_video_previews() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("broken.mp4"), b"invalid MP4").unwrap();
        fs::write(
            root.join("audio-only.mp4"),
            include_bytes!("../../tests/fixtures/video-audio-only.mp4"),
        )
        .unwrap();

        let scan = Scan::read(1, Root::new(root).unwrap(), |_| {});
        assert_eq!(scan.items.len(), 2);
        for item in &scan.items {
            assert!(item.preview_available);
            assert!(
                scan.preview(item.file.id, false)
                    .unwrap_err()
                    .contains("无法提取视频画面")
            );
        }
    }

    #[test]
    fn previews_heif_grid_photos_and_live_covers() {
        let fixture = include_bytes!("../../tests/fixtures/portrait-grid.heic");
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("photo.HEIC"), fixture).unwrap();
        fs::write(root.join("photo.heif"), fixture).unwrap();
        fs::write(root.join("broken.heic"), b"invalid image").unwrap();
        let mut package = zip::ZipWriter::new(File::create(root.join("live.livp")).unwrap());
        package
            .start_file("cover.HEIC", zip::write::SimpleFileOptions::default())
            .unwrap();
        package.write_all(fixture).unwrap();
        package.finish().unwrap();

        let scan = Scan::read(1, Root::new(root).unwrap(), |_| {});
        assert_eq!(scan.items.len(), 4);
        for item in &scan.items {
            assert!(item.preview_available);
            if item.file.relative == std::path::Path::new("broken.heic") {
                assert!(
                    scan.preview(item.file.id, false)
                        .unwrap_err()
                        .contains("HEIC/HEIF")
                );
                continue;
            }
            for (detail, dimensions) in [(false, (120, 240)), (true, (800, 1600))] {
                let image = decode(&scan.preview(item.file.id, detail).unwrap());
                assert_eq!(image.dimensions(), dimensions);
                let [r, _, b] = image.get_pixel(dimensions.0 / 4, dimensions.1 / 4).0;
                assert!(r > b + 50, "the red tile should be above the blue tile");
                let [r, _, b] = image.get_pixel(dimensions.0 / 4, dimensions.1 * 7 / 8).0;
                assert!(b > r + 50, "the blue tile should be below the red tile");
            }
        }
    }

    fn decode(source: &str) -> RgbImage {
        let jpeg = STANDARD
            .decode(source.strip_prefix("data:image/jpeg;base64,").unwrap())
            .unwrap();
        image::load_from_memory(&jpeg).unwrap().into_rgb8()
    }
}
