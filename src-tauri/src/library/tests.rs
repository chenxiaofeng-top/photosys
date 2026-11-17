use std::fs;

use image::codecs::jpeg::JpegEncoder;
use image::{ImageEncoder, RgbImage};
use tempfile::tempdir;

use super::*;

fn session_for(path: &Path) -> Session {
    let root = Root::new(path).unwrap();
    let keys = favorites::read(&root).unwrap();
    let rotations = if keys.is_some() {
        favorites::read_rotations(&root).unwrap()
    } else {
        HashMap::new()
    };
    Session::from_scan(
        Scan::read(1, root, |_| {}),
        keys.unwrap_or_default(),
        rotations,
    )
}

#[test]
fn rotation_survives_reopening_and_library_moves_without_changing_media_or_favorites() {
    let temp = tempdir().unwrap();
    let original = temp.path().join("album");
    fs::create_dir_all(original.join("旅行")).unwrap();
    let relative = Path::new("旅行/照片.jpg");
    let mut encoder = JpegEncoder::new(fs::File::create(original.join(relative)).unwrap());
    encoder
        .set_exif_metadata(
            b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec(),
        )
        .unwrap();
    encoder.encode_image(&RgbImage::new(80, 40)).unwrap();
    drop(encoder);
    let bytes = fs::read(original.join(relative)).unwrap();
    let root = Root::new(&original).unwrap();
    favorites::initialize(&root).unwrap();
    favorites::write(&root, relative, true).unwrap();
    let mut session = session_for(&original);
    let preview = session.scan.preview(0, true).unwrap();
    for rotation in [90, 180, 270, 0, 270] {
        assert_eq!(session.set_rotation(1, 0, rotation).unwrap(), rotation);
        let reopened = session_for(&original);
        let view = &reopened.items(all(), 0, Sort::default()).unwrap().items[0];
        assert_eq!(view.rotation, rotation);
        assert!(view.favorite);
        assert_eq!(session.scan.preview(0, true).unwrap(), preview);
        assert_eq!(fs::read(original.join(relative)).unwrap(), bytes);
        if rotation == 0 {
            assert!(favorites::read_rotations(&root).unwrap().is_empty());
        }
    }
    let moved = temp.path().join("moved album");
    fs::rename(&original, &moved).unwrap();
    assert_eq!(
        session_for(&moved)
            .items(all(), 0, Sort::default())
            .unwrap()
            .items[0]
            .rotation,
        270
    );
    fs::rename(moved.join(relative), moved.join("renamed.jpg")).unwrap();
    assert_eq!(
        session_for(&moved)
            .items(all(), 0, Sort::default())
            .unwrap()
            .items[0]
            .rotation,
        0
    );
}

#[test]
fn reading_old_database_does_not_migrate_until_rotation_is_saved() {
    let directory = tempdir().unwrap();
    let root = Root::new(directory.path()).unwrap();
    fs::write(directory.path().join("photo.png"), []).unwrap();
    favorites::initialize(&root).unwrap();
    let database = directory.path().join(".photosys/library.sqlite");
    let bytes = fs::read(&database).unwrap();
    assert!(favorites::read_rotations(&root).unwrap().is_empty());
    let mut session = session_for(directory.path());
    assert_eq!(
        session.items(all(), 0, Sort::default()).unwrap().items[0].rotation,
        0
    );
    assert_eq!(fs::read(&database).unwrap(), bytes);
    session.set_rotation(1, 0, 90).unwrap();
    assert_eq!(
        favorites::read_rotations(&root).unwrap()[&favorites::path_key(Path::new("photo.png"))],
        90
    );
}

