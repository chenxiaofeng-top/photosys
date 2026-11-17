use std::fs;
use std::path::Path;
use std::time::Instant;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::{FilterType, brighten, resize, rotate270};
use image::{ExtendedColorType, ImageEncoder, ImageFormat, Rgb, RgbImage};
use tempfile::tempdir;

use super::matching::{CaptureTime, RGB_LEN};
use super::*;

fn scene() -> RgbImage {
    RgbImage::from_fn(288, 192, |x, y| {
        let hill = 105 + (x * x / 690) % 35;
        let detail = ((x * 717 + y * 239) % 23) as u8;
        if (x.abs_diff(212)).pow(2) + (y.abs_diff(45)).pow(2) < 350 {
            Rgb([238, 209, 69 + detail])
        } else if y < hill {
            Rgb([45 + (y / 3) as u8, 98 + detail, 170 + detail])
        } else {
            Rgb([51 + detail, 117 - (y / 5) as u8 + detail, 41 + detail])
        }
    })
}

fn jpeg(path: &Path, image: &RgbImage, quality: u8, exif: Vec<u8>) {
    let mut out = JpegEncoder::new_with_quality(fs::File::create(path).unwrap(), quality);
    out.set_exif_metadata(exif).unwrap();
    out.write_image(
        image,
        image.width(),
        image.height(),
        ExtendedColorType::Rgb8,
    )
    .unwrap();
}

fn exif(orientation: u16, time: &[u8; 20], offset: &[u8; 7]) -> Vec<u8> {
    fn tag(data: &mut Vec<u8>, code: u16, format: u16, count: u32, value: u32) {
        data.extend(code.to_le_bytes());
        data.extend(format.to_le_bytes());
        data.extend(count.to_le_bytes());
        data.extend(value.to_le_bytes());
    }

    let mut data = b"II*\0\x08\0\0\0".to_vec();
    data.extend(2_u16.to_le_bytes());
    tag(&mut data, 0x0112, 3, 1, u32::from(orientation));
    tag(&mut data, 0x8769, 4, 1, 38);
    data.extend(0_u32.to_le_bytes());
    data.extend(2_u16.to_le_bytes());
    tag(&mut data, 0x9003, 2, 20, 68);
    tag(&mut data, 0x9011, 2, 7, 88);
    data.extend(0_u32.to_le_bytes());
    data.extend(time);
    data.extend(offset);
    data
}

fn scan_directory(directory: &Path, database: &Path) -> (Scan, Summary) {
    let ControlFlow::Continue(result) = run_scan(
        1,
        Root::new(directory).unwrap(),
        Ok(database),
        &AtomicBool::new(false),
        |_| {},
    ) else {
        panic!("unexpected cancellation");
    };
    result
}

