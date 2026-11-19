use super::*;
use crate::library::{self, Kind};
use std::{fs, path::Path};
use tempfile::tempdir;

fn fixture(path: &Path) -> Session {
    fs::create_dir_all(path.join("旅行")).unwrap();
    for name in [
        "20240101_photo.jpg",
        "20240102_video.mp4",
        "20240103_live.livp",
        "unknown.png",
    ] {
        fs::write(path.join("旅行").join(name), b"original-media").unwrap();
    }
    let root = Root::new(path).unwrap();
    favorites::initialize(&root).unwrap();
    library::tests::session_for(path)
}
fn all() -> Filter {
    serde_json::from_value(serde_json::json!({"query":"","kind":null,"from":null,"to":null,"unknownDate":false,"favoritesOnly":false})).unwrap()
}
fn create(s: &Session, name: &str, ids: Vec<usize>) -> i64 {
    edit(
        s,
        Edit::Create {
            name: name.into(),
            file_ids: ids,
        },
    )
    .unwrap()
    .album_id
}
fn ids(s: &Session) -> Vec<usize> {
    s.scan.items.iter().map(|i| i.file.id).collect()
}

#[test]
fn named_albums_keep_multiple_memberships_and_original_media() {
    let dir = tempdir().unwrap();
    let s = fixture(dir.path());
    let all_ids = ids(&s);
    let first = create(&s, "  周末露营  ", all_ids.clone());
    let second = create(&s, "周末露营", vec![all_ids[0]]);
    edit(
        &s,
        Edit::Add {
            album_id: first,
            file_ids: all_ids.clone(),
        },
    )
    .unwrap();
    assert_eq!(items(&s, first, all(), Sort::DateDesc, 0).unwrap().total, 4);
    let result = list(&s, "露营", 0).unwrap();
    assert_eq!(result.page.total, 2);
    assert!(result.writable);
    assert_eq!(result.page.items[1].name, "周末露营");
    assert_eq!(result.page.items[1].count, 4);
    assert!(result.page.items[1].cover.as_ref().unwrap().is_live_photo);
    let members = items(&s, first, all(), Sort::DateAsc, 0).unwrap();
    assert!(members.items.iter().any(|i| i.kind == Kind::Video));
    assert!(members.items.iter().any(|i| i.is_live_photo));
    edit(
        &s,
        Edit::Rename {
            album_id: first,
            name: "杭州之旅".into(),
        },
    )
    .unwrap();
    edit(
        &s,
        Edit::Remove {
            album_id: first,
            file_ids: vec![all_ids[0]],
        },
    )
    .unwrap();
    assert_eq!(items(&s, first, all(), Sort::DateDesc, 0).unwrap().total, 3);
    assert_eq!(
        items(&s, second, all(), Sort::DateDesc, 0).unwrap().total,
        1
    );
    edit(&s, Edit::Delete { album_id: first }).unwrap();
    assert!(items(&s, first, all(), Sort::DateDesc, 0).is_err());
    let c = open(&s.scan.root, false).unwrap();
    assert!(paths(&c, first).unwrap().is_empty());
    assert_eq!(list(&s, "", 0).unwrap().page.total, 1);
    for item in &s.scan.items {
        assert_eq!(
            fs::read(dir.path().join(&item.file.relative)).unwrap(),
            b"original-media"
        );
    }
}