#[test]
fn rotation_rejects_invalid_angles_stale_scans_videos_and_changed_files() {
    let directory = tempdir().unwrap();
    let root = Root::new(directory.path()).unwrap();
    favorites::initialize(&root).unwrap();
    fs::write(directory.path().join("photo.png"), []).unwrap();
    fs::write(directory.path().join("video.mp4"), []).unwrap();
    let mut session = session_for(directory.path());
    let id = session
        .scan
        .items
        .iter()
        .find(|item| item.kind == Kind::Photo)
        .unwrap()
        .file
        .id;
    let video = session
        .scan
        .items
        .iter()
        .find(|item| item.kind == Kind::Video)
        .unwrap()
        .file
        .id;
    assert!(session.set_rotation(1, id, 45).is_err());
    assert!(session.set_rotation(1, id, 360).is_err());
    assert_eq!(session.set_rotation(2, id, 90).unwrap_err(), STALE_SCAN);
    assert!(session.set_rotation(1, usize::MAX, 90).is_err());
    assert!(session.set_rotation(1, video, 90).is_err());
    session.set_rotation(1, id, 90).unwrap();
    fs::write(directory.path().join("photo.png"), b"changed").unwrap();
    assert_eq!(session.set_rotation(1, id, 180).unwrap_err(), STALE_SCAN);
    fs::remove_file(directory.path().join("photo.png")).unwrap();
    assert_eq!(session.set_rotation(1, id, 180).unwrap_err(), STALE_SCAN);
    assert_eq!(session.rotations[&id], 90);
    assert_eq!(favorites::read_rotations(&root).unwrap().len(), 1);
}

