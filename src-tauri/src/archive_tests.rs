use super::*;
use tempfile::{TempDir, tempdir};
use zip::write::FileOptions;

fn folders() -> (TempDir, PathBuf, PathBuf) {
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let destination = source.parent().unwrap().join("destination");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&destination).unwrap();
    (root, source, destination)
}

fn put(directory: &Path, name: &str, contents: &[u8]) {
    let path = directory.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn reported_date_ranges(report: &str) -> &str {
    report
        .split_once("成功复制媒体日期段：\n")
        .unwrap()
        .1
        .split("\n\n")
        .next()
        .unwrap()
        .trim_end()
}

fn photo_with_date() -> Vec<u8> {
    // TIFF header, IFD0 with an Exif IFD pointer, then one DateTimeOriginal entry.
    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    tiff.extend_from_slice(&[
        1, 0, 0x69, 0x87, 4, 0, 1, 0, 0, 0, 26, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0x03, 0x90, 2, 0, 20, 0,
        0, 0, 44, 0, 0, 0, 0, 0, 0, 0,
    ]);
    tiff.extend_from_slice(b"2024:02:29 10:15:01\0");
    tiff
}

fn video_with_date(date: Option<chrono::NaiveDate>) -> Vec<u8> {
    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut atom = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        atom.extend_from_slice(kind);
        atom.extend_from_slice(payload);
        atom
    }

    let epoch = chrono::NaiveDate::from_ymd_opt(1904, 1, 1).unwrap();
    let seconds = date.map_or(0, |date| (date - epoch).num_seconds() as u32);
    let mut header = [0; 100];
    header[4..8].copy_from_slice(&seconds.to_be_bytes());
    header[12..16].copy_from_slice(&1000_u32.to_be_bytes());
    let mut video = atom(b"ftyp", b"qt  \0\0\0\0qt  ");
    video.extend(atom(b"moov", &atom(b"mvhd", &header)));
    video
}

fn live_photo(photo: &[u8], video: &[u8]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    let options = FileOptions::<()>::default().compression_method(zip::CompressionMethod::Deflated);
    archive.start_file("IMG.mov", options).unwrap();
    archive.write_all(video).unwrap();
    archive.start_file("IMG.dng", options).unwrap();
    archive.write_all(photo).unwrap();
    archive.finish().unwrap().into_inner()
}

#[test]
fn sorts_using_filename_then_capture_time_and_preserves_live_photos() {
    let (_root, source, destination) = folders();
    let photo = photo_with_date();
    let video = video_with_date(chrono::NaiveDate::from_ymd_opt(2025, 5, 1));
    let live = live_photo(&photo, &video);
    let live_with_video_date = live_photo(b"no capture date", &video);

    put(&source, "deep/nested/相片_2026-06-07 195758.DNG", &photo);
    put(&source, "deep/相片_2023-02-29.DNG", &photo);
    put(&source, "first_20260131_2026-06-07.JPG", b"photo");
    put(&source, "sample_2026_07_01.jpg", b"photo");
    put(&source, "video.MOV", &video);
    put(&source, "live.livp", &live);
    put(&source, "video-date.livp", &live_with_video_date);
    put(&source, "未设置录制时间.mov", &video_with_date(None));
    put(&source, "流水号12026060790.png", b"no metadata");
    put(&source, "readme.txt", b"ignored");

    let summary = run(&source, &destination, |_| {}).unwrap();
    assert_eq!(
        (summary.found, summary.copied, summary.other, summary.failed),
        (9, 9, 2, 0)
    );

    for path in [
        "2026/2026-06/相片_2026-06-07 195758.DNG",
        "2024/2024-02/相片_2023-02-29.DNG",
        "2026/2026-01/first_20260131_2026-06-07.JPG",
        "2026/2026-07/sample_2026_07_01.jpg",
        "2025/2025-05/video.MOV",
        "2025/2025-05/video-date.livp",
        "其它/流水号12026060790.png",
        "其它/未设置录制时间.mov",
    ] {
        assert!(
            destination.join(path).is_file(),
            "missing archived file {path}"
        );
    }
    assert_eq!(
        fs::read(destination.join("2024/2024-02/live.livp")).unwrap(),
        live
    );

    let report = fs::read_to_string(summary.report_path.unwrap()).unwrap();
    assert!(report.contains("开始时间："));
    assert!(report.contains("结束时间："));
    assert!(report.contains("成功复制：9"));
    assert!(report.contains("归入其它：2"));
    assert_eq!(
        reported_date_ranges(&report),
        "2024-2-29-2024-2-29\n2025-5-1-2025-5-1\n2026-1-31-2026-1-31\n2026-6-7-2026-6-7\n2026-7-1-2026-7-1"
    );
}

