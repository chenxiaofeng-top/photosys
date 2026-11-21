use std::fs;
use std::path::Path;
use std::time::Instant;

use base64::{Engine, engine::general_purpose::STANDARD};
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
    fs::write(child.join("raw.DNG"), "unsupported").unwrap();
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
        preview(&scan.root, candidate, PreviewSize::Thumbnail)
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
    fs::rename(&rotated, child.join("old.bin")).unwrap();
    fs::copy(child.join("old.bin"), &rotated).unwrap();
    assert_eq!(
        preview(&scan.root, candidate, PreviewSize::Detail).unwrap_err(),
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

#[test]
fn comparison_viewer_renders_a_larger_oriented_preview_and_reads_checked_metadata() {
    let directory = tempdir().unwrap();
    let photos = directory.path().join("photos");
    fs::create_dir(&photos).unwrap();
    let rotated = photos.join("rotated.jpg");
    let plain = photos.join("plain.png");
    jpeg(
        &rotated,
        &resize(&scene(), 4000, 256, FilterType::Nearest),
        88,
        exif(6, b"2025:06:03 14:30:20\0", b"+08:00\0"),
    );
    scene().save(&plain).unwrap();
    let (scan, _) = scan_directory(&photos, &directory.path().join("index.sqlite3"));
    let candidate = &scan
        .photos
        .iter()
        .find(|photo| photo.view().name == "rotated.jpg")
        .unwrap()
        .file;
    let plain_candidate = &scan
        .photos
        .iter()
        .find(|photo| photo.view().name == "plain.png")
        .unwrap()
        .file;

    for (size, dimensions) in [
        (PreviewSize::Thumbnail, (15, 240)),
        (PreviewSize::Detail, (102, 1600)),
        (PreviewSize::Large, (205, 3200)),
    ] {
        let data = preview(&scan.root, candidate, size).unwrap();
        let bytes = STANDARD.decode(data.split_once(',').unwrap().1).unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), dimensions);
    }

    let details = serde_json::to_value(scan.media_details(candidate.id).unwrap()).unwrap();
    assert_eq!(details["photo"]["dimensions"]["width"], 4000);
    assert_eq!(details["photo"]["capturedAt"], "2025-06-03T14:30:20+08:00");
    let plain_details =
        serde_json::to_value(scan.media_details(plain_candidate.id).unwrap()).unwrap();
    assert_eq!(plain_details["photo"]["dimensions"]["width"], 288);
    assert!(plain_details["photo"]["capturedAt"].is_null());

    let moved = photos.join("original.bin");
    fs::rename(&rotated, &moved).unwrap();
    assert!(scan.media_details(candidate.id).is_err());
    fs::copy(&moved, &rotated).unwrap();
    assert!(preview(&scan.root, candidate, PreviewSize::Large).is_err());
    assert!(scan.media_details(candidate.id).is_err());
}

