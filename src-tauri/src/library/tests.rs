use std::fs;

use image::codecs::jpeg::JpegEncoder;
use image::{ImageEncoder, RgbImage};
use tempfile::tempdir;

use super::*;

fn session_for(path: &Path) -> (Session, Option<String>) {
    Session::from_scan(Scan::read(1, Root::new(path).unwrap(), |_| {}))
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

    let (session, warning) = session_for(path);
    assert!(warning.is_none());
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
        crate::similar::preview(
            &session.scan.root,
            &session.scan.item(photo.id).unwrap().file,
            false
        )
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
    assert!(
        crate::similar::preview(
            &session.scan.root,
            &session.scan.item(broken.id).unwrap().file,
            false
        )
        .is_err()
    );
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

    let (session, warning) = session_for(&original);
    assert!(warning.is_none());
    assert!(!original.join(".photosys").exists());
    favorites::write(&session.scan.root, &photo, true).unwrap();
    favorites::write(&session.scan.root, &photo, true).unwrap();
    drop(session);

    let moved = temp.path().join("album moved");
    fs::rename(&original, &moved).unwrap();
    let (session, warning) = session_for(&moved);
    assert!(warning.is_none());
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
    let (rescan, _) = session_for(&moved);
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
    let (rescan, _) = session_for(&moved);
    assert_eq!(rescan.items(favorites(), 0).unwrap().total, 1);
    super::favorites::write(&rescan.scan.root, &photo, false).unwrap();
    assert_eq!(
        session_for(&moved).0.items(favorites(), 0).unwrap().total,
        0
    );
}

#[test]
fn clamps_pages_to_matching_items() {
    let root = tempdir().unwrap();
    for i in 0..PAGE_SIZE + 3 {
        fs::write(root.path().join(format!("frame-{i:02}.png")), []).unwrap();
    }
    let (session, _) = session_for(root.path());
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
    let unreadable = root.path().join("unreadable.png");
    fs::write(&unreadable, []).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let (session, warning) = session_for(root.path());
    let write = favorites::write(&session.scan.root, Path::new("photo.png"), true);
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(warning.is_none());
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
    assert!(favorites::write(&root, Path::new("photo.png"), true).is_err());
    assert!(!outside.path().join("library.sqlite").exists());
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