#[test]
fn failed_rotation_write_keeps_the_saved_and_in_memory_angles() {
    let directory = tempdir().unwrap();
    let root = Root::new(directory.path()).unwrap();
    favorites::initialize(&root).unwrap();
    fs::write(directory.path().join("photo.png"), []).unwrap();
    let mut session = session_for(directory.path());
    session.set_rotation(1, 0, 90).unwrap();
    let connection =
        rusqlite::Connection::open(directory.path().join(".photosys/library.sqlite")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_rotation BEFORE UPDATE ON rotations BEGIN SELECT RAISE(ABORT, 'test failure'); END").unwrap();
    assert!(
        session
            .set_rotation(1, 0, 180)
            .unwrap_err()
            .contains("无法保存旋转方向")
    );
    assert_eq!(session.rotations[&0], 90);
    assert_eq!(session_for(directory.path()).rotations[&0], 90);
}

#[test]
fn rotating_live_cover_keeps_video_and_package_unchanged() {
    use std::io::Write;
    let directory = tempdir().unwrap();
    let root = Root::new(directory.path()).unwrap();
    favorites::initialize(&root).unwrap();
    let path = directory.path().join("live.livp");
    let mut package = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    package
        .start_file("cover.HEIC", zip::write::SimpleFileOptions::default())
        .unwrap();
    package
        .write_all(include_bytes!("../../tests/fixtures/portrait-grid.heic"))
        .unwrap();
    package
        .start_file("live.mov", zip::write::SimpleFileOptions::default())
        .unwrap();
    package
        .write_all(include_bytes!("../../tests/fixtures/video-preview.mp4"))
        .unwrap();
    package.finish().unwrap();
    let bytes = fs::read(&path).unwrap();
    let mut session = session_for(directory.path());
    assert!(session.scan.items[0].is_live_photo);
    let cover = session.scan.preview(0, false).unwrap();
    session.set_rotation(1, 0, 270).unwrap();
    assert_eq!(session_for(directory.path()).rotations[&0], 270);
    assert_eq!(session.scan.preview(0, false).unwrap(), cover);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn read_only_old_library_browses_when_rotation_cannot_be_saved() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let directory = tempdir().unwrap();
    let root = Root::new(directory.path()).unwrap();
    favorites::initialize(&root).unwrap();
    fs::write(directory.path().join("photo.png"), []).unwrap();
    let database = directory.path().join(".photosys/library.sqlite");
    fs::set_permissions(&database, fs::Permissions::from_mode(0o444)).unwrap();
    let mut session = session_for(directory.path());
    let saved = session.set_rotation(1, 0, 90);
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(saved.is_err());
    assert!(session.rotations.is_empty());
    assert_eq!(
        session.items(all(), 0, Sort::default()).unwrap().items[0].rotation,
        0
    );
    assert!(favorites::read_rotations(&root).unwrap().is_empty());
}

fn all() -> Filter {
    Filter {
        query: String::new(),
        kind: None,
        from: None,
        to: None,
        unknown_date: false,
        favorites_only: false,
    }
}

#[test]
fn reserves_capacity_for_the_desktop_and_bounds_thumbnail_concurrency() {
    assert_eq!(thumbnail_concurrency(1), 1);
    assert_eq!(thumbnail_concurrency(2), 1);
    assert_eq!(thumbnail_concurrency(4), 2);
    assert_eq!(thumbnail_concurrency(6), 3);
    assert_eq!(thumbnail_concurrency(64), 3);
    let _thumbnails = THUMBNAIL_SLOTS
        .try_acquire_many(*THUMBNAIL_CONCURRENCY as u32)
        .unwrap();
    assert!(THUMBNAIL_SLOTS.try_acquire().is_err());
    let _detail = DETAIL_SLOTS.try_acquire().unwrap();
}

#[test]
fn sorts_dates_and_filters_mixed_media_with_on_demand_preview() {
    let directory = tempdir().unwrap();
    let path = directory.path();
    fs::create_dir(path.join("2024")).unwrap();

    let mut exif = b"II*\0\x08\0\0\0".to_vec();
    exif.extend([
        1, 0, 0x69, 0x87, 4, 0, 1, 0, 0, 0, 26, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0x03, 0x90, 2, 0, 20, 0,
        0, 0, 44, 0, 0, 0, 0, 0, 0, 0,
    ]);
    exif.extend(b"2024:02:29 10:15:01\0");
    let mut encoder =
        JpegEncoder::new(fs::File::create(path.join("2024/photo_20260520.JPG")).unwrap());
    encoder.set_exif_metadata(exif).unwrap();
    encoder.encode_image(&RgbImage::new(64, 32)).unwrap();
    fs::write(path.join("video_20250601.MOV"), []).unwrap();
    fs::write(path.join("broken.jpg"), b"broken photo").unwrap();
    fs::write(path.join("notes.txt"), b"ignored").unwrap();
    fs::create_dir(path.join(".photosys")).unwrap();
    fs::write(path.join(".photosys/ignored.jpg"), []).unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(path.join("2024"), path.join("linked-photos")).unwrap();

    let session = session_for(path);
    let page = session.items(all(), 0, Sort::default()).unwrap();
    assert_eq!(page.total, 3);
    let [video, photo, broken] = page.items.as_slice() else {
        panic!("unexpected items")
    };
    assert_eq!(
        (video.kind, video.captured_on.as_deref()),
        (Kind::Video, Some("2025-06-01"))
    );
    assert!(video.preview_available);
    assert_eq!(photo.captured_on.as_deref(), Some("2024-02-29"));
    assert!(photo.preview_available);
    assert!(
        session
            .scan
            .preview(photo.id, false)
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
    assert_eq!(
        session
            .items(
                Filter {
                    unknown_date: true,
                    ..all()
                },
                0,
                Sort::default()
            )
            .unwrap()
            .items[0]
            .id,
        broken.id
    );
    assert!(session.scan.preview(broken.id, false).is_err());
    let page = session
        .items(
            Filter {
                kind: Some(Kind::Photo),
                from: Some("2024-02-29".into()),
                to: Some("2024-02-29".into()),
                ..all()
            },
            0,
            Sort::default(),
        )
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, photo.id);
}

#[test]
fn favorites_follow_the_library_root_and_refresh_reads_file_changes() {
    let temp = tempdir().unwrap();
    let original = temp.path().join("album");
    fs::create_dir_all(original.join("旅行")).unwrap();
    let photo = Path::new("旅行").join("照片.jpg");
    fs::write(original.join(&photo), []).unwrap();

    let root = Root::new(&original).unwrap();
    assert!(favorites::initialize(&root).unwrap().is_empty());
    favorites::write(&root, &photo, true).unwrap();
    favorites::write(&root, &photo, true).unwrap();

    let moved = temp.path().join("album moved");
    fs::rename(&original, &moved).unwrap();
    let session = session_for(&moved);
    let favorites = || Filter {
        favorites_only: true,
        ..all()
    };
    assert_eq!(
        session
            .items(favorites(), 0, Sort::default())
            .unwrap()
            .items[0]
            .name,
        "照片.jpg"
    );

    fs::rename(moved.join(&photo), moved.join("renamed.jpg")).unwrap();
    fs::write(moved.join("new.mp4"), []).unwrap();
    let rescan = session_for(&moved);
    assert_eq!(rescan.items(all(), 0, Sort::default()).unwrap().total, 2);
    assert_eq!(
        rescan.items(favorites(), 0, Sort::default()).unwrap().total,
        0
    );
    assert!(
        session
            .scan
            .root
            .open_matching(&session.scan.items[0].file)
            .is_err()
    );

    fs::rename(moved.join("renamed.jpg"), moved.join(&photo)).unwrap();
    let rescan = session_for(&moved);
    assert_eq!(
        rescan.items(favorites(), 0, Sort::default()).unwrap().total,
        1
    );
    super::favorites::write(&rescan.scan.root, &photo, false).unwrap();
    assert_eq!(
        session_for(&moved)
            .items(favorites(), 0, Sort::default())
            .unwrap()
            .total,
        0
    );
}

#[test]
fn clamps_pages_to_matching_items() {
    let root = tempdir().unwrap();
    for i in 0..PAGE_SIZE + 3 {
        fs::write(root.path().join(format!("frame-{i:02}.png")), []).unwrap();
    }
    let session = session_for(root.path());
    let first = session.items(all(), 0, Sort::default()).unwrap();
    let last = session.items(all(), usize::MAX, Sort::default()).unwrap();
    assert_eq!(first.items.len(), PAGE_SIZE);
    assert_eq!(
        (last.page, last.items.len(), last.total),
        (1, 3, PAGE_SIZE + 3)
    );
    assert_ne!(first.items[PAGE_SIZE - 1].id, last.items[0].id);
}

#[cfg(unix)]
#[test]
fn read_only_album_still_browses_when_favorite_write_fails() {
    use std::os::unix::fs::PermissionsExt;

    // Permissions do not restrict the superuser.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let root = tempdir().unwrap();
    fs::write(root.path().join("photo.png"), []).unwrap();
    let data = root.path().join(".photosys");
    let database = data.join("library.sqlite");
    favorites::initialize(&Root::new(root.path()).unwrap()).unwrap();
    let unreadable = root.path().join("unreadable.png");
    fs::write(&unreadable, []).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&data, fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let session = session_for(root.path());
    let write = favorites::write(&session.scan.root, Path::new("photo.png"), true);
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(session.items(all(), 0, Sort::default()).unwrap().total, 1);
    assert_eq!(session.scan.issues[0].path, "unreadable.png");
    assert!(write.is_err());
}

#[cfg(unix)]
#[test]
fn does_not_follow_a_symlink_to_portable_data() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join(".photosys")).unwrap();
    let root = Root::new(root.path()).unwrap();
    assert!(favorites::read(&root).is_err());
    assert!(favorites::initialize(&root).is_err());
    assert!(favorites::write(&root, Path::new("photo.png"), true).is_err());
    assert!(!outside.path().join("library.sqlite").exists());
}

