use std::collections::VecDeque;

use super::{STATE_ERROR, Scan};
use crate::similar::encode_preview;

const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 24 * 1024 * 1024;
const CHANGED: &str = "照片已变化或已移除，请重新扫描。";

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
        let image = encode_preview(&mut file, item.file.stamp.size, detail);
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

    use base64::{Engine, engine::general_purpose::STANDARD};
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

    fn decode(source: &str) -> RgbImage {
        let jpeg = STANDARD
            .decode(source.strip_prefix("data:image/jpeg;base64,").unwrap())
            .unwrap();
        image::load_from_memory(&jpeg).unwrap().into_rgb8()
    }
}