#[test]
fn scans_variants_applies_exif_and_reuses_cache() {
    let directory = tempdir().unwrap();
    let photos = directory.path().join("photos");
    let child = photos.join("nested");
    fs::create_dir_all(&child).unwrap();
    let database = directory.path().join("index.sqlite3");
    let image = scene();

    image.save(photos.join("a.PNG")).unwrap();
    jpeg(&child.join("compressed.jpeg"), &image, 37, Vec::new());
    let small = resize(&image, 144, 96, FilterType::Triangle);
    brighten(&small, 5).save(child.join("small.webp")).unwrap();
    let rotated = child.join("rotated.jpg");
    jpeg(
        &rotated,
        &rotate270(&image),
        88,
        exif(6, b"2025:06:03 14:30:20\0", b"+08:00\0"),
    );
    image
        .save_with_format(child.join("different.tiff"), ImageFormat::Tiff)
        .unwrap();
    fs::write(child.join("phone.HEIC"), "unsupported").unwrap();
    fs::write(photos.join("broken.jpg"), [0xff, 0xd8, 0xff]).unwrap();

    let (scan, first) = scan_directory(&photos, &database);
    assert_eq!(
        (
            first.checked,
            first.reused,
            first.unsupported,
            first.skipped,
            first.groups
        ),
        (5, 0, 1, 2, 1)
    );
    assert!(first.cache_warning.is_none());
    let groups = scan.groups(Some(MatchKind::Variant), 0);
    assert_eq!(groups.items[0].count, 5);
    assert_eq!(groups.items[0].representative.name, "a.PNG");
    assert_eq!(scan.groups(Some(MatchKind::Burst), 0).total, 0);
    let members = scan.members(groups.items[0].id, 0).unwrap();
    let photo = members
        .items
        .iter()
        .find(|member| member.photo.name == "rotated.jpg")
        .unwrap();
    assert_eq!((photo.photo.width, photo.photo.height), (288, 192));
    assert_eq!(
        photo.photo.captured_at.as_deref(),
        Some("2025-06-03T14:30:20+08:00")
    );
    assert_eq!(photo.kind, Some(MatchKind::Variant));
    let candidate = &scan.photos[photo.photo.id].file;
    assert!(
        preview(&scan.root, candidate, false)
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
    fs::rename(&rotated, child.join("old.bin")).unwrap();
    fs::copy(child.join("old.bin"), &rotated).unwrap();
    assert_eq!(
        preview(&scan.root, candidate, true).unwrap_err(),
        "照片已变化或已移除，请重新扫描。"
    );

    let (_, second) = scan_directory(&photos, &database);
    assert_eq!((second.checked, second.reused, second.groups), (5, 4, 1));
    fs::remove_file(child.join("small.webp")).unwrap();
    let (_, third) = scan_directory(&photos, &database);
    assert_eq!((third.checked, third.reused), (4, 4));
    let rows: u32 = rusqlite::Connection::open(&database)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM photo_features", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 4);
}

fn features(hash: u64, color: u8, captured: Option<(i64, bool)>) -> Features {
    Features {
        width: 300,
        height: 200,
        hash,
        rgb: Box::new([color; RGB_LEN]),
        captured: captured.map(|(millis, has_offset)| CaptureTime {
            label: String::new(),
            millis,
            has_offset,
        }),
    }
}

#[test]
fn burst_uses_time_and_visual_distance_and_prefers_variants() {
    let items = [
        features(0, 61, Some((0, true))),
        features(0x1fff, 81, Some((5_000, true))),
        features(0x1fff, 165, Some((4_000, true))),
        features(0x3fff, 99, Some((5_001, true))),
        features(0x3fff, 61, Some((4_000, false))),
        features(0x03, 65, None),
        features(0x1fff, 65, None),
    ];
    let refs: Vec<_> = items.iter().collect();
    let groups = matching::group(&refs, |_| ControlFlow::Continue(()))
        .continue_value()
        .unwrap();
    let group = &groups[0];
    assert_eq!(group.anchor, 0);
    assert_eq!(
        group.members,
        vec![(1, MatchKind::Burst), (5, MatchKind::Variant)]
    );
    assert!(group.has_burst && group.has_variant);
    assert!(
        groups
            .iter()
            .all(|group| group.members.iter().all(|(id, _)| *id != 2))
    );
}

#[test]
fn uses_capture_time_from_decoded_photos_for_bursts() {
    let directory = tempdir().unwrap();
    let photos = directory.path().join("photos");
    fs::create_dir(&photos).unwrap();
    let image = scene();
    let shifted = RgbImage::from_fn(image.width(), image.height(), |x, y| {
        *image.get_pixel(
            x.saturating_sub(13),
            y.saturating_add(4).min(image.height() - 1),
        )
    });
    jpeg(
        &photos.join("a.jpg"),
        &image,
        85,
        exif(1, b"2025:06:03 14:30:20\0", b"+08:00\0"),
    );
    jpeg(
        &photos.join("b.jpg"),
        &shifted,
        85,
        exif(1, b"2025:06:03 06:30:24\0", b"+00:00\0"),
    );
    let (scan, summary) = scan_directory(&photos, &directory.path().join("index.sqlite3"));
    assert_eq!(summary.checked, 2);
    let groups = scan.groups(Some(MatchKind::Burst), 0);
    assert_eq!(groups.total, 1);
    assert_eq!(
        scan.members(groups.items[0].id, 0).unwrap().items[1].kind,
        Some(MatchKind::Burst)
    );
}

#[test]
fn checks_full_hamming_radius_without_chaining() {
    let items = [
        features(0, 0, None),
        features(0x0003_0003_0003_0003, 0, None),
        features(0x000f_000f_000f_000f, 0, None),
    ];
    let refs: Vec<_> = items.iter().collect();
    for _ in 0..2 {
        let groups = matching::group(&refs, |_| ControlFlow::Continue(()))
            .continue_value()
            .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].anchor, 0);
        assert_eq!(groups[0].members, vec![(1, MatchKind::Variant)]);
    }
}