#[cfg(unix)]
#[test]
fn does_not_follow_a_symlink_to_the_database() {
    let album = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let target = outside.path().join("library.sqlite");
    let data = album.path().join(".photosys");
    fs::create_dir(&data).unwrap();
    std::os::unix::fs::symlink(&target, data.join("library.sqlite")).unwrap();

    let root = Root::new(album.path()).unwrap();
    assert!(favorites::read(&root).is_err());
    assert!(favorites::initialize(&root).is_err());
    assert!(!target.exists());
}

#[test]
fn startup_prefers_portable_library_then_remembered_directory() {
    let portable = tempdir().unwrap();
    let remembered = tempdir().unwrap();
    let config = tempdir().unwrap();
    let preference = config.path().join("app/library-directory");
    let discover = || find_startup_library(Some(portable.path()), Ok(&preference));

    assert!(discover().unwrap().is_none());
    assert!(!portable.path().join(".photosys").exists());
    assert!(!preference.exists());

    save_preference(&preference, remembered.path().to_str().unwrap()).unwrap();
    assert!(discover().unwrap().is_none());
    assert!(!remembered.path().join(".photosys").exists());
    let root = Root::new(remembered.path()).unwrap();
    favorites::initialize(&root).unwrap();
    assert_eq!(discover().unwrap().unwrap().root.path, root.path);

    let portable_root = Root::new(portable.path()).unwrap();
    favorites::initialize(&portable_root).unwrap();
    assert_eq!(discover().unwrap().unwrap().root.path, portable_root.path);
    assert_eq!(
        read_preference(&preference).unwrap().as_deref(),
        Some(remembered.path())
    );

    fs::write(portable.path().join(".photosys/library.sqlite"), b"corrupt").unwrap();
    assert_eq!(discover().unwrap().unwrap().root.path, root.path);
    drop(remembered);
    assert!(discover().is_err());
}