#[test]
fn reports_sorted_unique_date_ranges_across_media_months_years_and_leap_days() {
    let (_root, source, destination) = folders();
    for name in [
        "a_2026-09-15.jpg",
        "b_2024-03-01.mov",
        "c_2026-09-04.livp",
        "d_2026-01-01.mp4",
        "e_2024-02-28.jpg",
        "f_2026-09-14.mp4",
        "g_2025-12-31.png",
        "h_2024-02-29.livp",
        "i_2026-09-04.mov",
        "j_2026-09-02.jpg",
        "z_2026-09-01.jpg",
    ] {
        put(&source, name, b"media");
    }

    let summary = run(&source, &destination, |_| {}).unwrap();
    assert_eq!((summary.copied, summary.failed), (11, 0));
    let report = fs::read_to_string(summary.report_path.unwrap()).unwrap();
    assert_eq!(
        reported_date_ranges(&report),
        "2024-2-28-2024-3-1\n2025-12-31-2026-1-1\n2026-9-1-2026-9-2\n2026-9-4-2026-9-4\n2026-9-14-2026-9-15"
    );
}

#[test]
fn reports_no_available_dates_when_all_copied_media_have_unknown_dates() {
    let (_root, source, destination) = folders();
    put(&source, "photo.jpg", b"no metadata");
    put(&source, "video.mov", &video_with_date(None));

    let summary = run(&source, &destination, |_| {}).unwrap();
    assert_eq!((summary.copied, summary.other, summary.failed), (2, 2, 0));
    let report = fs::read_to_string(summary.report_path.unwrap()).unwrap();
    assert_eq!(reported_date_ranges(&report), "无可用日期");
    assert!(report.contains("归入其它：2"));
}

#[test]
fn handles_same_size_collisions_and_finds_duplicates_past_gaps() {
    let (_root, source, destination) = folders();
    put(&source, "a/20230607.jpg", b"same name A");
    put(&source, "b/20230607.jpg", b"same name B");
    put(&source, "c/20230607.jpg", b"same name B");

    let month = destination.join("2023/2023-06");
    put(&month, "20230607.jpg", b"preexisting");
    put(&month, "20230607_3.jpg", b"same name B");

    let first = run(&source, &destination, |_| {}).unwrap();
    assert_eq!(
        (first.copied, first.duplicates, first.renamed, first.failed),
        (1, 2, 1, 0)
    );
    assert_eq!(
        fs::read(month.join("20230607.jpg")).unwrap(),
        b"preexisting"
    );
    assert_eq!(
        fs::read(month.join("20230607_1.jpg")).unwrap(),
        b"same name A"
    );
    assert!(!month.join("20230607_2.jpg").exists());
    let first_report = first.report_path.unwrap();
    let report = fs::read_to_string(&first_report).unwrap();
    assert!(report.contains("重命名明细"));
    assert_eq!(reported_date_ranges(&report), "2023-6-7-2023-6-7");

    let second = run(&source, &destination, |_| {}).unwrap();
    assert_eq!((second.copied, second.duplicates, second.failed), (0, 3, 0));
    let second_report = second.report_path.unwrap();
    assert_ne!(first_report, second_report);
    let report = fs::read_to_string(second_report).unwrap();
    assert_eq!(reported_date_ranges(&report), "无可用日期");
    assert!(Path::new(&first_report).exists());
}

#[test]
fn excludes_nested_destination_and_reports_partial_failures() {
    let (_root, source, _) = folders();
    let destination = source.join("target");
    put(&destination, "already_here_2024-05-01.jpg", b"ignored");
    put(&destination, "2024", b"blocks date directory");
    put(&source, "2024-05-02.png", b"cannot archive");
    put(&source, "2026-05-02.png", b"valid");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        source.join("2026-05-02.png"),
        source.join("link_2026-05-02.png"),
    )
    .unwrap();

    assert!(run(&source, &source, |_| {}).is_err());
    assert!(run(&destination, &source, |_| {}).is_err());
    let summary = run(&source, &destination, |_| {}).unwrap();
    assert_eq!((summary.found, summary.copied, summary.failed), (2, 1, 1));
    assert!(source.join("2024-05-02.png").is_file());
    assert!(destination.join("2026/2026-05/2026-05-02.png").is_file());
    let report = fs::read_to_string(summary.report_path.unwrap()).unwrap();
    assert!(report.contains("复制失败：1"));
    assert!(report.contains("失败明细"));
    assert!(report.contains("2024-05-02.png"));
    assert_eq!(reported_date_ranges(&report), "2026-5-2-2026-5-2");
}

