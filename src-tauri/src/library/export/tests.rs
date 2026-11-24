use super::*;
use std::io::Write;
use tempfile::tempdir;
use zip::{ZipWriter, write::SimpleFileOptions};

fn read_scan(path: &Path) -> Scan {
    Scan::read(1, Root::new(path).unwrap(), |_| {})
}

fn ids(scan: &Scan, paths: &[&str]) -> Vec<usize> {
    paths
        .iter()
        .map(|path| {
            scan.items
                .iter()
                .find(|item| item.file.relative == Path::new(path))
                .unwrap()
                .file
                .id
        })
        .collect()
}

#[test]
fn exports_only_selected_media_and_keeps_original_bytes() {
    let source = tempdir().unwrap();
    let destination = tempdir().unwrap();
    let album = source.path().join("旅行");
    fs::create_dir(&album).unwrap();
    let photo = include_bytes!("../../../tests/fixtures/portrait-reference.jpg");
    let video = include_bytes!("../../../tests/fixtures/video-preview.mp4");
    fs::write(album.join("照片.jpg"), photo).unwrap();
    fs::write(album.join("视频.mp4"), video).unwrap();
    fs::write(album.join("未勾选.jpg"), photo).unwrap();
    let original_live = album.join("实况.livp");
    let mut live = ZipWriter::new(File::create(&original_live).unwrap());
    live.start_file("still.jpg", SimpleFileOptions::default())
        .unwrap();
    live.write_all(photo).unwrap();
    live.start_file("clip.mp4", SimpleFileOptions::default())
        .unwrap();
    live.write_all(video).unwrap();
    drop(live.finish().unwrap());

    // With no database present, exporting has no dependency on library write access.
    let scan = read_scan(source.path());
    let selected = ids(
        &scan,
        &[
            "旅行/照片.jpg",
            "旅行/视频.mp4",
            "旅行/实况.livp",
            "旅行/照片.jpg",
        ],
    );
    assert_eq!(
        export(&scan, selected, destination.path()).unwrap(),
        ExportSummary {
            succeeded: 3,
            skipped: 0,
            failed: 0
        }
    );
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 3);
    for name in ["照片.jpg", "视频.mp4", "实况.livp"] {
        assert_eq!(
            fs::read(destination.path().join(name)).unwrap(),
            fs::read(album.join(name)).unwrap()
        );
    }
    assert!(!source.path().join(".photosys").exists());
}

#[test]
fn skips_exact_matches_and_keeps_distinct_same_name_files() {
    let source = tempdir().unwrap();
    let destination = tempdir().unwrap();
    for (folder, bytes) in [("a", b"one"), ("b", b"two"), ("c", b"six")] {
        fs::create_dir(source.path().join(folder)).unwrap();
        fs::write(source.path().join(folder).join("旅行.jpg"), bytes).unwrap();
    }
    let scan = read_scan(source.path());
    let selected = ids(&scan, &["a/旅行.jpg", "b/旅行.jpg", "c/旅行.jpg"]);
    fs::write(destination.path().join("旅行.jpg"), b"one").unwrap();
    fs::write(destination.path().join("旅行_3.jpg"), b"six").unwrap();
    assert_eq!(
        export(&scan, selected.clone(), destination.path()).unwrap(),
        ExportSummary {
            succeeded: 1,
            skipped: 2,
            failed: 0
        }
    );
    assert_eq!(
        fs::read(destination.path().join("旅行.jpg")).unwrap(),
        b"one"
    );
    assert_eq!(
        fs::read(destination.path().join("旅行_1.jpg")).unwrap(),
        b"two"
    );
    assert_eq!(
        fs::read(destination.path().join("旅行_3.jpg")).unwrap(),
        b"six"
    );
    assert_eq!(
        export(&scan, selected.clone(), destination.path()).unwrap(),
        ExportSummary {
            succeeded: 0,
            skipped: 3,
            failed: 0
        }
    );

    fs::remove_file(destination.path().join("旅行.jpg")).unwrap();
    assert_eq!(
        export(&scan, selected[1..].to_vec(), destination.path()).unwrap(),
        ExportSummary {
            succeeded: 0,
            skipped: 2,
            failed: 0
        }
    );
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 2);
}

#[test]
fn skips_the_original_at_the_destination_and_continues_after_source_failures() {
    let source = tempdir().unwrap();
    fs::create_dir(source.path().join("trip")).unwrap();
    for name in [
        "self.jpg",
        "trip/good.jpg",
        "trip/deleted.jpg",
        "trip/replaced.jpg",
    ] {
        fs::write(source.path().join(name), b"original").unwrap();
    }
    let scan = read_scan(source.path());
    let selected = ids(
        &scan,
        &[
            "self.jpg",
            "trip/deleted.jpg",
            "trip/replaced.jpg",
            "trip/good.jpg",
        ],
    );
    fs::remove_file(source.path().join("trip/deleted.jpg")).unwrap();
    fs::write(source.path().join("trip/replaced.jpg"), b"replacement").unwrap();
    assert_eq!(
        export(&scan, selected, source.path()).unwrap(),
        ExportSummary {
            succeeded: 1,
            skipped: 1,
            failed: 2
        }
    );
    assert_eq!(
        fs::read(source.path().join("good.jpg")).unwrap(),
        b"original"
    );
    assert_eq!(
        fs::read(source.path().join("self.jpg")).unwrap(),
        b"original"
    );
    assert!(!source.path().join("replaced.jpg").exists());
    assert_eq!(fs::read_dir(source.path()).unwrap().count(), 3);
}

#[test]
fn rejects_invalid_destination_and_stale_scan() {
    let source = tempdir().unwrap();
    let not_a_directory = source.path().join("image.jpg");
    fs::write(&not_a_directory, b"image").unwrap();
    let scan = read_scan(source.path());
    assert!(
        export(&scan, vec![0], &not_a_directory)
            .unwrap_err()
            .contains("无法打开导出目录")
    );
    assert_eq!(
        export_current(u64::MAX, vec![0], source.path()),
        Err(crate::library::STALE_SCAN.into())
    );
}

#[test]
fn publication_failure_cleans_up_the_staged_copy() {
    let source = tempdir().unwrap();
    let destination = tempdir().unwrap();
    let name = format!("{}.jpg", "a".repeat(251));
    fs::write(source.path().join(&name), b"source").unwrap();
    fs::write(destination.path().join(&name), b"existing").unwrap();
    let scan = read_scan(source.path());
    assert_eq!(
        export(&scan, vec![0], destination.path()).unwrap(),
        ExportSummary {
            succeeded: 0,
            skipped: 0,
            failed: 1
        }
    );
    assert_eq!(
        fs::read(destination.path().join(name)).unwrap(),
        b"existing"
    );
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 1);
}