#[test]
fn opens_empty_libraries_and_preserves_current_session_on_failure() {
    let empty = tempdir().unwrap();
    let directory = empty.path().to_str().unwrap();
    assert!(open_directory(directory, OpenMode::Refresh, |_| {}).is_err());
    assert!(!empty.path().join(".photosys").exists());

    let opened = open_directory(directory, OpenMode::Select, |_| {}).unwrap();
    assert_eq!(opened.total, 0);
    assert!(
        startup_candidate(empty.path())
            .unwrap()
            .unwrap()
            .keys
            .is_empty()
    );

    let partial = tempdir().unwrap();
    fs::create_dir(partial.path().join(".photosys")).unwrap();
    let directory = partial.path().to_str().unwrap();
    let opened = open_directory(directory, OpenMode::Select, |_| {}).unwrap();
    let refreshed = open_directory(directory, OpenMode::Refresh, |_| {}).unwrap();
    assert_eq!(refreshed.total, opened.total);

    let invalid = tempdir().unwrap();
    let invalid_data = invalid.path().join(".photosys");
    fs::create_dir(&invalid_data).unwrap();
    let invalid_db = invalid_data.join("library.sqlite");
    fs::write(&invalid_db, b"corrupt database").unwrap();
    let invalid_directory = invalid.path().to_str().unwrap();
    assert!(open_directory(invalid_directory, OpenMode::Select, |_| {}).is_err());
    assert_eq!(fs::read(&invalid_db).unwrap(), b"corrupt database");
    assert!(scan(refreshed.scan_id).is_ok());

    fs::remove_dir_all(&invalid_data).unwrap();
    fs::write(&invalid_data, b"blocking file").unwrap();
    assert!(open_directory(invalid_directory, OpenMode::Select, |_| {}).is_err());
    assert_eq!(fs::read(&invalid_data).unwrap(), b"blocking file");
    assert!(scan(refreshed.scan_id).is_ok());
}