#[test]
fn cancels_after_a_committed_batch_and_falls_back_without_cache() {
    let directory = tempdir().unwrap();
    let photos = directory.path().join("photos");
    fs::create_dir(&photos).unwrap();
    let database = directory.path().join("cached").join("index.sqlite");
    let photo = photos.join("000.png");
    resize(&scene(), 24, 16, FilterType::Triangle)
        .save(&photo)
        .unwrap();
    for index in 1..=(CACHE_BATCH + 1) {
        fs::copy(&photo, photos.join(format!("{index:03}.png"))).unwrap();
    }
    let cancelled = AtomicBool::new(false);
    let scan = run_scan(
        1,
        Root::new(&photos).unwrap(),
        Ok(&database),
        &cancelled,
        |progress| {
            if matches!(progress, Progress::Matching { .. }) {
                cancelled.store(true, Ordering::Relaxed);
            }
        },
    );
    assert!(matches!(scan, ControlFlow::Break(())));
    let (scan, resumed) = scan_directory(&photos, &database);
    assert_eq!(resumed.reused, CACHE_BATCH + 2);
    let group = scan.groups(None, 0).items.remove(0);
    assert_eq!(scan.members(group.id, 0).unwrap().items.len(), PAGE_SIZE);
    let last = scan
        .members(group.id, (group.count - 1) / PAGE_SIZE)
        .unwrap();
    assert_eq!(last.items.len(), group.count % PAGE_SIZE);
    assert!(scan.members(group.id, usize::MAX).unwrap().items.is_empty());

    let invalid_database = directory.path().join("regular-file");
    fs::write(&invalid_database, "file").unwrap();
    let (_, fallback) = scan_directory(&photos, &invalid_database.join("index.sqlite"));
    assert_eq!((fallback.reused, fallback.checked), (0, CACHE_BATCH + 2));
    assert!(fallback.cache_warning.is_some());
}

#[test]
#[ignore = "100,000-photo scalability check"]
fn groups_one_hundred_thousand_and_pages_results() {
    let timer = Instant::now();
    let directory = tempdir().unwrap();
    let seed = directory.path().join("stamp");
    fs::write(&seed, "seed").unwrap();
    let file = fs::File::open(seed).unwrap();
    let items: Vec<_> = (0..100_000_u64)
        .map(|index| {
            let mut hash = (index / 2).wrapping_add(0x9e37_79b9_7f4a_7c15);
            hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            features(
                hash ^ (hash >> 31),
                (index / 2 % 256) as u8,
                Some((index as i64 * 414, false)),
            )
        })
        .collect();
    let refs: Vec<_> = items.iter().collect();
    let groups = matching::group(&refs, |_| ControlFlow::Continue(()))
        .continue_value()
        .unwrap();
    let photos = (0..items.len())
        .map(|id| Photo {
            file: Candidate {
                id,
                relative: format!("{id}.jpg").into(),
                stamp: FileStamp::read(&file).unwrap(),
            },
            width: 300,
            height: 200,
            captured_at: None,
        })
        .collect();
    let scan = Scan {
        id: 1,
        root: Root::new(directory.path()).unwrap(),
        photos,
        groups,
        issues: Vec::new(),
    };
    let first = scan.groups(None, 0);
    let count = first.total;
    assert!(count > 49_000);
    assert_eq!(first.items.len(), PAGE_SIZE);
    assert_eq!(
        scan.groups(None, (count - 1) / PAGE_SIZE)
            .items
            .last()
            .unwrap()
            .id,
        count - 1
    );
    assert!(
        scan.groups(None, count.div_ceil(PAGE_SIZE))
            .items
            .is_empty()
    );
    eprintln!(
        "100,000 feature records: {count} groups in {:.3}s",
        timer.elapsed().as_secs_f64()
    );
}

#[test]
#[ignore = "run with PHOTOSYS_SAMPLE_DIR pointing to a real photo folder"]
fn benchmark_real_photo_folder() {
    let directory = std::env::var_os("PHOTOSYS_SAMPLE_DIR").expect("set PHOTOSYS_SAMPLE_DIR");
    let directory = Path::new(&directory);
    let cache = tempdir().unwrap();
    let database = cache.path().join("index.sqlite");
    let first_start = Instant::now();
    let (_, first) = scan_directory(directory, &database);
    let first_time = first_start.elapsed();
    let second_start = Instant::now();
    let (_, second) = scan_directory(directory, &database);
    assert!(first.checked > 0);
    assert_eq!(second.reused, first.checked);
    assert_eq!(second.groups, first.groups);
    eprintln!(
        "{} real photos, {} groups, {} skipped: cold {:.3}s, warm {:.3}s, {} cache hits",
        first.checked,
        first.groups,
        first.skipped,
        first_time.as_secs_f64(),
        second_start.elapsed().as_secs_f64(),
        second.reused
    );
}