#[test]
fn old_databases_are_read_without_migration_and_failed_creates_roll_back() {
    let dir = tempdir().unwrap();
    let s = fixture(dir.path());
    assert_eq!(list(&s, "", 0).unwrap().page.total, 0);
    assert!(!validate_schema(&open(&s.scan.root, false).unwrap()).unwrap());
    for value in ["  ".to_string(), "字".repeat(81)] {
        assert!(
            edit(
                &s,
                Edit::Create {
                    name: value,
                    file_ids: vec![]
                }
            )
            .is_err()
        );
    }
    assert!(
        edit(
            &s,
            Edit::Create {
                name: "失败".into(),
                file_ids: vec![0, usize::MAX]
            }
        )
        .is_err()
    );
    assert!(!validate_schema(&open(&s.scan.root, false).unwrap()).unwrap());
    let id = create(&s, &"😀".repeat(80), vec![]);
    assert_eq!(items(&s, id, all(), Sort::DateDesc, 0).unwrap().total, 0);
    assert_eq!(
        open(&s.scan.root, false)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn edits_are_atomic_when_a_member_is_missing_or_invalid() {
    let dir = tempdir().unwrap();
    let s = fixture(dir.path());
    let id = create(&s, "旅行", vec![0, 1]);
    assert!(
        edit(
            &s,
            Edit::Add {
                album_id: id,
                file_ids: vec![2, usize::MAX]
            }
        )
        .is_err()
    );
    assert_eq!(items(&s, id, all(), Sort::DateDesc, 0).unwrap().total, 2);
    assert!(
        edit(
            &s,
            Edit::Remove {
                album_id: id,
                file_ids: vec![0, usize::MAX]
            }
        )
        .is_err()
    );
    assert_eq!(items(&s, id, all(), Sort::DateDesc, 0).unwrap().total, 2);
    fs::remove_file(dir.path().join(&s.scan.items[2].file.relative)).unwrap();
    assert!(
        edit(
            &s,
            Edit::Add {
                album_id: id,
                file_ids: vec![2, 3]
            }
        )
        .is_err()
    );
    assert_eq!(
        paths(&open(&s.scan.root, false).unwrap(), id)
            .unwrap()
            .len(),
        2
    );
    assert!(
        edit(
            &s,
            Edit::Rename {
                album_id: 999,
                name: "不存在".into()
            }
        )
        .is_err()
    );
}

#[test]
fn missing_renamed_and_restored_paths_survive_reopen_and_library_move() {
    let dir = tempdir().unwrap();
    let original = dir.path().join("original");
    let s = fixture(&original);
    let id = create(&s, "杭州之旅", ids(&s));
    drop(s);
    let moved = dir.path().join("moved");
    fs::rename(&original, &moved).unwrap();
    let s = library::tests::session_for(&moved);
    assert_eq!(list(&s, "", 0).unwrap().page.items[0].count, 4);
    fs::rename(
        moved.join("旅行/20240101_photo.jpg"),
        moved.join("renamed.jpg"),
    )
    .unwrap();
    fs::remove_file(moved.join("旅行/20240103_live.livp")).unwrap();
    // Even an old scan must stop returning files that disappeared externally.
    assert_eq!(items(&s, id, all(), Sort::DateDesc, 0).unwrap().total, 2);
    assert_eq!(list(&s, "", 0).unwrap().page.items[0].count, 2);
    let s = library::tests::session_for(&moved);
    assert_eq!(items(&s, id, all(), Sort::DateDesc, 0).unwrap().total, 2);
    fs::rename(
        moved.join("renamed.jpg"),
        moved.join("旅行/20240101_photo.jpg"),
    )
    .unwrap();
    fs::write(moved.join("旅行/20240103_live.livp"), b"original-media").unwrap();
    let s = library::tests::session_for(&moved);
    assert_eq!(items(&s, id, all(), Sort::DateDesc, 0).unwrap().total, 4);
    assert_eq!(
        paths(&open(&s.scan.root, false).unwrap(), id)
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn album_queries_filter_sort_and_page_consistently() {
    let dir = tempdir().unwrap();
    let mut s = fixture(dir.path());
    favorites::write(&s.scan.root, &s.scan.items[0].file.relative, true).unwrap();
    s.favorites.insert(0);
    let id = create(&s, "活动", ids(&s));
    let mut filter = all();
    filter.kind = Some(Kind::Video);
    assert_eq!(items(&s, id, filter, Sort::NameAsc, 0).unwrap().total, 1);
    let mut filter = all();
    filter.favorites_only = true;
    assert!(items(&s, id, filter, Sort::DateDesc, 0).unwrap().items[0].favorite);
    let mut filter = all();
    filter.unknown_date = true;
    assert_eq!(items(&s, id, filter, Sort::DateDesc, 0).unwrap().total, 1);
    let mut filter = all();
    filter.query = "PHOTO".into();
    filter.from = Some("2024-01-01".into());
    filter.to = Some("2024-01-01".into());
    assert_eq!(items(&s, id, filter, Sort::DateDesc, 0).unwrap().total, 1);
    for i in 0..30 {
        create(&s, &format!("活动{i}"), vec![]);
    }
    assert_eq!(list(&s, "活动", 0).unwrap().page.items.len(), 24);
    assert_eq!(list(&s, "活动", 1).unwrap().page.items.len(), 7);
    assert_eq!(list(&s, "活动", 999).unwrap().page.page, 1);
    for i in 0..30 {
        fs::write(
            dir.path().join(format!("202501{:02}_extra.jpg", i + 1)),
            b"media",
        )
        .unwrap();
    }
    let s = library::tests::session_for(dir.path());
    edit(
        &s,
        Edit::Add {
            album_id: id,
            file_ids: ids(&s),
        },
    )
    .unwrap();
    let second = items(&s, id, all(), Sort::NameAsc, 1).unwrap();
    assert_eq!(second.total, 34);
    assert_eq!(second.items.len(), 10);
    assert_eq!(items(&s, id, all(), Sort::NameAsc, 999).unwrap().page, 1);
}

#[test]
fn partial_and_incompatible_schema_is_reported_instead_of_hidden() {
    let dir = tempdir().unwrap();
    let s = fixture(dir.path());
    let c = open(&s.scan.root, true).unwrap();
    c.execute_batch("CREATE TABLE albums(id INTEGER PRIMARY KEY,name TEXT)")
        .unwrap();
    assert!(list(&s, "", 0).is_err());
    assert!(
        edit(
            &s,
            Edit::Create {
                name: "test".into(),
                file_ids: vec![]
            }
        )
        .is_err()
    );
    c.execute_batch("CREATE TABLE album_members(wrong TEXT)")
        .unwrap();
    assert!(list(&s, "", 0).is_err());
}

#[test]
fn stale_scan_ids_cannot_mutate_albums() {
    let _serial = library::TEST_SESSION.lock().unwrap();
    let dir = tempdir().unwrap();
    let s = fixture(dir.path());
    *library::session().unwrap() = Some(s);
    assert!(
        edit_album(
            999,
            Edit::Create {
                name: "stale".into(),
                file_ids: vec![0]
            }
        )
        .is_err()
    );
    assert_eq!(
        list(library::session().unwrap().as_ref().unwrap(), "", 0)
            .unwrap()
            .page
            .total,
        0
    );
    *library::session().unwrap() = None;
}

#[test]
fn read_only_database_can_be_browsed_without_schema_changes() {
    let dir = tempdir().unwrap();
    let s = fixture(dir.path());
    let id = create(&s, "只读旅行", vec![0]);
    let path = dir.path().join(".photosys/library.sqlite");
    let original = fs::metadata(&path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&path, readonly).unwrap();
    let result = list(&s, "", 0).unwrap();
    assert!(!result.writable);
    assert_eq!(items(&s, id, all(), Sort::DateDesc, 0).unwrap().total, 1);
    assert!(edit(&s, Edit::Delete { album_id: id }).is_err());
    fs::set_permissions(path, original).unwrap();
}
