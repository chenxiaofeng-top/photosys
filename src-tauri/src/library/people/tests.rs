use super::super::{self as library, OpenMode, TEST_SESSION};
use super::*;
use crate::ai::faces::Face;
use image::RgbImage;
use rusqlite::params;
use std::{fs, path::Path, sync::atomic::AtomicBool};
fn face(axis: usize) -> Face {
    let mut v = vec![0.; faces::DIM];
    v[axis] = 1.;
    Face {
        bbox: [0.1, 0.1, 0.4, 0.4],
        confidence: 0.99,
        vector: Some(v),
        thumbnail: vec![1, 2, 3],
    }
}
fn all() -> Filter {
    Filter {
        query: String::new(),
        kind: None,
        from: None,
        to: None,
        unknown_date: false,
        favorites_only: false,
        location_mode: Default::default(),
        location_selection: None,
    }
}
fn open(path: &Path) -> u64 {
    library::open_directory(path.to_str().unwrap(), OpenMode::Select, |_| {})
        .unwrap()
        .scan_id
}
fn save(scan_id: u64) -> Vec<i64> {
    let scan = scan(scan_id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    let mut ids = Vec::new();
    for item in &scan.items {
        ids.extend(
            store
                .save(
                    &favorites::path_key(&item.file.relative),
                    &item.file.stamp.portable_key(),
                    0,
                    &Ok(vec![face(0), face(1)]),
                )
                .unwrap(),
        );
    }
    ids
}
#[test]
fn persistent_edits_undo_are_atomic_and_keep_corrections_after_reindex() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("people.sqlite");
    let mut store = Store::at(&path, true).unwrap().unwrap();
    let ids = store
        .save(b"a.jpg", b"one", 0, &Ok(vec![face(0), face(1)]))
        .unwrap();
    store
        .edit(Edit::Assign {
            face_ids: vec![ids[0]],
            person_id: None,
            name: Some("妈妈".into()),
        })
        .unwrap();
    let person: i64 = store
        .connection
        .query_row("SELECT person_id FROM faces WHERE id=?1", [ids[0]], |r| {
            r.get(0)
        })
        .unwrap();
    store
        .edit(Edit::Assign {
            face_ids: vec![ids[1]],
            person_id: None,
            name: Some("爸爸".into()),
        })
        .unwrap();
    let other: i64 = store
        .connection
        .query_row("SELECT person_id FROM faces WHERE id=?1", [ids[1]], |r| {
            r.get(0)
        })
        .unwrap();
    // Invalid edits roll back both a new person and any partially applied changes.
    assert!(
        store
            .edit(Edit::Assign {
                face_ids: vec![ids[0], 999],
                person_id: None,
                name: Some("无效".into())
            })
            .is_err()
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM people", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    store
        .edit(Edit::Assign {
            face_ids: vec![ids[0]],
            person_id: Some(other),
            name: None,
        })
        .unwrap();
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM rejections WHERE face_id=?1 AND person_id=?2",
                params![ids[0], person],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    store.undo().unwrap();
    assert_eq!(
        store
            .connection
            .query_row("SELECT person_id FROM faces WHERE id=?1", [ids[0]], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        person
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM rejections", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    store
        .edit(Edit::Rename {
            person_id: person,
            name: "家人".into(),
        })
        .unwrap();
    drop(store);
    let mut store = Store::at(&path, true).unwrap().unwrap();
    store.undo().unwrap();
    assert_eq!(
        store
            .connection
            .query_row("SELECT name FROM people WHERE id=?1", [person], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "妈妈"
    );
    store
        .edit(Edit::Merge {
            source_id: other,
            target_id: person,
        })
        .unwrap();
    assert!(
        store
            .connection
            .query_row("SELECT archived FROM people WHERE id=?1", [other], |r| r
                .get::<_, bool>(
                0
            ))
            .unwrap()
    );
    store.undo().unwrap();
    assert_eq!(
        store
            .connection
            .query_row("SELECT person_id FROM faces WHERE id=?1", [ids[1]], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        other
    );
    store
        .edit(Edit::Ignore {
            face_ids: vec![ids[0]],
        })
        .unwrap();
    store
        .edit(Edit::Restore {
            face_ids: vec![ids[0]],
        })
        .unwrap();
    assert!(
        !store
            .connection
            .query_row("SELECT ignored FROM faces WHERE id=?1", [ids[0]], |r| r
                .get::<_, bool>(0))
            .unwrap()
    );
    store
        .save(b"a.jpg", b"two", 90, &Ok(vec![face(0)]))
        .unwrap();
    let old: (bool, bool, i64) = store
        .connection
        .query_row(
            "SELECT active,review,person_id FROM faces WHERE id=?1",
            [ids[0]],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(old, (false, true, person));
    assert!(
        store
            .edit(Edit::Assign {
                face_ids: vec![ids[0]],
                person_id: Some(person),
                name: None
            })
            .is_err()
    );
    let read = Store::at(&path, false).unwrap().unwrap();
    assert_eq!(read.records().unwrap()[b"a.jpg".as_slice()].rotation, 90);
}
#[test]
fn photos_timeline_deduplicate_filter_paginate_move_and_reject_stale_results() {
    let _session = TEST_SESSION.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("album");
    fs::create_dir(&root).unwrap();
    for i in 0..30 {
        RgbImage::new(2, 2)
            .save(root.join(format!("20240101_000000-{i:03}.png")))
            .unwrap();
    }
    RgbImage::new(2, 2).save(root.join("unknown.png")).unwrap();
    let id = open(&root);
    let ids = save(id);
    let scan = scan(id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    store
        .edit(Edit::Assign {
            face_ids: ids.clone(),
            person_id: None,
            name: Some("家人".into()),
        })
        .unwrap();
    let people = query::people(id, "家人", 0, Default::default()).unwrap();
    let p = people.items[0].id;
    assert_eq!(people.items[0].photo_count, 31);
    assert_eq!(people.items[0].face_count, 62);
    let page = query::photos(id, p, &all(), Sort::DateAsc, 0).unwrap();
    assert_eq!(page.total, 31);
    assert_eq!(page.items.len(), 24);
    assert_eq!(
        query::photos(id, p, &all(), Sort::DateAsc, 1)
            .unwrap()
            .items
            .len(),
        7
    );
    let months = query::months(id, p, &all(), Sort::DateAsc).unwrap();
    assert_eq!(months.len(), 2);
    assert_eq!(months[0].month.as_deref(), Some("2024-01"));
    assert_eq!(months[0].count, 30);
    assert_eq!(months[1].count, 1);
    let mut filter = all();
    filter.unknown_date = true;
    assert_eq!(
        query::photos(id, p, &filter, Sort::DateAsc, 0)
            .unwrap()
            .total,
        1
    );
    filter.unknown_date = false;
    filter.from = Some("2025-01-01".into());
    assert_eq!(
        query::photos(id, p, &filter, Sort::DateAsc, 0)
            .unwrap()
            .total,
        0
    );
    filter.to = Some("2020-01-01".into());
    assert!(query::photos(id, p, &filter, Sort::DateAsc, 0).is_err());
    let chosen = page.items[0].id;
    current(&*session().unwrap(), id).unwrap();
    session()
        .unwrap()
        .as_mut()
        .unwrap()
        .favorites
        .insert(chosen);
    let mut fav = all();
    fav.favorites_only = true;
    assert_eq!(
        query::photos(id, p, &fav, Sort::DateAsc, 0).unwrap().total,
        1
    );
    session()
        .unwrap()
        .as_mut()
        .unwrap()
        .set_rotation(id, chosen, 90)
        .unwrap();
    assert_eq!(
        query::photos(id, p, &all(), Sort::DateAsc, 0)
            .unwrap()
            .total,
        30
    );
    assert_eq!(
        query::faces(id, FaceScope::Pending, None, None, 0)
            .unwrap()
            .items
            .iter()
            .filter(|f| f.needs_review)
            .count(),
        2
    );
    let item = scan.item(chosen).unwrap();
    store
        .save(
            &favorites::path_key(&item.file.relative),
            &item.file.stamp.portable_key(),
            90,
            &Ok(vec![face(0)]),
        )
        .unwrap();
    assert!(query::counts(id).unwrap().0 >= 3);
    assert_eq!(
        query::counts(id).unwrap().0,
        query::faces(id, FaceScope::Pending, None, None, 0)
            .unwrap()
            .total
    );
    drop(store);
    drop(scan);
    // Moving an album requires closing the app, including the cached reader.
    query::close_for_test();
    *session().unwrap() = None;
    let moved = temp.path().join("moved");
    fs::rename(&root, &moved).unwrap();
    let fresh = open(&moved);
    assert!(query::people(id, "", 0, Default::default()).is_err());
    assert_eq!(
        query::people(fresh, "家人", 0, Default::default())
            .unwrap()
            .items[0]
            .id,
        p
    );
    assert_eq!(
        query::photos(fresh, p, &all(), Sort::DateAsc, 0)
            .unwrap()
            .total,
        30
    );
    fs::remove_file(moved.join("unknown.png")).unwrap();
    let fresh = open(&moved);
    assert_eq!(
        query::photos(fresh, p, &all(), Sort::DateAsc, 0)
            .unwrap()
            .total,
        29
    );
    *session().unwrap() = None;
}
#[cfg(unix)]
#[test]
fn refuses_symlinked_database_and_reads_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("original");
    let target = temp.path().join("link");
    let mut s = Store::at(&source, true).unwrap().unwrap();
    s.save(b"a", b"s", 0, &Ok(vec![face(0)])).unwrap();
    drop(s);
    std::os::unix::fs::symlink(&source, &target).unwrap();
    assert!(Store::at(&target, false).is_err());
    assert!(Store::at(&target, true).is_err());
    let before = fs::read(&source).unwrap();
    let s = Store::at(&source, false).unwrap().unwrap();
    assert_eq!(s.records().unwrap().len(), 1);
    assert!(s.connection.execute("DELETE FROM faces", []).is_err());
    drop(s);
    assert_eq!(before, fs::read(&source).unwrap());
}

#[test]
fn fixed_anchors_avoid_transitive_merges_and_respect_manual_decisions() {
    let _session = TEST_SESSION.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    for i in 0..4 {
        RgbImage::new(2, 2)
            .save(temp.path().join(format!("{i}.png")))
            .unwrap();
    }
    let id = open(temp.path());
    let scan = scan(id).unwrap();
    let mut s = Store::open(&scan, true).unwrap().unwrap();
    let mut p = grouping::Policy::shipped();
    p.threshold = 0.8;
    p.margin = 0.08;
    let mut groups = grouping::Groups::for_evaluation(id, p).unwrap();
    let vectors = [
        vec![1., 0.],
        vec![0.85, (1f32 - 0.85f32.powi(2)).sqrt()],
        vec![0.5, (1f32 - 0.5f32.powi(2)).sqrt()],
        vec![1., 0.],
    ];
    let mut ids = Vec::new();
    for (i, v) in vectors.iter().enumerate() {
        let item = &scan.items[i];
        let mut f = face(0);
        let mut full = vec![0.; faces::DIM];
        full[..2].copy_from_slice(v);
        f.vector = Some(full);
        let ids_for_photo = s
            .save(
                &favorites::path_key(&item.file.relative),
                &item.file.stamp.portable_key(),
                0,
                &Ok(vec![f]),
            )
            .unwrap();
        groups.assign(&mut s, &ids_for_photo).unwrap();
        ids.push(ids_for_photo[0]);
    }
    let person = |id| {
        s.connection
            .query_row("SELECT person_id FROM faces WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
    };
    let a = person(ids[0]);
    assert_eq!(person(ids[1]), a);
    assert_ne!(person(ids[2]), a);
    assert_eq!(person(ids[3]), a);
    s.edit(Edit::Ignore {
        face_ids: vec![ids[3]],
    })
    .unwrap();
    groups.assign(&mut s, &[ids[3]]).unwrap();
    assert!(
        s.connection
            .query_row("SELECT ignored FROM faces WHERE id=?1", [ids[3]], |r| r
                .get::<_, bool>(0))
            .unwrap()
    );
    *session().unwrap() = None;
}

// The active model is process-wide; callers hold TEST_SESSION and restore it on exit.
struct RestorePeopleModel(&'static ai::catalog::ModelSpec);
impl Drop for RestorePeopleModel {
    fn drop(&mut self) {
        ai::catalog::select(self.0);
        *session().unwrap() = None;
    }
}
fn model_face(spec: &ai::catalog::ModelSpec, components: &[f32]) -> Face {
    let mut f = face(0);
    let mut v = vec![0.; spec.dimension];
    v[..components.len()].copy_from_slice(components);
    f.vector = Some(v);
    f
}
fn assigned(store: &Store, id: i64) -> Option<i64> {
    store
        .connection
        .query_row("SELECT person_id FROM faces WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn avatar_search_filters_candidates_and_moves_only_the_confirmed_faces() {
    let _session = TEST_SESSION.lock().unwrap();
    let _restore = RestorePeopleModel(ai::catalog::active(ai::ModelKind::People));
    let spec = ai::catalog::default_model(ai::ModelKind::People);
    ai::catalog::select(spec);
    let album = tempfile::tempdir().unwrap();
    for n in 0..10 {
        RgbImage::new(2, 2)
            .save(album.path().join(format!("{n}.png")))
            .unwrap();
    }
    let id = open(album.path());
    let scan = scan(id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    let mut ids = Vec::new();
    for (item, similarities) in scan.items.iter().zip([
        vec![1., 1.],
        vec![0.90],
        vec![0.80],
        vec![0.64],
        vec![1.],
        vec![1.],
        vec![0.62],
        vec![1.],
        vec![0.7, 0.7],
        vec![0.2],
    ]) {
        let found = similarities
            .into_iter()
            .map(|score: f32| model_face(spec, &[score, (1. - score * score).sqrt()]))
            .collect();
        ids.push(
            store
                .save(
                    &favorites::path_key(&item.file.relative),
                    &item.file.stamp.portable_key(),
                    0,
                    &Ok(found),
                )
                .unwrap(),
        );
    }
    for (faces, name) in [
        (vec![ids[0][0]], "目标"),
        (vec![ids[2][0], ids[9][0]], "另一个分组"),
        (vec![ids[6][0]], "已忽略人物"),
    ] {
        store
            .edit(Edit::Assign {
                face_ids: faces,
                person_id: None,
                name: Some(name.into()),
            })
            .unwrap();
    }
    let target = assigned(&store, ids[0][0]).unwrap();
    let other = assigned(&store, ids[2][0]).unwrap();
    store
        .edit(Edit::Ignore {
            face_ids: vec![ids[5][0]],
        })
        .unwrap();
    store
        .connection
        .execute(
            "UPDATE people SET ignored=1 WHERE id=?1",
            [assigned(&store, ids[6][0]).unwrap()],
        )
        .unwrap();
    store
        .connection
        .pragma_update(None, "user_version", 5)
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO rejections VALUES(?1,?2)",
            params![ids[4][0], target],
        )
        .unwrap();
    store
        .connection
        .execute(
            "UPDATE media SET version='old' WHERE path=(SELECT path FROM faces WHERE id=?1)",
            [ids[7][0]],
        )
        .unwrap();
    drop(store);

    let target_view = query::people(id, "目标", 0, Default::default())
        .unwrap()
        .items
        .remove(0);
    assert_eq!(target_view.avatar_face_id, Some(ids[0][0]));
    let mut criteria = similarity::Criteria {
        target_person_id: target,
        anchor_face_id: ids[0][0],
        threshold: 0.85,
    };
    let search = |criteria| tauri::async_runtime::block_on(people_similar_faces(id, criteria, 0));
    let results = search(criteria).unwrap();
    assert_eq!(results.faces.total, 1);
    assert_eq!(results.faces.items[0].face_id, ids[1][0]);

    criteria.threshold = 0.60;
    let results = search(criteria).unwrap();
    let found: Vec<_> = results
        .faces
        .items
        .iter()
        .map(|face| face.face_id)
        .collect();
    assert_eq!(
        found,
        [
            ids[1][0], ids[2][0], ids[8][0], ids[8][1], ids[3][0], ids[6][0]
        ]
    );
    assert_eq!(results.faces.items[1].source_person_id, Some(other));
    assert_eq!(results.faces.items[5].source_person_id, None);
    assert!((results.faces.items[1].score - 0.8).abs() < 0.001);
    assert!(
        query::with(id, |store| store
            .unwrap()
            .connection
            .is_readonly("main")
            .map_err(error))
        .unwrap()
    );

    let confirm = |positions: &[usize]| {
        let selected = positions
            .iter()
            .map(|&position| {
                let face = &results.faces.items[position];
                similarity::Selection {
                    face_id: face.face_id,
                    token: face.token.clone(),
                }
            })
            .collect();
        tauri::async_runtime::block_on(people_confirm_similar_faces(
            id,
            criteria,
            results.anchor_token.clone(),
            selected,
        ))
    };
    assert!(confirm(&[2, 3]).is_err());
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    store
        .edit(Edit::Unassign {
            face_ids: vec![ids[2][0]],
        })
        .unwrap();
    assert!(confirm(&[0, 1]).is_err());
    assert_eq!(assigned(&store, ids[1][0]), None);
    store.undo().unwrap();

    confirm(&[0, 1]).unwrap();
    assert_eq!(assigned(&store, ids[1][0]), Some(target));
    assert_eq!(assigned(&store, ids[2][0]), Some(target));
    assert_eq!(assigned(&store, ids[9][0]), Some(other));
    assert_eq!(assigned(&store, ids[3][0]), None);
    assert!(
        store
            .connection
            .query_row("SELECT manual FROM faces WHERE id=?1", [ids[1][0]], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap()
    );
    assert_eq!(
        query::photos(id, target, &all(), Sort::DateAsc, 0)
            .unwrap()
            .total,
        3
    );
    tauri::async_runtime::block_on(people_undo(id)).unwrap();
    assert_eq!(
        query::photos(id, target, &all(), Sort::DateAsc, 0)
            .unwrap()
            .total,
        1
    );
    assert_eq!(assigned(&store, ids[2][0]), Some(other));

    #[cfg(windows)]
    {
        let path = album.path().join(".photosys/people.sqlite");
        let original = fs::metadata(&path).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let visible = search(criteria);
        let write = confirm(&[0]);
        fs::set_permissions(&path, original).unwrap();
        assert_eq!(visible.unwrap().faces.total, results.faces.total);
        assert!(write.is_err());
        assert_eq!(assigned(&store, ids[1][0]), None);
    }

    session()
        .unwrap()
        .as_mut()
        .unwrap()
        .set_rotation(id, results.faces.items[0].file_id, 90)
        .unwrap();
    assert!(confirm(&[0]).is_err());
    assert!(
        search(criteria)
            .unwrap()
            .faces
            .items
            .iter()
            .all(|face| face.face_id != ids[1][0])
    );

    let alternative = ai::catalog::find(ai::ModelKind::People, "yunet-auraface-fp32-v1").unwrap();
    ai::catalog::select(alternative);
    assert!(confirm(&[1]).is_err());
    assert!(search(criteria).is_err());
    ai::catalog::select(spec);

    let reopened = open(album.path());
    assert_ne!(id, reopened);
    let old = &results.faces.items[1];
    assert!(
        tauri::async_runtime::block_on(people_confirm_similar_faces(
            reopened,
            criteria,
            results.anchor_token.clone(),
            vec![similarity::Selection {
                face_id: old.face_id,
                token: old.token.clone()
            }],
        ))
        .is_err()
    );
    assert_eq!(assigned(&store, ids[2][0]), Some(other));

    let second_album = tempfile::tempdir().unwrap();
    open(second_album.path());
    assert!(confirm(&[1]).is_err());
}

#[test]
fn important_people_are_prioritized_and_survive_migration_merge_and_undo() {
    let _session = TEST_SESSION.lock().unwrap();
    let album = tempfile::tempdir().unwrap();
    RgbImage::new(2, 2)
        .save(album.path().join("portrait.png"))
        .unwrap();
    let id = open(album.path());
    let ids = save(id);
    let scan = scan(id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    for (face_id, name) in [(ids[0], "A"), (ids[1], "Z")] {
        store
            .edit(Edit::Assign {
                face_ids: vec![face_id],
                person_id: None,
                name: Some(name.into()),
            })
            .unwrap();
    }
    let first = assigned(&store, ids[0]).unwrap();
    let second = assigned(&store, ids[1]).unwrap();
    store
        .connection
        .execute_batch("ALTER TABLE people DROP COLUMN important; PRAGMA user_version=3;")
        .unwrap();
    drop(store);
    let list = || {
        let filter = serde_json::from_value(serde_json::json!({ "importantFirst": true })).unwrap();
        query::people(id, "", 0, filter).unwrap().items
    };
    assert_eq!(list()[0].id, first);
    assert!(!list()[1].important);
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    store
        .edit(Edit::SetImportant {
            person_id: second,
            important: true,
        })
        .unwrap();
    drop(store);
    query::close_for_test();
    assert_eq!(list()[0].id, second);
    assert!(list()[0].important);
    let exact = serde_json::from_value(serde_json::json!({ "personId": first })).unwrap();
    assert_eq!(query::people(id, "", 0, exact).unwrap().items[0].id, first);

    let mut store = Store::open(&scan, true).unwrap().unwrap();
    store
        .edit(Edit::Merge {
            source_id: second,
            target_id: first,
        })
        .unwrap();
    assert_eq!(list().len(), 1);
    assert!(list()[0].important);
    store.undo().unwrap();
    assert_eq!(list()[0].id, second);
    assert!(!list()[1].important);
    store.undo().unwrap();
    assert_eq!(list()[0].id, first);
    assert!(!list()[1].important);
    *session().unwrap() = None;
}

fn suggested_people(scan_id: u64, ids: &[i64]) -> std::collections::HashMap<i64, Vec<i64>> {
    let result =
        tauri::async_runtime::block_on(people_face_suggestions(scan_id, ids.to_vec())).unwrap();
    let json = serde_json::to_value(&result).unwrap();
    json["faces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|face| {
            let people = face["people"]
                .as_array()
                .unwrap()
                .iter()
                .map(|person| person["id"].as_i64().unwrap())
                .collect();
            (face["faceId"].as_i64().unwrap(), people)
        })
        .collect()
}

#[test]
fn deleted_people_return_faces_to_manual_sorting_and_undo_as_one_edit() {
    let _session = TEST_SESSION.lock().unwrap();
    let _restore = RestorePeopleModel(ai::catalog::active(ai::ModelKind::People));
    let spec = ai::catalog::default_model(ai::ModelKind::People);
    ai::catalog::select(spec);
    let album = tempfile::tempdir().unwrap();
    for index in 0..5 {
        RgbImage::new(2, 2)
            .save(album.path().join(format!("{index}.png")))
            .unwrap();
    }
    let id = open(album.path());
    let scan = scan(id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    let ids: Vec<_> = scan
        .items
        .iter()
        .map(|item| {
            store
                .save_for_model(
                    &favorites::path_key(&item.file.relative),
                    &item.file.stamp.portable_key(),
                    0,
                    &Ok(vec![model_face(spec, &[1.])]),
                    spec.feature_version,
                )
                .unwrap()[0]
        })
        .collect();
    let [anchor_face, automatic, ignored, review, manual]: [i64; 5] = ids.try_into().unwrap();
    for (name, face_ids) in [
        ("保留人物", vec![anchor_face]),
        ("人物甲", vec![automatic, ignored, review]),
        ("人物乙", vec![manual]),
    ] {
        store
            .edit(Edit::Assign {
                face_ids,
                person_id: None,
                name: Some(name.into()),
            })
            .unwrap();
    }
    let anchor = assigned(&store, anchor_face).unwrap();
    let first = assigned(&store, automatic).unwrap();
    let second = assigned(&store, manual).unwrap();
    store
        .edit(Edit::SetImportant {
            person_id: first,
            important: true,
        })
        .unwrap();
    store
        .edit(Edit::Ignore {
            face_ids: vec![ignored],
        })
        .unwrap();
    store
        .connection
        .execute("UPDATE faces SET manual=0 WHERE id=?1", [automatic])
        .unwrap();
    store
        .connection
        .execute("UPDATE faces SET active=0,review=1 WHERE id=?1", [review])
        .unwrap();

    let edit = |edit, version| tauri::async_runtime::block_on(people_edit(id, edit, version));
    let delete = |person_ids| edit(Edit::DeletePeople { person_ids }, None);
    assert!(delete(vec![first, i64::MAX]).is_err());
    assert_eq!(assigned(&store, automatic), Some(first));
    assert_eq!(
        status(id).unwrap().undo_description.as_deref(),
        Some("忽略人脸或旧标注")
    );

    delete(vec![first, second]).unwrap();
    assert!(delete(vec![first]).is_err());
    let state = status(id).unwrap();
    assert_eq!(state.pending_faces, 3);
    assert_eq!(state.undo_description.as_deref(), Some("删除人物"));
    let people = query::people(id, "", 0, Default::default()).unwrap();
    assert_eq!(people.items.len(), 1);
    assert_eq!(people.items[0].id, anchor);
    let pending = query::faces(id, FaceScope::Pending, None, None, 0).unwrap();
    assert!(pending.items.iter().all(|face| face.person_id.is_none()));
    assert!(
        pending
            .items
            .iter()
            .any(|face| face.id == review && face.needs_review)
    );
    assert_eq!(
        query::faces(id, FaceScope::Ignored, None, None, 0)
            .unwrap()
            .items[0]
            .id,
        ignored
    );

    assert!(grouping::Groups::pending_ids(id).unwrap().is_empty());
    pending::overview(id, 0, None).unwrap();
    let mut groups = grouping::Groups::load(id, grouping::Policy::shipped()).unwrap();
    groups.assign(&mut store, &[automatic, manual]).unwrap();
    assert_eq!(assigned(&store, automatic), None);
    assert_eq!(assigned(&store, manual), None);

    assert_eq!(suggested_people(id, &[automatic])[&automatic], [anchor]);
    edit(
        Edit::Assign {
            face_ids: vec![automatic],
            person_id: Some(anchor),
            name: None,
        },
        Some(spec.feature_version.into()),
    )
    .unwrap();
    assert_eq!(assigned(&store, automatic), Some(anchor));
    tauri::async_runtime::block_on(people_undo(id)).unwrap();
    assert_eq!(assigned(&store, automatic), None);

    drop(store);
    query::close_for_test();
    let reopened = open(album.path());
    tauri::async_runtime::block_on(people_undo(reopened)).unwrap();
    let people = query::people(reopened, "人物", 0, Default::default()).unwrap();
    let restored = people
        .items
        .iter()
        .find(|person| person.id == first)
        .unwrap();
    assert_eq!(restored.name, "人物甲");
    assert!(restored.important);
    let store = Store::open(&library::scan(reopened).unwrap(), false)
        .unwrap()
        .unwrap();
    assert_eq!(assigned(&store, automatic), Some(first));
    assert_eq!(assigned(&store, ignored), Some(first));
    assert_eq!(assigned(&store, review), Some(first));
    assert_eq!(assigned(&store, manual), Some(second));
    let faces = query::faces(reopened, FaceScope::Person, Some(first), None, 0).unwrap();
    assert_eq!(faces.items.len(), 1);
    assert!(!faces.items[0].manual);
    assert_eq!(status(reopened).unwrap().pending_faces, 1);
    *session().unwrap() = None;
}

#[test]
fn pending_suggestions_filter_candidates_and_validate_batch_with_persistent_undo() {
    let _session = TEST_SESSION.lock().unwrap();
    let _restore = RestorePeopleModel(ai::catalog::active(ai::ModelKind::People));
    let spec = ai::catalog::default_model(ai::ModelKind::People);
    ai::catalog::select(spec);
    let album = tempfile::tempdir().unwrap();
    for i in 0..8 {
        RgbImage::new(2, 2)
            .save(album.path().join(format!("{i}.png")))
            .unwrap();
    }
    let id = open(album.path());
    let scan = scan(id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    let a = model_face(spec, &[1.]);
    let b = model_face(spec, &[0., 1.]);
    let mixed = model_face(spec, &[0.70710677, 0.70710677]);
    let mut missing = mixed.clone();
    missing.vector = None;
    let mut ids = Vec::new();
    for (item, found) in scan.items.iter().zip([
        vec![a.clone(), mixed.clone()],
        vec![b],
        vec![mixed.clone()],
        vec![mixed.clone()],
        vec![missing],
        vec![model_face(spec, &[0., 0., 1.])],
        vec![a],
        vec![mixed.clone(), mixed],
    ]) {
        ids.push(
            store
                .save_for_model(
                    &favorites::path_key(&item.file.relative),
                    &item.file.stamp.portable_key(),
                    0,
                    &Ok(found),
                    spec.feature_version,
                )
                .unwrap(),
        );
    }
    for (face_id, name) in [(ids[0][0], "人物甲"), (ids[1][0], "人物乙")] {
        store
            .edit(Edit::Assign {
                face_ids: vec![face_id],
                person_id: None,
                name: Some(name.into()),
            })
            .unwrap();
    }
    let person_a = assigned(&store, ids[0][0]).unwrap();
    let person_b = assigned(&store, ids[1][0]).unwrap();
    store
        .connection
        .execute(
            "INSERT INTO rejections VALUES(?1,?2)",
            params![ids[2][0], person_b],
        )
        .unwrap();
    let selections = [
        ids[0][1], ids[2][0], ids[3][0], ids[4][0], ids[5][0], ids[6][0],
    ];
    assert!(
        query::with(id, |store| store
            .unwrap()
            .connection
            .is_readonly("main")
            .map_err(error))
        .unwrap()
    );
    let candidates = suggested_people(id, &selections);
    assert_eq!(candidates[&ids[0][1]], [person_b]);
    assert_eq!(candidates[&ids[2][0]], [person_a]);
    assert_eq!(candidates[&ids[3][0]], [person_a, person_b]);
    assert!(!candidates.contains_key(&ids[4][0]));
    assert!(candidates[&ids[5][0]].is_empty());
    assert_eq!(candidates[&ids[6][0]], [person_a]);
    assert!(
        tauri::async_runtime::block_on(people_face_suggestions(id, vec![ids[2][0]; 25])).is_err()
    );

    let apply = |face_ids: Vec<i64>, person_id, version: &str| {
        tauri::async_runtime::block_on(people_edit(
            id,
            Edit::Assign {
                face_ids,
                person_id: Some(person_id),
                name: None,
            },
            Some(version.into()),
        ))
    };
    let version = spec.feature_version;
    assert!(apply(vec![ids[3][0], ids[2][0]], person_b, version).is_err());
    assert!(apply(ids[7].clone(), person_a, version).is_err());
    assert!(apply(vec![ids[3][0]], person_a, "old-model").is_err());
    assert!(ids[7].iter().all(|&id| assigned(&store, id).is_none()));
    assert_eq!(assigned(&store, ids[3][0]), None);

    // Simulate the background index assigning the face after the UI received its suggestions.
    let mut groups = grouping::Groups::load(id, grouping::Policy::shipped()).unwrap();
    groups.assign(&mut store, &ids[6]).unwrap();
    assert_eq!(assigned(&store, ids[6][0]), Some(person_a));
    assert!(apply(vec![ids[3][0], ids[6][0]], person_a, version).is_err());
    assert_eq!(assigned(&store, ids[3][0]), None);

    apply(vec![ids[2][0], ids[3][0]], person_a, version).unwrap();
    let confirmed = query::faces(id, FaceScope::Person, Some(person_a), None, 0).unwrap();
    for face_id in [ids[2][0], ids[3][0]] {
        assert!(
            confirmed
                .items
                .iter()
                .any(|face| face.id == face_id && face.manual)
        );
    }
    drop(store);
    query::close_for_test();
    let reopened = open(album.path());
    tauri::async_runtime::block_on(people_undo(reopened)).unwrap();
    let store = Store::open(&library::scan(reopened).unwrap(), false)
        .unwrap()
        .unwrap();
    assert_eq!(assigned(&store, ids[2][0]), None);
    assert_eq!(assigned(&store, ids[3][0]), None);
    assert_eq!(
        suggested_people(reopened, &[ids[2][0]])[&ids[2][0]],
        [person_a]
    );
}

#[test]
fn conservative_grouping_for_both_models_reuses_features_and_keeps_uncertain_faces_pending() {
    let _session = TEST_SESSION.lock().unwrap();
    let _restore = RestorePeopleModel(ai::catalog::active(ai::ModelKind::People));
    for model in ["yunet-sface-fp32-v1", "yunet-auraface-fp32-v1"] {
        let spec = ai::catalog::find(ai::ModelKind::People, model).unwrap();
        ai::catalog::select(spec);
        let album = tempfile::tempdir().unwrap();
        for i in 0..8 {
            RgbImage::new(2, 2)
                .save(album.path().join(format!("{i}.png")))
                .unwrap();
        }
        let id = open(album.path());
        let scan = scan(id).unwrap();
        let mut store = Store::open(&scan, true).unwrap().unwrap();
        let mut small = model_face(spec, &[1.]);
        small.vector = None;
        let mut malformed = model_face(spec, &[1.]);
        malformed.vector = Some(vec![1.]);
        let found = [
            model_face(spec, &[1.]),
            model_face(spec, &[0.9, 0., 0.4358899]),
            model_face(spec, &[0., 1.]),
            model_face(spec, &[0.70710677, 0.70710677]),
            small,
            malformed,
            model_face(spec, &[f32::NAN]),
            model_face(spec, &[1.]),
        ];
        let mut ids = Vec::new();
        for (i, (item, f)) in scan.items.iter().zip(found).enumerate() {
            let mut faces = vec![f.clone()];
            if i == 7 {
                faces.push(f);
            }
            ids.extend(
                store
                    .save_for_model(
                        &favorites::path_key(&item.file.relative),
                        &item.file.stamp.portable_key(),
                        0,
                        &Ok(faces),
                        spec.feature_version,
                    )
                    .unwrap(),
            );
        }
        // These were indexed before grouping was enabled. Only cached vectors are read.
        let before = store.records().unwrap();
        let mut groups = grouping::Groups::load(id, grouping::Policy::shipped()).unwrap();
        groups
            .assign(&mut store, &grouping::Groups::pending_ids(id).unwrap())
            .unwrap();
        let a = assigned(&store, ids[0]).unwrap();
        assert_eq!(assigned(&store, ids[1]), Some(a));
        let b = assigned(&store, ids[2]).unwrap();
        assert_ne!(a, b);
        for index in [3, 4, 5, 6, 8] {
            assert_eq!(assigned(&store, ids[index]), None);
        }
        assert_eq!(assigned(&store, ids[7]), Some(a));
        assert!(status(id).unwrap().auto_grouping);
        assert_eq!(status(id).unwrap().pending_faces, 5);
        assert_eq!(status(id).unwrap().indexed, 8);
        assert!(
            query::faces(id, FaceScope::Person, Some(a), None, 0)
                .unwrap()
                .items
                .iter()
                .all(|f| !f.manual)
        );
        // Simulate stopping/reopening and continuing: no duplicate groups or inference writes.
        let mut groups = grouping::Groups::load(id, grouping::Policy::shipped()).unwrap();
        groups
            .assign(&mut store, &grouping::Groups::pending_ids(id).unwrap())
            .unwrap();
        assert_eq!(
            query::people(id, "", 0, Default::default()).unwrap().total,
            2
        );
        for (path, record) in before {
            let after = store.records().unwrap();
            assert_eq!(record.stamp, after[&path].stamp);
            assert_eq!(record.version, after[&path].version);
        }
    }
}

#[test]
fn pending_groups_are_temporary_and_confirm_across_pages_for_both_models() {
    let _session = TEST_SESSION.lock().unwrap();
    let _restore = RestorePeopleModel(ai::catalog::active(ai::ModelKind::People));
    for model in ["yunet-sface-fp32-v1", "yunet-auraface-fp32-v1"] {
        let spec = ai::catalog::find(ai::ModelKind::People, model).unwrap();
        ai::catalog::select(spec);
        let album = tempfile::tempdir().unwrap();
        for i in 0..28 {
            RgbImage::new(2, 2)
                .save(album.path().join(format!("{i:02}.png")))
                .unwrap();
        }
        let id = open(album.path());
        let scan = scan(id).unwrap();
        let mut store = Store::open(&scan, true).unwrap().unwrap();
        let mut ids = Vec::new();
        for (i, item) in scan.items.iter().enumerate() {
            let detections = match i {
                0 => vec![model_face(spec, &[1.]), model_face(spec, &[1.])],
                27 => vec![model_face(spec, &[0., 1.])],
                _ => vec![model_face(spec, &[1., 0.2])],
            };
            ids.push(
                store
                    .save_for_model(
                        &favorites::path_key(&item.file.relative),
                        &item.file.stamp.portable_key(),
                        0,
                        &Ok(detections),
                        spec.feature_version,
                    )
                    .unwrap(),
            );
        }
        assert_eq!(
            query::people(id, "", 0, Default::default()).unwrap().total,
            0
        );
        let overview = || serde_json::to_value(pending::overview(id, 0, None).unwrap()).unwrap();
        let view = |snapshot: &serde_json::Value| pending::View {
            token: snapshot["token"].as_str().unwrap().into(),
            group_id: snapshot["groups"]["items"][0]["id"].as_i64(),
        };
        let initial = overview();
        assert_eq!(initial["groups"]["total"], 1);
        assert_eq!(initial["groups"]["items"][0]["count"], 27);
        assert_eq!(initial["ungroupedCount"], 2);
        assert_eq!(
            query::pending_faces(id, 0, &view(&initial))
                .unwrap()
                .items
                .len(),
            24
        );
        let next_page = query::pending_faces(id, 1, &view(&initial)).unwrap();
        assert_eq!(next_page.items.len(), 3);
        let excluded = next_page.items[0].id;

        let parameters = |threshold, margin| {
            serde_json::from_value(serde_json::json!({"threshold": threshold, "margin": margin}))
                .unwrap()
        };
        save_grouping_parameters(id, spec.feature_version, parameters(0.995, 0.0)).unwrap();
        let changed = overview();
        assert_ne!(changed["token"], initial["token"]);
        assert_eq!(changed["groups"]["items"][0]["count"], 26);
        assert!(
            edit_pending_group(
                id,
                view(&initial),
                pending::Selection::All {
                    excluded_face_ids: vec![],
                },
                None,
                None,
                false
            )
            .is_err()
        );
        save_grouping_parameters(id, spec.feature_version, parameters(0.995, 0.08)).unwrap();
        assert_eq!(overview()["groups"]["total"], 0);
        save_grouping_parameters(id, spec.feature_version, parameters(0.65, 0.08)).unwrap();
        let current = overview();
        edit_pending_group(
            id,
            view(&current),
            pending::Selection::All {
                excluded_face_ids: vec![excluded],
            },
            None,
            Some("家人".into()),
            false,
        )
        .unwrap();
        let person = query::people(id, "家人", 0, Default::default())
            .unwrap()
            .items
            .remove(0);
        assert_eq!(person.photo_count, 26);
        assert_eq!(assigned(&store, excluded), None);
        assert_eq!(assigned(&store, ids[0][1]), None);
        assert!(
            store
                .edit(Edit::Assign {
                    face_ids: vec![ids[0][0]],
                    person_id: None,
                    name: None,
                })
                .is_err()
        );
        store
            .edit(Edit::Unassign {
                face_ids: vec![ids[0][0]],
            })
            .unwrap();
        assert_eq!(assigned(&store, ids[0][0]), None);
        store.undo().unwrap();
        assert_eq!(assigned(&store, ids[0][0]), Some(person.id));
        store.undo().unwrap();
        assert_eq!(
            query::people(id, "", 0, Default::default()).unwrap().total,
            0
        );
        assert_eq!(status(id).unwrap().pending_faces, 29);

        let original = view(&overview());
        let original_group_id = original.group_id.unwrap();
        assert_eq!(original_group_id, ids[0][0]);
        let ignored = vec![ids[0][0], ids[1][0], ids[2][0], ids[3][0]];
        let remaining = edit_pending_group(
            id,
            original,
            pending::Selection::Selected {
                face_ids: ignored.clone(),
            },
            None,
            None,
            true,
        )
        .unwrap();
        assert_eq!(remaining.len(), 23);
        assert!(remaining.contains(&excluded));
        assert!(ignored.iter().all(|id| !remaining.contains(id)));
        let refreshed = serde_json::to_value(
            pending::overview(
                id,
                0,
                Some(&pending::ResumeGroup {
                    group_id: original_group_id,
                    face_ids: remaining,
                }),
            )
            .unwrap(),
        )
        .unwrap();
        let resumed_id = refreshed["resumedGroupId"].as_i64().unwrap();
        assert_ne!(resumed_id, original_group_id);
        let resumed = pending::View {
            token: refreshed["token"].as_str().unwrap().into(),
            group_id: Some(resumed_id),
        };
        let faces = query::pending_faces(id, 1, &resumed).unwrap();
        assert_eq!(faces.page, 0);
        assert!(faces.items.iter().any(|face| face.id == excluded));
        assert!(faces.items.iter().all(|face| !ignored.contains(&face.id)));
        let remaining = edit_pending_group(
            id,
            resumed,
            pending::Selection::All {
                excluded_face_ids: vec![],
            },
            None,
            None,
            true,
        )
        .unwrap();
        assert!(remaining.is_empty());
        let refreshed = serde_json::to_value(
            pending::overview(
                id,
                0,
                Some(&pending::ResumeGroup {
                    group_id: resumed_id,
                    face_ids: remaining,
                }),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(refreshed["resumedGroupId"].is_null());
        store.undo().unwrap();
        store.undo().unwrap();

        store
            .edit(Edit::Assign {
                face_ids: ids[27].clone(),
                person_id: None,
                name: None,
            })
            .unwrap();
        let target = assigned(&store, ids[27][0]).unwrap();
        let current = overview();
        edit_pending_group(
            id,
            view(&current),
            pending::Selection::Selected {
                face_ids: vec![ids[0][0], excluded],
            },
            Some(target),
            None,
            false,
        )
        .unwrap();
        assert_eq!(assigned(&store, excluded), Some(target));
        store.undo().unwrap();
        assert_eq!(assigned(&store, excluded), None);
    }
}

#[test]
fn legacy_auto_only_groups_are_filtered_and_confirmed_edits_survive_migration() {
    let _session = TEST_SESSION.lock().unwrap();
    let album = tempfile::tempdir().unwrap();
    for i in 0..4 {
        RgbImage::new(2, 2)
            .save(album.path().join(format!("{i}.png")))
            .unwrap();
    }
    let id = open(album.path());
    let ids = save(id);
    let scan = scan(id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    store
        .edit(Edit::Assign {
            face_ids: vec![ids[0]],
            person_id: None,
            name: Some("妈妈".into()),
        })
        .unwrap();
    let person = assigned(&store, ids[0]).unwrap();
    let c = &store.connection;
    c.execute("INSERT INTO people DEFAULT VALUES", []).unwrap();
    let auto = c.last_insert_rowid();
    c.execute("INSERT INTO people(important) VALUES(1)", [])
        .unwrap();
    let important = c.last_insert_rowid();
    for (face, owner) in [
        (ids[2], person),
        (ids[4], person),
        (ids[6], auto),
        (ids[7], auto),
    ] {
        c.execute(
            "UPDATE faces SET person_id=?2 WHERE id=?1",
            params![face, owner],
        )
        .unwrap();
    }
    c.execute("INSERT INTO edits(description,payload) VALUES('补分待整理人脸',?1)", [
        serde_json::json!({"faces":[],"people":[],"created":[],"pending_assignments":[[ids[4], person]]}).to_string(),
    ]).unwrap();
    store
        .edit(Edit::Ignore {
            face_ids: vec![ids[7]],
        })
        .unwrap();
    store
        .connection
        .pragma_update(None, "user_version", 4)
        .unwrap();
    drop(store);

    let read = Store::open(&scan, false).unwrap().unwrap();
    let assert_projection = || {
        let items = query::people(id, "", 0, Default::default()).unwrap().items;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, person);
        assert_eq!(items[0].photo_count, 2);
        assert_eq!(items[1].id, important);
        let pending = query::faces(id, FaceScope::Pending, None, None, 0).unwrap();
        assert!(pending.items.iter().any(|face| face.id == ids[2]));
        assert!(pending.items.iter().any(|face| face.id == ids[6]));
        let ignored = query::faces(id, FaceScope::Ignored, None, None, 0).unwrap();
        assert_eq!(ignored.items[0].id, ids[7]);
        assert_eq!(ignored.items[0].person_id, None);
    };
    assert_projection();
    assert_eq!(read.schema_version().unwrap(), 4);
    assert_eq!(assigned(&read, ids[6]), Some(auto));
    drop(read);

    let mut store = Store::open(&scan, true).unwrap().unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_projection();
    assert_eq!(assigned(&store, ids[2]), None);
    assert_eq!(assigned(&store, ids[4]), Some(person));
    assert_eq!(assigned(&store, ids[6]), None);
    store.undo().unwrap();
    assert_eq!(assigned(&store, ids[7]), None);
    store.undo().unwrap();
    assert_eq!(assigned(&store, ids[4]), None);
    assert_eq!(assigned(&store, ids[0]), Some(person));
}

#[test]
#[ignore = "requires real models, an OpenCV test portrait, and a Windows hardware GPU"]
fn real_directml_people_reuses_cpu_index_and_manual_identity() {
    use ai::device_tests::{assert_directml_profiles, benchmark_indexes, directml_test_device};
    let _session = TEST_SESSION.lock().unwrap();
    let models = std::path::PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_DIR").unwrap());
    let portrait =
        std::path::PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_PORTRAIT").unwrap());
    let spec = ai::catalog::default_model(ai::ModelKind::People);
    let model = models.join(spec.id);
    let profiles = tempfile::tempdir().unwrap();
    let device = directml_test_device(profiles.path());
    let cpu = Arc::new(FaceRuntime::load_spec(&model, spec).unwrap());
    let album = tempfile::tempdir().unwrap();
    fs::copy(&portrait, album.path().join("a.jpg")).unwrap();
    let id = open(album.path());
    let progress = Channel::new(|_| Ok(()));
    assert_eq!(
        update(id, false, &progress, || Ok(cpu.clone()))
            .unwrap()
            .indexed,
        1
    );
    let person = query::people(id, "", 0, Default::default()).unwrap().items[0].id;
    let face = query::faces(id, FaceScope::Person, Some(person), None, 0)
        .unwrap()
        .items[0]
        .id;
    Store::open(&scan(id).unwrap(), true)
        .unwrap()
        .unwrap()
        .edit(Edit::Assign {
            face_ids: vec![face],
            person_id: None,
            name: Some("样例人物".into()),
        })
        .unwrap();
    let named = query::people(id, "样例人物", 0, Default::default())
        .unwrap()
        .items[0]
        .id;

    let gpu = Arc::new(FaceRuntime::load_with_device(&model, spec, &device).unwrap());
    fs::copy(&portrait, album.path().join("b.jpg")).unwrap();
    let id = open(album.path());
    let added = update(id, true, &progress, || Ok(gpu.clone())).unwrap();
    assert_eq!((added.indexed, added.reused, added.failed), (2, 1, 0));
    assert!(!added.needs_rebuild);
    let same = update(id, true, &progress, || Ok(cpu.clone())).unwrap();
    assert_eq!(same.reused, 2);
    let person = &query::people(id, "样例人物", 0, Default::default())
        .unwrap()
        .items[0];
    assert_eq!(person.id, named);
    assert_eq!(person.photo_count, 2);
    assert!(
        query::faces(id, FaceScope::Person, Some(named), None, 0)
            .unwrap()
            .items
            .iter()
            .any(|entry| entry.id == face)
    );
    benchmark_indexes(&portrait, |path, use_gpu| {
        let runtime = if use_gpu { &gpu } else { &cpu };
        let result = update(open(path), false, &progress, || Ok(runtime.clone())).unwrap();
        (result.indexed, result.failed)
    });
    assert_directml_profiles(gpu.end_profiles());
    *session().unwrap() = None;
}

#[test]
#[ignore = "requires PHOTOSYS_PEOPLE_MODEL_DIR and PHOTOSYS_PEOPLE_PACKAGE"]
fn real_faces_install_incremental_cancel_and_restore() {
    let _session = TEST_SESSION.lock().unwrap();
    let models = std::path::PathBuf::from(std::env::var_os("PHOTOSYS_PEOPLE_MODEL_DIR").unwrap());
    let package = std::path::PathBuf::from(std::env::var_os("PHOTOSYS_PEOPLE_PACKAGE").unwrap());
    let runtime_name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    ai::inference::initialize_from(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/onnxruntime")
            .join(std::env::consts::ARCH)
            .join(runtime_name),
    )
    .unwrap();
    let install_dir = tempfile::tempdir().unwrap();
    // Exercise exactly the shared installer used by both IPC model packages.
    let runtime = ai::people_model::test_install(install_dir.path(), &package).unwrap();
    let album = tempfile::tempdir().unwrap();
    for i in 0..3 {
        fs::copy(
            models.join("lena.jpg"),
            album.path().join(format!("{i}.jpg")),
        )
        .unwrap();
    }
    let id = open(album.path());
    let channel = Channel::new(|_| Ok(()));
    let initial = update(id, false, &channel, || Ok(runtime.clone())).unwrap();
    assert_eq!(initial.indexed, 3);
    assert!(initial.enabled);
    assert!(!initial.running);
    assert_eq!(initial.pending_faces, 3);
    assert!(initial.auto_grouping);
    assert_eq!(
        query::people(id, "", 0, Default::default()).unwrap().total,
        0
    );
    let same = update(id, true, &channel, || Ok(runtime.clone())).unwrap();
    assert_eq!(same.reused, 3);
    assert_eq!(same.pending_faces, 3);
    let overview = serde_json::to_value(pending::overview(id, 0, None).unwrap()).unwrap();
    assert_eq!(overview["groups"]["items"][0]["count"], 3);
    let f = query::faces(id, FaceScope::Pending, None, None, 0).unwrap();
    let face_id = f.items[0].id;
    let scan = scan(id).unwrap();
    let mut s = Store::open(&scan, true).unwrap().unwrap();
    s.edit(Edit::Assign {
        face_ids: vec![face_id],
        person_id: None,
        name: Some("样例人物".into()),
    })
    .unwrap();
    update(id, true, &channel, || Ok(runtime.clone())).unwrap();
    assert_eq!(
        query::people(id, "样例人物", 0, Default::default())
            .unwrap()
            .items[0]
            .photo_count,
        1
    );
    session()
        .unwrap()
        .as_mut()
        .unwrap()
        .set_rotation(id, f.items[0].file_id, 90)
        .unwrap();
    assert_eq!(status(id).unwrap().pending, 1);
    let rotated = update(id, true, &channel, || Ok(runtime.clone())).unwrap();
    assert_eq!(rotated.reused, 2);
    assert_eq!(rotated.indexed, 3);
    assert!(
        query::faces(id, FaceScope::Pending, None, None, 0)
            .unwrap()
            .items
            .iter()
            .any(|f| f.id == face_id && f.needs_review)
    );
    // A callback cancels after a committed image, proving completed records survive.
    drop(s);
    drop(scan);
    for i in 3..10 {
        fs::copy(
            models.join("lena.jpg"),
            album.path().join(format!("{i}.jpg")),
        )
        .unwrap();
    }
    let id = open(album.path());
    let cancel_channel = Channel::new(move |message| {
        if let tauri::ipc::InvokeResponseBody::Json(json) = message {
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            if v["indexed"].as_u64().unwrap_or(0) > 3 {
                people_cancel_index(id);
            }
        }
        Ok(())
    });
    let partial = update(id, false, &cancel_channel, || Ok(runtime.clone())).unwrap();
    assert!(partial.indexed >= 4 && partial.indexed < 10);
    assert!(!partial.running);
    let resumed = update(id, true, &channel, || Ok(runtime.clone())).unwrap();
    assert_eq!(resumed.indexed, 10);
    assert!(resumed.reused >= partial.indexed);
    *session().unwrap() = None;
}

#[test]
#[ignore = "synthetic 10k/100k photo catalog and 300k face scalability benchmark"]
fn benchmark_people_catalogs() {
    use std::time::Instant;
    let _session = TEST_SESSION.lock().unwrap();
    let mut reports = Vec::new();
    for count in [10_000usize, 100_000] {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::duplicates::Root::new(temp.path()).unwrap();
        let keys = favorites::initialize(&root).unwrap();
        RgbImage::new(1, 1)
            .save(temp.path().join("seed.png"))
            .unwrap();
        let file = fs::File::open(temp.path().join("seed.png")).unwrap();
        let id = library::SCAN_ID.fetch_add(1, Ordering::Relaxed);
        let mut items = Vec::with_capacity(count);
        for i in 0..count {
            let relative = std::path::PathBuf::from(format!("photo-{i:06}.jpg"));
            items.push(library::Item {
                file: crate::duplicates::Candidate {
                    id: i,
                    relative: relative.clone(),
                    stamp: crate::duplicates::FileStamp::read(&file).unwrap(),
                },
                search_path: relative.to_str().unwrap().into(),
                name_key: relative.to_str().unwrap().into(),
                kind: library::Kind::Photo,
                location: None,
                captured_on: if i % 100 == 0 {
                    None
                } else {
                    Some(
                        chrono::NaiveDate::from_ymd_opt(
                            2015 + (i % 10) as i32,
                            1 + (i % 12) as u32,
                            1 + (i % 28) as u32,
                        )
                        .unwrap(),
                    )
                },
                preview_available: true,
                is_live_photo: false,
            });
        }
        let catalog = library::Scan {
            id,
            root,
            items,
            issues: Vec::new(),
            previews: Mutex::default(),
            ai_decodes: Default::default(),
        };
        *session().unwrap() = Some(library::Session::from_scan(
            catalog,
            keys,
            std::collections::HashMap::new(),
        ));
        let scan = scan(id).unwrap();
        let mut s = Store::open(&scan, true).unwrap().unwrap();
        let started = Instant::now();
        let tx = s.connection.transaction().unwrap();
        for p in 1..=100 {
            tx.execute(
                "INSERT INTO people(id,name) VALUES(?1,?2)",
                params![p, format!("人物 {p}")],
            )
            .unwrap();
        }
        {
            let mut media = tx
                .prepare("INSERT INTO media VALUES(?1,?2,0,?3,NULL)")
                .unwrap();
            let mut faces=tx.prepare("INSERT INTO faces(path,bbox,confidence,thumbnail,person_id,manual) VALUES(?1,'[0.1,0.1,0.4,0.4]',0.99,X'01',?2,1)").unwrap();
            for item in &scan.items {
                let path = favorites::path_key(&item.file.relative);
                media
                    .execute(params![
                        path,
                        item.file.stamp.portable_key(),
                        faces::VERSION
                    ])
                    .unwrap();
                for face in 0..3 {
                    faces
                        .execute(params![path, 1 + ((item.file.id + face) % 100) as i64])
                        .unwrap();
                }
            }
        }
        tx.commit().unwrap();
        let insert_ms = started.elapsed().as_millis();
        let started = Instant::now();
        let first = query::photos(id, 1, &all(), Sort::DateAsc, 0).unwrap();
        let cold_ms = started.elapsed().as_millis();
        assert_eq!(first.total, count / 100 * 3);
        let started = Instant::now();
        let page = query::photos(id, 1, &all(), Sort::DateAsc, 5).unwrap();
        let page_ms = started.elapsed().as_millis();
        assert_eq!(page.items.len(), 24);
        assert!(page_ms < 1000, "warm pagination {page_ms}ms");
        let started = Instant::now();
        let months = query::months(id, 1, &all(), Sort::DateAsc).unwrap();
        let months_ms = started.elapsed().as_millis();
        assert!(!months.is_empty());
        let started = Instant::now();
        let persons = query::people(id, "", 0, Default::default()).unwrap();
        let people_ms = started.elapsed().as_millis();
        assert_eq!(persons.total, 100);
        let started = Instant::now();
        let state = status(id).unwrap();
        let status_ms = started.elapsed().as_millis();
        assert_eq!(state.indexed, count);
        reports.push(serde_json::json!({"photos":count,"faces":count*3,"synthetic_metadata_insert_ms":insert_ms,"cold_catalog_and_first_page_ms":cold_ms,"warm_photo_page_ms":page_ms,"months_ms":months_ms,"people_list_ms":people_ms,"incremental_status_ms":status_ms}));
        *session().unwrap() = None;
    }
    let report = serde_json::to_string_pretty(&reports).unwrap();
    println!("{report}");
    if let Some(path) = std::env::var_os("PHOTOSYS_PEOPLE_BENCHMARK_OUTPUT") {
        fs::write(path, report).unwrap();
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LabeledSample {
    path: std::path::PathBuf,
    identity: String,
    split: String,
    strata: Vec<String>,
}
#[derive(serde::Deserialize)]
struct EvaluationInput {
    samples: Vec<LabeledSample>,
}
#[test]
#[ignore = "requires independently labeled PHOTOSYS_PEOPLE_EVALUATION manifest; writes an evaluation report only"]
fn evaluate_labeled_people() {
    use sha2::{Digest, Sha256};
    use std::collections::{HashMap, HashSet};
    let _session = TEST_SESSION.lock().unwrap();
    let input = std::path::PathBuf::from(
        std::env::var_os("PHOTOSYS_PEOPLE_EVALUATION").expect("PHOTOSYS_PEOPLE_EVALUATION"),
    );
    let models = std::path::PathBuf::from(
        std::env::var_os("PHOTOSYS_PEOPLE_MODEL_DIR").expect("PHOTOSYS_PEOPLE_MODEL_DIR"),
    );
    let output = std::path::PathBuf::from(
        std::env::var_os("PHOTOSYS_PEOPLE_EVALUATION_OUTPUT")
            .expect("PHOTOSYS_PEOPLE_EVALUATION_OUTPUT"),
    );
    let input_bytes = fs::read(&input).unwrap();
    let manifest: EvaluationInput = serde_json::from_slice(&input_bytes).unwrap();
    assert!(!manifest.samples.is_empty());
    let labels = |split: &str| {
        manifest
            .samples
            .iter()
            .filter(|s| s.split == split)
            .map(|s| s.identity.as_str())
            .collect::<HashSet<_>>()
    };
    let calibration = labels("calibration");
    let validation = labels("validation");
    assert!(!calibration.is_empty() && !validation.is_empty());
    assert!(
        calibration.is_disjoint(&validation),
        "identities must be disjoint across splits"
    );
    let runtime_name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    ai::inference::initialize_from(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/onnxruntime")
            .join(std::env::consts::ARCH)
            .join(runtime_name),
    )
    .unwrap();
    let runtime = FaceRuntime::load(&models).unwrap();
    let album = tempfile::tempdir().unwrap();
    let mut labels_by_path = HashMap::new();
    let mut extracted = HashMap::new();
    let mut images = HashSet::new();
    let mut hash = Sha256::new();
    hash.update(&input_bytes);
    for (i, sample) in manifest.samples.iter().enumerate() {
        assert!(matches!(
            sample.split.as_str(),
            "calibration" | "validation"
        ));
        assert!(!sample.identity.is_empty());
        let path = if sample.path.is_absolute() {
            sample.path.clone()
        } else {
            input.parent().unwrap().join(&sample.path)
        };
        let bytes = fs::read(&path).unwrap();
        let image_hash = Sha256::digest(&bytes).to_vec();
        assert!(
            images.insert(image_hash.clone()),
            "duplicate image in evaluation"
        );
        hash.update(image_hash);
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("jpg");
        let filename = format!("sample-{i:08}.{extension}");
        fs::copy(&path, album.path().join(&filename)).unwrap();
        labels_by_path.insert(filename, i);
    }
    let fingerprint = format!("{:x}", hash.finalize());
    let id = open(album.path());
    let scan = scan(id).unwrap();
    assert_eq!(
        scan.items.len(),
        manifest.samples.len(),
        "evaluation contains unsupported or unreadable files"
    );
    for item in &scan.items {
        let image = library::ai_search::decode(&scan, item, &AtomicBool::new(false)).unwrap();
        let found = runtime.analyze(image, 0, &AtomicBool::new(false)).unwrap();
        // Each entry labels one face; skip ambiguous detections and include them in coverage.
        let usable = if found.len() == 1 && found[0].vector.is_some() {
            found
        } else {
            Vec::new()
        };
        extracted.insert(item.file.relative.to_str().unwrap().to_string(), usable);
    }
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    #[derive(Default, serde::Serialize)]
    struct Metrics {
        accepted_links: usize,
        correct_links: usize,
        total_samples: usize,
        usable_faces: usize,
        coverage: f32,
        strata: HashMap<String, usize>,
        strata_correct: HashMap<String, usize>,
    }
    fn evaluate(
        id: u64,
        scan: &library::Scan,
        store: &mut Store,
        manifest: &EvaluationInput,
        labels: &HashMap<String, usize>,
        extracted: &HashMap<String, Vec<Face>>,
        split: &str,
        threshold: f32,
    ) -> Metrics {
        store.connection.execute_batch("DELETE FROM rejections;DELETE FROM edits;DELETE FROM faces;DELETE FROM media;DELETE FROM people;").unwrap();
        let mut policy = grouping::Policy::shipped();
        policy.threshold = threshold;
        let mut groups = grouping::Groups::for_evaluation(id, policy).unwrap();
        let mut anchors: HashMap<i64, String> = HashMap::new();
        let mut metrics = Metrics::default();
        for item in &scan.items {
            let key = item.file.relative.to_str().unwrap();
            let sample = &manifest.samples[labels[key]];
            if sample.split != split {
                continue;
            }
            metrics.total_samples += 1;
            let found = &extracted[key];
            if found.is_empty() {
                continue;
            }
            metrics.usable_faces += 1;
            let ids = store
                .save(
                    &favorites::path_key(&item.file.relative),
                    &item.file.stamp.portable_key(),
                    0,
                    &Ok(found.clone()),
                )
                .unwrap();
            groups.assign(store, &ids).unwrap();
            let person: Option<i64> = store
                .connection
                .query_row("SELECT person_id FROM faces WHERE id=?1", [ids[0]], |r| {
                    r.get(0)
                })
                .unwrap();
            if let Some(person) = person {
                if let Some(label) = anchors.get(&person) {
                    metrics.accepted_links += 1;
                    let correct = *label == sample.identity;
                    if correct {
                        metrics.correct_links += 1;
                    }
                    for tag in &sample.strata {
                        *metrics.strata.entry(tag.clone()).or_default() += 1;
                        if correct {
                            *metrics.strata_correct.entry(tag.clone()).or_default() += 1;
                        }
                    }
                } else {
                    anchors.insert(person, sample.identity.clone());
                }
            }
        }
        metrics.coverage = metrics.accepted_links as f32 / metrics.total_samples.max(1) as f32;
        metrics
    }
    let mut candidates = Vec::new();
    let mut chosen = None;
    for threshold in [0.5, 0.55, 0.6, 0.65, 0.7, 0.75, 0.8, 0.85] {
        let m = evaluate(
            id,
            &scan,
            &mut store,
            &manifest,
            &labels_by_path,
            &extracted,
            "calibration",
            threshold,
        );
        let passed =
            m.accepted_links >= 100 && m.correct_links as f64 / m.accepted_links as f64 >= 0.99;
        candidates.push(serde_json::json!({"threshold":threshold,"metrics":m}));
        if passed && chosen.is_none() {
            chosen = Some(threshold);
        }
    }
    let threshold = chosen.unwrap_or(0.85);
    let m = evaluate(
        id,
        &scan,
        &mut store,
        &manifest,
        &labels_by_path,
        &extracted,
        "validation",
        threshold,
    );
    let correct = m.correct_links;
    let accepted = m.accepted_links;
    let strata_ok = ["adult", "child", "profile", "occluded", "years"]
        .iter()
        .all(|k| {
            let n = m.strata.get(*k).copied().unwrap_or(0);
            let c = m.strata_correct.get(*k).copied().unwrap_or(0);
            n >= 20 && c as f64 / n as f64 >= 0.99
        });
    let enabled = chosen.is_some()
        && accepted >= 1000
        && correct as f64 / accepted as f64 >= 0.99
        && strata_ok;
    let report = serde_json::json!({"enabled":enabled,"version":faces::VERSION,"threshold":threshold,"margin":0.08,"validation":{"pipeline":"fixed-anchors-hnsw-exact-v1","correct_links":correct,"accepted_links":accepted,"coverage":m.coverage,"strata":m.strata,"strata_correct":m.strata_correct,"dataset_sha256":fingerprint},"reason":if enabled{"独立标注验证通过。"}else{"独立标注验证未达到准确率验收门槛。运行时启用状态不由此报告决定。"},"calibration":candidates,"validation_total_samples":m.total_samples,"validation_usable_faces":m.usable_faces});
    fs::write(&output, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    println!(
        "release gate passed={enabled}; accepted={accepted}; correct={correct}; report={}",
        output.display()
    );
    *session().unwrap() = None;
}

#[test]
fn model_change_keeps_unique_faces_manual_corrections_ignored_and_undo() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("people.sqlite");
    let mut store = Store::at(&path, true).unwrap().unwrap();
    let mut second = face(1);
    second.bbox[0] = 0.6;
    let old = store
        .save(
            b"family.jpg",
            b"same",
            0,
            &Ok(vec![face(0), second.clone()]),
        )
        .unwrap();
    store
        .edit(Edit::Assign {
            face_ids: vec![old[0]],
            person_id: None,
            name: Some("妈妈".into()),
        })
        .unwrap();
    store
        .edit(Edit::Ignore {
            face_ids: vec![old[1]],
        })
        .unwrap();
    let mut new = face(0);
    new.vector = Some(vec![1.; 512]);
    second.vector = Some(vec![1.; 512]);
    let version = "yunet640-auraface112-align5-v1";
    let ids = store
        .save_for_model(
            b"family.jpg",
            b"same",
            0,
            &Ok(vec![second.clone(), new.clone()]),
            version,
        )
        .unwrap();
    assert_eq!(ids, vec![old[1], old[0]]);
    let (person, manual, active, review): (i64, bool, bool, bool) = store
        .connection
        .query_row(
            "SELECT person_id,manual,active,review FROM faces WHERE id=?1",
            [old[0]],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert!(manual && active && !review);
    assert!(person > 0);
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT length(vector) FROM faces WHERE id=?1",
                [old[0]],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        512 * 4
    );
    drop(store);
    let mut store = Store::at(&path, true).unwrap().unwrap();
    store.undo().unwrap();
    assert!(
        !store
            .connection
            .query_row("SELECT ignored FROM faces WHERE id=?1", [old[1]], |r| r
                .get::<_, bool>(0))
            .unwrap()
    );
    // A failed re-embedding does not erase organization, including after a retry.
    store
        .save_for_model(
            b"family.jpg",
            b"same",
            0,
            &Err("temporary inference failure".into()),
            faces::VERSION,
        )
        .unwrap();
    let ids = store
        .save_for_model(
            b"family.jpg",
            b"same",
            0,
            &Ok(vec![new, second]),
            faces::VERSION,
        )
        .unwrap();
    assert_eq!(ids, old);
    store.undo().unwrap();
    assert!(
        store
            .connection
            .query_row(
                "SELECT person_id IS NULL FROM faces WHERE id=?1",
                [old[0]],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
}
#[test]
fn model_change_never_transfers_ambiguous_or_replaced_faces() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::at(&temp.path().join("people.sqlite"), true)
        .unwrap()
        .unwrap();
    let ids = store.save(b"a", b"stamp", 0, &Ok(vec![face(0)])).unwrap();
    store
        .edit(Edit::Assign {
            face_ids: ids.clone(),
            person_id: None,
            name: Some("家人".into()),
        })
        .unwrap();
    let fresh = store
        .save_for_model(
            b"a",
            b"stamp",
            0,
            &Ok(vec![face(0), face(1)]),
            "yunet640-auraface112-align5-v1",
        )
        .unwrap();
    assert!(!fresh.contains(&ids[0]));
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT active=0 AND review=1 FROM faces WHERE id=?1",
                [ids[0]],
                |r| r.get::<_, bool>(0)
            )
            .unwrap(),
        true
    );
    let newer = store
        .save_for_model(b"a", b"changed", 90, &Ok(vec![face(0)]), faces::VERSION)
        .unwrap();
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT person_id IS NULL FROM faces WHERE id=?1",
                [newer[0]],
                |r| r.get::<_, bool>(0)
            )
            .unwrap(),
        true
    );
}
#[test]
fn legacy_people_schema_reads_without_writes_and_migrates_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("people.sqlite");
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE media(path BLOB PRIMARY KEY,stamp BLOB,rotation INTEGER,version TEXT,error TEXT);PRAGMA user_version=1;").unwrap();
    c.execute(
        "INSERT INTO media VALUES(?1,?2,0,?3,NULL)",
        params![b"a", b"same", faces::VERSION],
    )
    .unwrap();
    drop(c);
    let store = Store::at(&path, false).unwrap().unwrap();
    assert_eq!(store.schema_version().unwrap(), 1);
    assert_eq!(
        store.records().unwrap()[b"a".as_slice()].geometry_version,
        faces::GEOMETRY_VERSION
    );
    drop(store);
    let store = Store::at(&path, true).unwrap().unwrap();
    assert_eq!(
        store.records().unwrap()[b"a".as_slice()].geometry_version,
        faces::GEOMETRY_VERSION
    );
    assert_eq!(
        store
            .connection
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
}

#[test]
fn version_two_people_remain_visible_and_old_undo_payloads_survive() {
    let _session = TEST_SESSION.lock().unwrap();
    let album = tempfile::tempdir().unwrap();
    RgbImage::new(2, 2)
        .save(album.path().join("a.png"))
        .unwrap();
    let id = open(album.path());
    let face = save(id)[0];
    let scan = scan(id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    store
        .edit(Edit::Assign {
            face_ids: vec![face],
            person_id: None,
            name: Some("旧名字".into()),
        })
        .unwrap();
    let person = assigned(&store, face).unwrap();
    store
        .edit(Edit::Rename {
            person_id: person,
            name: "新名字".into(),
        })
        .unwrap();
    store.connection.execute_batch(
        r#"UPDATE edits SET payload=replace(payload, ',"ignored":false', '') WHERE description='修改人物名称';
         ALTER TABLE people DROP COLUMN ignored;
         PRAGMA user_version=2;"#,
    ).unwrap();
    drop(store);
    assert_eq!(
        query::people(id, "", 0, Default::default()).unwrap().items[0].name,
        "新名字"
    );
    assert_eq!(
        query::counts(id).unwrap().2.as_deref(),
        Some("修改人物名称")
    );
    assert_eq!(
        Store::open(&scan, false)
            .unwrap()
            .unwrap()
            .schema_version()
            .unwrap(),
        2
    );
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    store.undo().unwrap();
    assert_eq!(
        query::people(id, "", 0, Default::default()).unwrap().items[0].name,
        "旧名字"
    );
}

#[test]
#[ignore = "requires PHOTOSYS_MODEL_OPTIONS_DIR and PHOTOSYS_MODEL_OPTIONS_PORTRAIT; run alone"]
fn real_people_model_selection_keeps_manual_identity_and_requires_manual_start() {
    let _session = TEST_SESSION.lock().unwrap();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ai::catalog::select(ai::catalog::default_model(ai::ModelKind::People));
        }
    }
    let _restore = Restore;
    let models =
        std::path::PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_DIR").expect("models"));
    let portrait = std::path::PathBuf::from(
        std::env::var_os("PHOTOSYS_MODEL_OPTIONS_PORTRAIT").expect("portrait"),
    );
    let name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    ai::inference::initialize_from(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/onnxruntime")
            .join(std::env::consts::ARCH)
            .join(name),
    )
    .unwrap();
    let album = tempfile::tempdir().unwrap();
    fs::copy(&portrait, album.path().join("portrait.jpg")).unwrap();
    let id = open(album.path());
    let old = ai::catalog::default_model(ai::ModelKind::People);
    ai::catalog::select(old);
    let runtime = Arc::new(FaceRuntime::load_spec(&models.join(old.id), old).unwrap());
    assert_eq!(
        update(id, false, &Channel::new(|_| Ok(())), || Ok(runtime.clone()))
            .unwrap()
            .indexed,
        1
    );
    drop(runtime);
    let person = query::people(id, "", 0, Default::default()).unwrap().items[0].id;
    let cards = query::faces(id, FaceScope::Person, Some(person), None, 0).unwrap();
    assert!(!cards.items.is_empty());
    let face_id = cards.items[0].id;
    let scan = scan(id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    store
        .edit(Edit::Assign {
            face_ids: vec![face_id],
            person_id: None,
            name: Some("家人".into()),
        })
        .unwrap();
    drop(store);
    let person = query::people(id, "家人", 0, Default::default())
        .unwrap()
        .items[0]
        .id;
    let new = ai::catalog::find(ai::ModelKind::People, "yunet-auraface-fp32-v1").unwrap();
    ai::catalog::select(new);
    assert!(status(id).unwrap().needs_rebuild);
    assert_eq!(
        query::photos(id, person, &all(), Sort::DateAsc, 0)
            .unwrap()
            .total,
        1
    );
    update(id, true, &Channel::new(|_| Ok(())), || {
        panic!("must wait for manual rebuild")
    })
    .unwrap();
    let runtime = Arc::new(FaceRuntime::load_spec(&models.join(new.id), new).unwrap());
    let result = update(id, false, &Channel::new(|_| Ok(())), || Ok(runtime.clone())).unwrap();
    assert_eq!(result.indexed, 1);
    assert!(!result.needs_rebuild);
    assert!(result.auto_grouping);
    let cards = query::faces(id, FaceScope::Person, Some(person), None, 0).unwrap();
    assert_eq!(cards.items[0].id, face_id);
    assert!(cards.items[0].manual);
    assert!(!cards.items[0].needs_review);
    let store = Store::open(&scan, false).unwrap().unwrap();
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT length(vector) FROM faces WHERE id=?1",
                [face_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        512 * 4
    );
    drop(store);
    fs::copy(&portrait, album.path().join("new-portrait.jpg")).unwrap();
    let id = open(album.path());
    let incremental = update(id, true, &Channel::new(|_| Ok(())), || Ok(runtime.clone())).unwrap();
    assert_eq!(incremental.reused, 1);
    assert_eq!(incremental.indexed, 2);
    assert_eq!(incremental.pending_faces, 0);
    assert_eq!(
        query::faces(id, FaceScope::Person, Some(person), None, 0)
            .unwrap()
            .total,
        2
    );
    ai::catalog::select(old);
    assert!(status(id).unwrap().needs_rebuild);
    assert_eq!(
        query::photos(id, person, &all(), Sort::DateAsc, 0)
            .unwrap()
            .total,
        2
    );
    *session().unwrap() = None;
}

#[test]
fn person_photos_months_and_map_share_geographic_filters() {
    let _guard = TEST_SESSION.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    for n in 0..30 {
        RgbImage::new(4, 4)
            .save(temp.path().join(format!("20240101-{n:03}.png")))
            .unwrap();
    }
    let root = crate::duplicates::Root::new(temp.path()).unwrap();
    favorites::initialize(&root).unwrap();
    {
        let mut cache = library::locations::Cache::open(&root).unwrap();
        for n in 0..25 {
            let name = format!("20240101-{n:03}.png");
            let relative = Path::new(&name);
            let stamp =
                crate::duplicates::FileStamp::read(&root.open_file(relative).unwrap()).unwrap();
            let coordinates =
                crate::media_date::Coordinates::new(20., 100. + n as f64 / 100.).unwrap();
            cache.write(
                relative,
                &stamp,
                &crate::media_date::Metadata {
                    date: None,
                    coordinates: Some(coordinates),
                },
                &Some(library::locations::Location {
                    coordinates,
                    address: None,
                }),
            );
        }
    }
    let scan_id = open(temp.path());
    let ids = save(scan_id);
    let scanned = scan(scan_id).unwrap();
    let mut store = Store::open(&scanned, true).unwrap().unwrap();
    store
        .edit(Edit::Assign {
            face_ids: ids,
            person_id: None,
            name: Some("家人".into()),
        })
        .unwrap();
    drop(store);
    let person = query::people(scan_id, "家人", 0, Default::default())
        .unwrap()
        .items[0]
        .id;
    let mut filter = all();
    filter.location_mode = library::locations::LocationMode::Known;
    assert_eq!(
        query::photos(scan_id, person, &filter, Sort::DateAsc, 1)
            .unwrap()
            .total,
        25
    );
    assert_eq!(
        query::photos(scan_id, person, &filter, Sort::DateAsc, 1)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        query::photo_ids(scan_id, person, &filter).unwrap().len(),
        25
    );
    assert_eq!(
        query::months(scan_id, person, &filter, Sort::DateAsc).unwrap()[0].count,
        25
    );
    filter.location_selection = Some(library::locations::Selection::Point {
        latitude: 20.,
        longitude: 100.,
    });
    assert_eq!(
        query::photos(scan_id, person, &filter, Sort::DateAsc, 0)
            .unwrap()
            .total,
        1
    );
    filter.location_selection = None;
    filter.location_mode = library::locations::LocationMode::Unknown;
    assert_eq!(query::photo_ids(scan_id, person, &filter).unwrap().len(), 5);
    query::close_for_test();
    *session().unwrap() = None;
}

#[test]
fn database_backup_restores_legacy_people_and_dissolves_ignored_groups() {
    use crate::database_backup;
    let _session = TEST_SESSION.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    for file in ["a.jpg", "b.jpg"] {
        image::RgbImage::new(8, 8).save(source.join(file)).unwrap();
    }
    let id = open(&source);
    let faces = save(id);
    let scan = scan(id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    store
        .edit(Edit::Assign {
            face_ids: vec![faces[0]],
            person_id: None,
            name: Some("妈妈".into()),
        })
        .unwrap();
    let parent = assigned(&store, faces[0]).unwrap();
    store
        .edit(Edit::Assign {
            face_ids: faces[1..].to_vec(),
            person_id: None,
            name: Some("旧分组".into()),
        })
        .unwrap();
    let retired = assigned(&store, faces[1]).unwrap();
    store
        .edit(Edit::Ignore {
            face_ids: vec![faces[2]],
        })
        .unwrap();
    store
        .edit(Edit::Rename {
            person_id: parent,
            name: "新名称".into(),
        })
        .unwrap();
    store
        .edit(Edit::Rename {
            person_id: retired,
            name: "隐藏分组".into(),
        })
        .unwrap();
    let connection = &store.connection;
    connection
        .execute("UPDATE people SET ignored=1 WHERE id=?1", [retired])
        .unwrap();
    connection
        .execute("UPDATE faces SET manual=0 WHERE id=?1", [faces[1]])
        .unwrap();
    connection
        .execute("UPDATE faces SET active=0,review=1 WHERE id=?1", [faces[3]])
        .unwrap();
    for description in ["忽略人物", "恢复忽略的人物", "忽略人物"] {
        connection
            .execute(
                "INSERT INTO edits(description,payload)
            SELECT ?1,payload FROM edits WHERE description='修改人物名称' ORDER BY id DESC LIMIT 1",
                [description],
            )
            .unwrap();
    }
    connection.pragma_update(None, "user_version", 5).unwrap();
    let model = ai::catalog::active(ai::ModelKind::People);
    let saved = serde_json::json!({"threshold": 0.62_f32, "margin": 0.04_f32});
    grouping::Policy::save(
        &store,
        model.feature_version,
        serde_json::from_value(saved.clone()).unwrap(),
    )
    .unwrap();
    drop(store);
    let assert_projection = |scan_id| {
        let people = query::people(scan_id, "", 0, Default::default()).unwrap();
        assert_eq!(people.total, 1);
        assert_eq!(people.items[0].id, parent);
        assert_eq!(people.items[0].photo_count, 1);
        assert_eq!(
            query::photos(scan_id, parent, &all(), Sort::DateAsc, 0)
                .unwrap()
                .total,
            1
        );
        assert!(query::photos(scan_id, retired, &all(), Sort::DateAsc, 0).is_err());
        let pending = query::faces(scan_id, FaceScope::Pending, None, None, 0).unwrap();
        assert_eq!(pending.total, 2);
        assert!(
            pending
                .items
                .iter()
                .all(|f| f.person_id.is_none() && f.manual)
        );
        assert!(
            pending
                .items
                .iter()
                .any(|f| f.id == faces[1] && !f.needs_review)
        );
        assert!(
            pending
                .items
                .iter()
                .any(|f| f.id == faces[3] && f.needs_review)
        );
        let ignored = query::faces(scan_id, FaceScope::Ignored, None, None, 0).unwrap();
        assert_eq!(ignored.total, 1);
        assert_eq!(ignored.items[0].id, faces[2]);
        assert_eq!(ignored.items[0].person_id, None);
        assert_eq!(
            query::counts(scan_id).unwrap(),
            (2, 1, Some("修改人物名称".into()))
        );
    };
    // Keep the reusable read connection alive just as the people UI does.
    assert_projection(id);
    assert_eq!(
        Store::open(&scan, false)
            .unwrap()
            .unwrap()
            .schema_version()
            .unwrap(),
        5
    );
    let zip = temp.path().join("backup.zip");
    database_backup::export(&scan.root, &zip, |_| {}).unwrap();
    let checked = tempfile::tempdir().unwrap();
    let inspection = database_backup::inspect(&zip, checked.path(), &|_| {}).unwrap();
    let original = [
        fs::read(source.join("a.jpg")).unwrap(),
        fs::read(source.join("b.jpg")).unwrap(),
    ];
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_projection(id);
    store
        .edit(Edit::Rename {
            person_id: parent,
            name: "恢复前的名称".into(),
        })
        .unwrap();
    drop(store);
    close_reader().unwrap();
    database_backup::restore(
        &scan.root,
        &zip,
        &inspection.fingerprint,
        "restore-backup-12345678-1234-1234-1234-123456789abc",
        |_| {},
        |_| Ok(()),
    )
    .unwrap();
    library::invalidate().unwrap();
    let new_id = open(&source);
    assert_ne!(id, new_id);
    assert!(query::people(id, "", 0, Default::default()).is_err());
    assert_projection(new_id);
    let scan = super::scan(new_id).unwrap();
    assert_eq!(
        Store::open(&scan, false)
            .unwrap()
            .unwrap()
            .schema_version()
            .unwrap(),
        5
    );
    let restored = status(new_id).unwrap();
    assert_eq!(
        serde_json::to_value(restored.grouping_settings).unwrap()["parameters"],
        saved
    );
    assert_eq!(
        Store::open(&scan, false)
            .unwrap()
            .unwrap()
            .schema_version()
            .unwrap(),
        SCHEMA_VERSION
    );
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    assert_projection(new_id);
    assert_eq!(assigned(&store, faces[1]), None);
    assert_eq!(assigned(&store, faces[2]), None);
    assert_eq!(assigned(&store, faces[3]), None);
    store.undo().unwrap();
    assert_eq!(
        query::people(new_id, "", 0, Default::default())
            .unwrap()
            .items[0]
            .name,
        "妈妈"
    );
    store.undo().unwrap();
    assert_eq!(
        query::faces(new_id, FaceScope::Ignored, None, None, 0)
            .unwrap()
            .total,
        0
    );
    assert_eq!(assigned(&store, faces[2]), None);
    assert_eq!(query::counts(new_id).unwrap().0, 3);
    while store.undo_description().unwrap().is_some() {
        store.undo().unwrap();
        assert_eq!(
            query::people(new_id, "隐藏分组", 0, Default::default())
                .unwrap()
                .total,
            0
        );
        assert!(query::photos(new_id, retired, &all(), Sort::DateAsc, 0).is_err());
    }
    for (file, bytes) in ["a.jpg", "b.jpg"].into_iter().zip(original) {
        assert_eq!(fs::read(source.join(file)).unwrap(), bytes);
    }
    query::close_for_test();
}