#[test]
fn concurrent_initialization_keeps_one_valid_database() {
    let album = tempdir().unwrap();
    let ready = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let root = Root::new(album.path()).unwrap();
                ready.wait();
                assert!(favorites::initialize(&root).unwrap().is_empty());
            });
        }
    });
    let data = album.path().join(".photosys");
    assert_eq!(fs::read_dir(data).unwrap().count(), 1);
    let root = Root::new(album.path()).unwrap();
    let photo = Path::new("photo.png");
    favorites::write(&root, photo, true).unwrap();
    assert!(
        favorites::initialize(&root)
            .unwrap()
            .contains(&favorites::path_key(photo))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn uses_the_appimage_parent_instead_of_the_working_directory() {
    let root = tempdir().unwrap();
    let app = root.path().join("PhotoSys.AppImage");
    fs::write(&app, []).unwrap();
    assert_eq!(appimage_parent(&app), Some(root.path()));
    assert!(appimage_parent(root.path()).is_none());
    assert!(appimage_parent(Path::new("PhotoSys.AppImage")).is_none());
}

#[test]
fn searches_full_paths_case_insensitively_and_combines_all_filters() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("旅行 Album")).unwrap();
    for name in [
        "旅行 Album/IMG_20250102.JPG",
        "旅行 Album/clip_20250102.mp4",
        "other_20250102.jpg",
        "旅行 Album/unknown.jpg",
    ] {
        fs::write(root.path().join(name), []).unwrap();
    }
    let mut session = session_for(root.path());
    let search = |query: &str| Filter {
        query: query.into(),
        ..all()
    };
    assert_eq!(
        session
            .items(search("  旅行 aLBUM  "), 0, Sort::default())
            .unwrap()
            .total,
        3
    );
    assert_eq!(
        session
            .items(search("iMg_"), 0, Sort::default())
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        session
            .items(search("  "), 0, Sort::default())
            .unwrap()
            .total,
        4
    );
    let empty = session
        .items(search("missing"), usize::MAX, Sort::NameAsc)
        .unwrap();
    assert_eq!((empty.total, empty.page, empty.items.len()), (0, 0, 0));
    let photo = session
        .items(search("img_"), 0, Sort::default())
        .unwrap()
        .items[0]
        .id;
    session.favorites.insert(photo);
    let combined = Filter {
        kind: Some(Kind::Photo),
        from: Some("2025-01-02".into()),
        to: Some("2025-01-02".into()),
        favorites_only: true,
        ..search("ALBUM")
    };
    let page = session.items(combined, 0, Sort::NameDesc).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, photo);
    let page = session
        .items(
            Filter {
                unknown_date: true,
                ..search("旅行")
            },
            0,
            Sort::DateAsc,
        )
        .unwrap();
    assert_eq!(page.items[0].name, "unknown.jpg");
    assert!(
        session
            .items(
                Filter {
                    from: Some("2025-02-01".into()),
                    to: Some("2025-01-01".into()),
                    ..all()
                },
                0,
                Sort::default()
            )
            .is_err()
    );
}

#[test]
fn all_sorts_keep_unknown_dates_last_and_stable_file_ids() {
    let root = tempdir().unwrap();
    let entries = [
        ("a/Z.jpg", 3),
        ("b/z.JPG", 3),
        ("B.jpg", 8),
        ("c.jpg", 1),
        ("D.jpg", 8),
    ];
    fs::create_dir(root.path().join("a")).unwrap();
    fs::create_dir(root.path().join("b")).unwrap();
    for (name, size) in entries {
        fs::write(root.path().join(name), vec![0; size]).unwrap();
    }
    let mut session = session_for(root.path());
    for item in &mut Arc::get_mut(&mut session.scan).unwrap().items {
        item.captured_on = match item.file.path_label().replace('\\', "/").as_str() {
            "a/Z.jpg" | "b/z.JPG" => Some(NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()),
            "B.jpg" => Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            _ => None,
        };
    }
    let paths = |sort| {
        session
            .items(all(), 0, sort)
            .unwrap()
            .items
            .into_iter()
            .map(|item| {
                assert_eq!(
                    session.scan.item(item.id).unwrap().file.path_label(),
                    item.relative_path
                );
                item.relative_path.replace('\\', "/")
            })
            .collect::<Vec<_>>()
    };
    for (sort, expected) in [
        (
            Sort::DateDesc,
            vec!["a/Z.jpg", "b/z.JPG", "B.jpg", "D.jpg", "c.jpg"],
        ),
        (
            Sort::DateAsc,
            vec!["B.jpg", "a/Z.jpg", "b/z.JPG", "D.jpg", "c.jpg"],
        ),
        (
            Sort::NameAsc,
            vec!["B.jpg", "c.jpg", "D.jpg", "a/Z.jpg", "b/z.JPG"],
        ),
        (
            Sort::NameDesc,
            vec!["a/Z.jpg", "b/z.JPG", "D.jpg", "c.jpg", "B.jpg"],
        ),
        (
            Sort::SizeDesc,
            vec!["B.jpg", "D.jpg", "a/Z.jpg", "b/z.JPG", "c.jpg"],
        ),
        (
            Sort::SizeAsc,
            vec!["c.jpg", "a/Z.jpg", "b/z.JPG", "B.jpg", "D.jpg"],
        ),
    ] {
        assert_eq!(paths(sort), expected);
    }
}

