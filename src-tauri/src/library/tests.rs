use std::fs;

use image::codecs::jpeg::JpegEncoder;
use image::{ImageEncoder, RgbImage};
use tempfile::tempdir;

use super::*;

fn session_for(path: &Path) -> Session {
    let root = Root::new(path).unwrap();
    let keys = favorites::read(&root).unwrap().unwrap_or_default();
    Session::from_scan(Scan::read(1, root, |_| {}), keys)
}

fn all() -> Filter {
    Filter {
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
    let page = session.items(all(), 0).unwrap();
    assert_eq!(page.total, 3);
    let [video, photo, broken] = page.items.as_slice() else {
        panic!("unexpected items")
    };
    assert_eq!(
        (video.kind, video.captured_on.as_deref()),
        (Kind::Video, Some("2025-06-01"))
    );
    assert!(!video.preview_available);
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
                0
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
        session.items(favorites(), 0).unwrap().items[0].name,
        "照片.jpg"
    );

    fs::rename(moved.join(&photo), moved.join("renamed.jpg")).unwrap();
    fs::write(moved.join("new.mp4"), []).unwrap();
    let rescan = session_for(&moved);
    assert_eq!(rescan.items(all(), 0).unwrap().total, 2);
    assert_eq!(rescan.items(favorites(), 0).unwrap().total, 0);
    assert!(
        session
            .scan
            .root
            .open_matching(&session.scan.items[0].file)
            .is_err()
    );

    fs::rename(moved.join("renamed.jpg"), moved.join(&photo)).unwrap();
    let rescan = session_for(&moved);
    assert_eq!(rescan.items(favorites(), 0).unwrap().total, 1);
    super::favorites::write(&rescan.scan.root, &photo, false).unwrap();
    assert_eq!(session_for(&moved).items(favorites(), 0).unwrap().total, 0);
}

#[test]
fn clamps_pages_to_matching_items() {
    let root = tempdir().unwrap();
    for i in 0..PAGE_SIZE + 3 {
        fs::write(root.path().join(format!("frame-{i:02}.png")), []).unwrap();
    }
    let session = session_for(root.path());
    let first = session.items(all(), 0).unwrap();
    let last = session.items(all(), usize::MAX).unwrap();
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
    assert_eq!(session.items(all(), 0).unwrap().total, 1);
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