#[test]
fn matches_heic_with_jpeg_and_reuses_features() {
    let directory = tempdir().unwrap();
    let photos = directory.path().join("photos");
    fs::create_dir(&photos).unwrap();
    let database = directory.path().join("index.sqlite3");
    let heic = photos.join("portrait.HEIC");
    fs::write(
        &heic,
        include_bytes!("../../tests/fixtures/portrait-grid.heic"),
    )
    .unwrap();
    fs::write(
        photos.join("portrait.jpg"),
        include_bytes!("../../tests/fixtures/portrait-reference.jpg"),
    )
    .unwrap();
    fs::write(photos.join("broken.heif"), b"invalid image").unwrap();

    let (scan, summary) = scan_directory(&photos, &database);
    assert_eq!(
        (
            summary.checked,
            summary.reused,
            summary.unsupported,
            summary.skipped
        ),
        (2, 0, 0, 1)
    );
    assert_eq!(scan.issues[0].path, "broken.heif");
    assert!(scan.issues[0].reason.contains("HEIC/HEIF"));
    let groups = scan.groups(Some(MatchKind::Variant), 0);
    assert_eq!(groups.total, 1);
    let members = scan.members(groups.items[0].id, 0).unwrap();
    assert_eq!(members.total, 2);
    for member in &members.items {
        let photo = &member.photo;
        assert_eq!((photo.width, photo.height), (1024, 2048));
        if photo.name == "portrait.HEIC" {
            assert_eq!(
                photo.captured_at.as_deref(),
                Some("2025-06-03T14:30:20+08:00")
            );
        }
        assert!(
            preview(&scan.root, &scan.photos[photo.id].file, PreviewSize::Detail)
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
    }

    let (_, second) = scan_directory(&photos, &database);
    assert_eq!((second.checked, second.reused, second.groups), (2, 2, 1));
    let candidate = &scan
        .photos
        .iter()
        .find(|photo| photo.file.relative == Path::new("portrait.HEIC"))
        .unwrap()
        .file;
    fs::write(heic, b"replacement").unwrap();
    assert_eq!(
        preview(&scan.root, candidate, PreviewSize::Thumbnail).unwrap_err(),
        "照片已变化或已移除，请重新扫描。"
    );
    let (_, replaced) = scan_directory(&photos, &database);
    assert_eq!(
        (replaced.checked, replaced.reused, replaced.skipped),
        (1, 1, 2)
    );
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
            features: features(0, 0, None),
            moved: false,
        })
        .collect();
    let scan = Scan {
        id: 1,
        root: Root::new(directory.path()).unwrap(),
        photos,
        groups: groups.into_iter().map(Some).collect(),
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

// Real files and stamps, with deterministic visual features; no system trash is
// touched by these tests.
fn trash_scan(directory: &Path, items: Vec<Features>) -> Scan {
    let refs: Vec<_> = items.iter().collect();
    let groups = matching::group(&refs, |_| ControlFlow::Continue(()))
        .continue_value()
        .unwrap();
    let photos = items
        .into_iter()
        .enumerate()
        .map(|(id, features)| {
            let relative = PathBuf::from(format!("photo-{id}.jpg"));
            fs::write(directory.join(&relative), format!("original photo {id}")).unwrap();
            let (_, file) = open_photo(&Root::new(directory).unwrap(), &relative, id).unwrap();
            Photo {
                file,
                features,
                moved: false,
            }
        })
        .collect();
    Scan {
        id: 17,
        root: Root::new(directory).unwrap(),
        photos,
        groups: groups.into_iter().map(Some).collect(),
        issues: Vec::new(),
    }
}

fn identical_trash_scan(directory: &Path, count: usize) -> Scan {
    trash_scan(
        directory,
        (0..count).map(|_| features(0, 61, None)).collect(),
    )
}

fn mock_trash(path: &Path) -> Result<(), String> {
    fs::remove_file(path).map_err(|error| error.to_string())
}

#[test]
fn trash_members_preserves_photo_ids_and_unrelated_groups() {
    let directory = tempdir().unwrap();
    let mut scan = trash_scan(
        directory.path(),
        vec![
            features(0, 61, None),
            features(0, 61, None),
            features(0, 61, None),
            features(0, 200, None),
            features(0, 200, None),
        ],
    );
    let result = scan.recycle(17, 0, vec![1], mock_trash).unwrap();
    assert_eq!(result.total_groups, 2);
    assert_eq!(result.outcomes[0].status, TrashStatus::Moved);
    assert_eq!(result.affected_groups[0].count, 2);
    assert_eq!(
        scan.members(0, 0)
            .unwrap()
            .items
            .iter()
            .map(|m| m.photo.id)
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    let unrelated = scan.groups(None, 0).items.remove(1);
    assert_eq!(
        (unrelated.id, unrelated.representative.id, unrelated.count),
        (1, 3, 2)
    );
    assert!(scan.photos[1].moved);
    assert!(preview(&scan.root, &scan.photos[1].file, PreviewSize::Thumbnail).is_err());
    assert!(scan.media_details(1).is_err());
    assert!(
        scan.recycle(17, 0, vec![1], |_| panic!("must not trash twice"))
            .is_err()
    );
}

#[test]
fn trash_rejects_invalid_selections_before_any_move() {
    let directory = tempdir().unwrap();
    let mut scan = trash_scan(
        directory.path(),
        vec![
            features(0, 61, None),
            features(0, 61, None),
            features(0, 200, None),
            features(0, 200, None),
        ],
    );
    for (scan_id, group_id, ids) in [
        (16, 0, vec![1]),
        (17, 99, vec![1]),
        (17, 0, vec![]),
        (17, 0, vec![99]),
        (17, 0, vec![1, 2]),
        (17, 0, vec![0, 1]),
    ] {
        assert!(
            scan.recycle(scan_id, group_id, ids, |_| panic!(
                "invalid request reached trash"
            ))
            .is_err()
        );
    }
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 4);
}

#[test]
fn trash_deduplicates_ids_and_hides_single_photo_groups() {
    let directory = tempdir().unwrap();
    let mut scan = identical_trash_scan(directory.path(), 2);
    let mut calls = 0;
    let result = scan
        .recycle(17, 0, vec![1, 1], |path| {
            calls += 1;
            mock_trash(path)
        })
        .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(result.outcomes.len(), 1);
    assert_eq!(result.total_groups, 0);
    assert!(result.affected_groups.is_empty());
    assert!(scan.members(0, 0).is_err());
    assert!(directory.path().join("photo-0.jpg").exists());
    assert!(!scan.photos[0].moved);
}

#[test]
fn trash_representative_rematches_and_splits_without_renumbering_other_groups() {
    let directory = tempdir().unwrap();
    let mut scan = trash_scan(
        directory.path(),
        vec![
            features(0, 61, None),
            features(0x00f, 61, None),
            features(0x00f, 61, None),
            features(0xff0, 61, None),
            features(0xff0, 61, None),
            features(0, 200, None),
            features(0, 200, None),
        ],
    );
    assert_eq!(scan.groups(None, 0).items[0].count, 5);
    let result = scan.recycle(17, 0, vec![0], mock_trash).unwrap();
    assert_eq!(result.total_groups, 3);
    assert_eq!(
        result
            .affected_groups
            .iter()
            .map(|g| (g.id, g.representative.id, g.count))
            .collect::<Vec<_>>(),
        vec![(0, 1, 2), (2, 3, 2)]
    );
    assert_eq!(scan.groups(None, 0).items[1].representative.id, 5);
    assert_eq!(scan.members(2, 0).unwrap().items[1].photo.id, 4);
}

#[test]
fn trash_representative_can_leave_only_unmatched_survivors() {
    let directory = tempdir().unwrap();
    let mut scan = trash_scan(
        directory.path(),
        vec![
            features(0, 61, None),
            features(0x00f, 61, None),
            features(0xff0, 61, None),
        ],
    );
    let result = scan.recycle(17, 0, vec![0], mock_trash).unwrap();
    assert_eq!(result.total_groups, 0);
    assert!(result.affected_groups.is_empty());
    assert!(directory.path().join("photo-1.jpg").exists());
    assert!(directory.path().join("photo-2.jpg").exists());
}

#[test]
fn trash_updates_variant_and_burst_filters() {
    let directory = tempdir().unwrap();
    let mut scan = trash_scan(
        directory.path(),
        vec![
            features(0, 61, Some((0, true))),
            features(0x1fff, 81, Some((5_000, true))),
            features(0x03, 65, None),
        ],
    );
    assert_eq!(scan.groups(Some(MatchKind::Burst), 0).total, 1);
    scan.recycle(17, 0, vec![1], mock_trash).unwrap();
    assert_eq!(scan.groups(Some(MatchKind::Burst), 0).total, 0);
    assert_eq!(scan.groups(Some(MatchKind::Variant), 0).total, 1);
}

#[test]
fn trash_reports_partial_failures_and_keeps_failed_members() {
    let directory = tempdir().unwrap();
    let mut scan = identical_trash_scan(directory.path(), 4);
    let result = scan
        .recycle(17, 0, vec![1, 2], |path| {
            if path.ends_with("photo-1.jpg") {
                Err("回收站不可用".into())
            } else {
                mock_trash(path)
            }
        })
        .unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Failed);
    assert_eq!(result.outcomes[0].reason.as_deref(), Some("回收站不可用"));
    assert_eq!(result.outcomes[1].status, TrashStatus::Moved);
    assert_eq!(result.affected_groups[0].count, 3);
    assert!(!scan.photos[1].moved);
    assert!(scan.photos[2].moved);
    let retried = scan.recycle(17, 0, vec![1], mock_trash).unwrap();
    assert_eq!(retried.outcomes[0].status, TrashStatus::Moved);
}