#[test]
fn search_and_sort_apply_before_pagination_without_reassigning_ids() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("match")).unwrap();
    for i in 0..PAGE_SIZE + 3 {
        fs::write(
            root.path().join(format!("match/frame-{i:02}.jpg")),
            vec![0; i],
        )
        .unwrap();
        fs::write(root.path().join(format!("other-{i:02}.jpg")), []).unwrap();
    }
    let session = session_for(root.path());
    let search = || Filter {
        query: "MATCH".into(),
        ..all()
    };
    let first = session.items(search(), 0, Sort::SizeDesc).unwrap();
    let last = session.items(search(), usize::MAX, Sort::SizeDesc).unwrap();
    assert_eq!(
        (first.total, first.items.len(), last.page, last.items.len()),
        (27, 24, 1, 3)
    );
    assert_eq!(first.items[0].name, "frame-26.jpg");
    assert_eq!(last.items[0].name, "frame-02.jpg");
    assert_eq!(last.items[2].name, "frame-00.jpg");
    for item in first.items.iter().chain(&last.items) {
        assert_eq!(
            session.scan.item(item.id).unwrap().file.path_label(),
            item.relative_path
        );
    }
}

#[test]
fn location_and_clipboard_paths_validate_file_identity() {
    let root = tempdir().unwrap();
    let path = root.path().join("旅行 & space #.jpg");
    fs::write(&path, []).unwrap();
    let session = session_for(root.path());
    let resolved = session.scan.item_path(0).unwrap();
    assert!(resolved.is_absolute());
    assert_eq!(
        clipboard_path(&resolved).unwrap(),
        clipboard_path(&path.canonicalize().unwrap()).unwrap()
    );
    assert!(session.scan.item_path(usize::MAX).is_err());
    fs::write(&path, [1]).unwrap();
    assert_eq!(session.scan.item_path(0).unwrap_err(), STALE_SCAN);
    fs::remove_file(&path).unwrap();
    assert_eq!(session.scan.item_path(0).unwrap_err(), STALE_SCAN);
}

#[cfg(unix)]
#[test]
fn refuses_to_copy_non_utf8_paths_and_to_locate_symlink_replacements() {
    use std::os::unix::ffi::OsStrExt;
    let path = Path::new(std::ffi::OsStr::from_bytes(b"/album/bad-\xff.jpg"));
    assert!(clipboard_path(path).is_err());
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let photo = root.path().join("photo.jpg");
    let target = outside.path().join("target.jpg");
    fs::write(&photo, []).unwrap();
    fs::write(&target, []).unwrap();
    let session = session_for(root.path());
    fs::remove_file(&photo).unwrap();
    std::os::unix::fs::symlink(&target, &photo).unwrap();
    assert_eq!(session.scan.item_path(0).unwrap_err(), STALE_SCAN);
}

#[test]
fn reports_reveal_success_parent_fallback_and_total_failure() {
    let path = Path::new("/album/photo.jpg");
    assert_eq!(
        reveal_path(path, |_| Ok(()), |_| panic!("must not open parent")).unwrap(),
        RevealResult::Revealed
    );
    assert_eq!(
        reveal_path(
            path,
            |_| Err("no file manager".into()),
            |parent| {
                assert_eq!(parent, Path::new("/album"));
                Ok(())
            }
        )
        .unwrap(),
        RevealResult::OpenedParent
    );
    let error = reveal_path(
        path,
        |_| Err("reveal failure".into()),
        |_| Err("open failure".into()),
    )
    .unwrap_err();
    assert!(error.contains("reveal failure") && error.contains("open failure"));
}