#[test]
fn writes_a_report_for_an_empty_source() {
    let (_root, source, destination) = folders();
    let mut progress = Vec::new();
    let summary = run(&source, &destination, |update| progress.push(update)).unwrap();
    assert_eq!(summary.found, 0);
    let report = fs::read_to_string(summary.report_path.unwrap()).unwrap();
    assert_eq!(reported_date_ranges(&report), "无可用日期");
    assert_eq!(
        progress,
        [
            ArchiveProgress::Scanning { found: 0 },
            ArchiveProgress::Reporting {
                processed: 0,
                total: 0,
            },
        ]
    );
}

#[test]
fn reports_copy_duplicate_and_failure_in_processed_total() {
    let (_root, source, destination) = folders();
    put(&source, "a_2026-06-01.jpg", b"copy");
    put(&source, "b_2026-06-02.jpg", b"duplicate");
    put(&source, "c_2024-06-01.jpg", b"fail");
    put(&source, "notes.txt", b"ignored");
    put(&destination, "2026/2026-06/b_2026-06-02.jpg", b"duplicate");
    put(&destination, "2024", b"blocks directory creation");

    let mut progress = Vec::new();
    let summary = run(&source, &destination, |update| progress.push(update)).unwrap();
    assert_eq!(
        (summary.copied, summary.duplicates, summary.failed),
        (1, 1, 1)
    );
    let report = fs::read_to_string(summary.report_path.unwrap()).unwrap();
    assert_eq!(reported_date_ranges(&report), "2026-6-1-2026-6-1");
    assert_eq!(
        progress.first(),
        Some(&ArchiveProgress::Scanning { found: 0 })
    );
    assert_eq!(
        progress
            .iter()
            .find(|update| matches!(update, ArchiveProgress::Archiving { .. })),
        Some(&ArchiveProgress::Archiving {
            processed: 0,
            total: 3,
            current_file: "a_2026-06-01.jpg".into(),
            transfer: None,
        })
    );
    assert_eq!(
        progress.last(),
        Some(&ArchiveProgress::Reporting {
            processed: 3,
            total: 3,
        })
    );
}

#[test]
fn reports_written_bytes_during_copy_and_skips_duplicate_and_empty_transfers() {
    let (_root, source, destination) = folders();
    let contents = vec![0x42; COPY_BUFFER_SIZE * 2 + 1];
    put(&source, "video.mov", &contents);
    put(&source, "empty.mov", b"");

    let mut progress = Vec::new();
    let video = source.join("video.mov");
    assert!(
        copy_media(&video, &destination, |update| progress
            .push(update.unwrap()))
        .unwrap()
        .is_some()
    );
    assert_eq!(fs::read(destination.join("video.mov")).unwrap(), contents);
    assert!(progress.len() >= 3);
    assert!(progress.first().unwrap().bytes_written < contents.len() as u64);
    assert!(
        progress
            .windows(2)
            .all(|updates| updates[0].bytes_written < updates[1].bytes_written)
    );
    assert!(
        progress
            .iter()
            .all(|update| update.total_bytes == contents.len() as u64)
    );
    assert_eq!(
        progress.last().unwrap().bytes_written,
        contents.len() as u64
    );

    let mut comparisons = 0;
    assert!(
        copy_media(&video, &destination, |update| {
            assert!(update.is_none());
            comparisons += 1;
        })
        .unwrap()
        .is_none()
    );
    assert!(comparisons > 1);

    let mut empty_updates = Vec::new();
    assert!(
        copy_media(&source.join("empty.mov"), &destination, |update| {
            empty_updates.push(update);
        })
        .unwrap()
        .is_some()
    );
    assert!(empty_updates.is_empty());
}

#[cfg(unix)]
#[test]
fn keeps_copy_statistics_when_the_report_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;

    let (_root, source, destination) = folders();
    put(&source, "2026-06-01.png", b"copy me");
    fs::create_dir_all(destination.join("2026/2026-06")).unwrap();
    let permissions = fs::metadata(&destination).unwrap().permissions();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o555)).unwrap();
    // A privileged test runner can still write into a read-only directory.
    let report_is_blocked = NamedTempFile::new_in(&destination).is_err();
    let summary = run(&source, &destination, |_| {});
    fs::set_permissions(&destination, permissions).unwrap();

    let summary = summary.unwrap();
    assert_eq!((summary.found, summary.copied, summary.failed), (1, 1, 0));
    if report_is_blocked {
        assert!(summary.report_path.is_none());
        assert!(summary.report_error.is_some());
    }
}