#[test]
fn trash_failure_preserves_representative_and_group() {
    let directory = tempdir().unwrap();
    let mut scan = identical_trash_scan(directory.path(), 3);
    let result = scan
        .recycle(17, 0, vec![0], |_| Err("拒绝访问".into()))
        .unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Failed);
    assert_eq!(result.affected_groups[0].representative.id, 0);
    assert_eq!(result.affected_groups[0].count, 3);
    assert!(!scan.photos[0].moved);
}

#[test]
fn trash_skips_missing_changed_and_replaced_targets() {
    for change in 0..3 {
        let directory = tempdir().unwrap();
        let mut scan = identical_trash_scan(directory.path(), 3);
        let target = directory.path().join("photo-1.jpg");
        match change {
            0 => fs::remove_file(&target).unwrap(),
            1 => fs::write(&target, b"changed size").unwrap(),
            _ => {
                fs::rename(&target, directory.path().join("original.bin")).unwrap();
                fs::copy(directory.path().join("original.bin"), &target).unwrap();
            }
        }
        let result = scan
            .recycle(17, 0, vec![1, 2], |path| {
                assert!(!path.ends_with("photo-1.jpg"));
                mock_trash(path)
            })
            .unwrap();
        assert_eq!(result.outcomes[0].status, TrashStatus::Skipped);
        assert!(
            result.outcomes[0]
                .reason
                .as_ref()
                .unwrap()
                .contains("重新扫描")
        );
        assert_eq!(result.outcomes[1].status, TrashStatus::Moved);
    }
}

#[test]
fn trash_revalidates_keeper_for_each_target() {
    let directory = tempdir().unwrap();
    let mut scan = identical_trash_scan(directory.path(), 3);
    let result = scan
        .recycle(17, 0, vec![1, 2], |path| {
            mock_trash(path)?;
            fs::write(directory.path().join("photo-0.jpg"), b"keeper changed").unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Moved);
    assert_eq!(result.outcomes[1].status, TrashStatus::Skipped);
    assert!(directory.path().join("photo-2.jpg").exists());
}

#[test]
fn trash_accepts_another_valid_unselected_keeper() {
    let directory = tempdir().unwrap();
    let mut scan = identical_trash_scan(directory.path(), 3);
    fs::remove_file(directory.path().join("photo-0.jpg")).unwrap();
    let result = scan.recycle(17, 0, vec![1], mock_trash).unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Moved);
    assert!(directory.path().join("photo-2.jpg").exists());
}

#[test]
fn trash_skips_when_no_unselected_keeper_is_valid() {
    let directory = tempdir().unwrap();
    let mut scan = identical_trash_scan(directory.path(), 2);
    fs::remove_file(directory.path().join("photo-0.jpg")).unwrap();
    let result = scan
        .recycle(17, 0, vec![1], |_| panic!("no surviving keeper"))
        .unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Skipped);
    assert!(
        result.outcomes[0]
            .reason
            .as_ref()
            .unwrap()
            .contains("保留照片")
    );
}

#[test]
fn trash_refuses_hard_links() {
    let directory = tempdir().unwrap();
    let mut scan = identical_trash_scan(directory.path(), 2);
    let original = directory.path().join("photo-1.jpg");
    fs::hard_link(&original, directory.path().join("linked.jpg")).unwrap();
    scan.photos[1].file.stamp = FileStamp::read(&fs::File::open(&original).unwrap()).unwrap();
    assert!(scan.members(0, 0).unwrap().items[1].photo.hard_link);
    assert!(
        scan.recycle(17, 0, vec![1], |_| panic!("hard link reached trash"))
            .is_err()
    );
}

#[test]
fn trash_skips_replaced_scan_root() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("photos");
    fs::create_dir(&root).unwrap();
    let mut scan = identical_trash_scan(&root, 2);
    fs::rename(&root, directory.path().join("original")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("photo-0.jpg"), b"replacement keeper").unwrap();
    fs::write(root.join("photo-1.jpg"), b"replacement target").unwrap();
    let result = scan
        .recycle(17, 0, vec![1], |_| panic!("root replaced"))
        .unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Skipped);
}

#[cfg(unix)]
#[test]
fn trash_skips_symlink_targets_and_parent_directories() {
    use std::os::unix::fs::symlink;
    for replace_parent in [false, true] {
        let directory = tempdir().unwrap();
        let root = directory.path().join("photos");
        fs::create_dir(&root).unwrap();
        let mut scan = identical_trash_scan(&root, 2);
        if replace_parent {
            let moved = directory.path().join("moved");
            fs::rename(&root, &moved).unwrap();
            symlink(&moved, &root).unwrap();
        } else {
            let target = root.join("photo-1.jpg");
            let moved = directory.path().join("original.jpg");
            fs::rename(&target, &moved).unwrap();
            symlink(&moved, &target).unwrap();
        }
        let result = scan
            .recycle(17, 0, vec![1], |_| panic!("symlink reached trash"))
            .unwrap();
        assert_eq!(result.outcomes[0].status, TrashStatus::Skipped);
    }
}

#[test]
fn trash_representative_reclassifies_burst_members_as_variants() {
    let directory = tempdir().unwrap();
    let mut scan = trash_scan(
        directory.path(),
        vec![
            features(0, 61, Some((5_000, true))),
            features(0x1fff, 81, Some((0, true))),
            features(0x1ffe, 81, Some((10_000, true))),
        ],
    );
    assert_eq!(scan.groups(Some(MatchKind::Burst), 0).total, 1);
    assert_eq!(scan.groups(Some(MatchKind::Variant), 0).total, 0);
    let result = scan.recycle(17, 0, vec![0], mock_trash).unwrap();
    assert_eq!(result.total_groups, 1);
    assert_eq!(scan.groups(Some(MatchKind::Burst), 0).total, 0);
    assert_eq!(scan.groups(Some(MatchKind::Variant), 0).total, 1);
    assert_eq!(
        scan.members(0, 0).unwrap().items[1].kind,
        Some(MatchKind::Variant)
    );
}
