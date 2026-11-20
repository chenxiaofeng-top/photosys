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
    let people = query::people(id, "家人", 0).unwrap();
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
    assert!(query::people(id, "", 0).is_err());
    assert_eq!(query::people(fresh, "家人", 0).unwrap().items[0].id, p);
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
        assert_eq!(query::people(id, "", 0).unwrap().total, 2);
        for (path, record) in before {
            let after = store.records().unwrap();
            assert_eq!(record.stamp, after[&path].stamp);
            assert_eq!(record.version, after[&path].version);
        }
    }
}

#[test]
fn edits_during_grouping_refresh_representatives_and_survive_reopen_for_both_models() {
    let _session = TEST_SESSION.lock().unwrap();
    let _restore = RestorePeopleModel(ai::catalog::active(ai::ModelKind::People));
    for model in ["yunet-sface-fp32-v1", "yunet-auraface-fp32-v1"] {
        let spec = ai::catalog::find(ai::ModelKind::People, model).unwrap();
        ai::catalog::select(spec);
        let album = tempfile::tempdir().unwrap();
        for i in 0..6 {
            RgbImage::new(2, 2)
                .save(album.path().join(format!("{i}.png")))
                .unwrap();
        }
        let id = open(album.path());
        let scan = scan(id).unwrap();
        let mut store = Store::open(&scan, true).unwrap().unwrap();
        let mut ids = Vec::new();
        for (i, item) in scan.items.iter().enumerate() {
            let f = if i == 1 {
                model_face(spec, &[0.9, 0., 0.4358899])
            } else {
                model_face(spec, &[1.])
            };
            ids.extend(
                store
                    .save_for_model(
                        &favorites::path_key(&item.file.relative),
                        &item.file.stamp.portable_key(),
                        0,
                        &Ok(vec![f]),
                        spec.feature_version,
                    )
                    .unwrap(),
            );
        }
        let policy = grouping::Policy::shipped();
        let mut groups = grouping::Groups::load(id, policy.clone()).unwrap();
        let mut edits = EDIT_GENERATION.load(Ordering::Relaxed);
        assign_with_edits(id, &mut store, &mut groups, &policy, &mut edits, &ids[..2]).unwrap();
        let old = assigned(&store, ids[0]).unwrap();
        // Use the same IPC mutation path as the UI while analysis still holds its old anchors.
        tauri::async_runtime::block_on(people_edit(
            id,
            Edit::Assign {
                face_ids: vec![ids[0]],
                person_id: None,
                name: Some("纠正的人物".into()),
            },
            None,
        ))
        .unwrap();
        let corrected = assigned(&store, ids[0]).unwrap();
        assert_ne!(corrected, old);
        assign_with_edits(id, &mut store, &mut groups, &policy, &mut edits, &ids[..3]).unwrap();
        assert_eq!(assigned(&store, ids[0]), Some(corrected));
        assert_eq!(assigned(&store, ids[1]), Some(old));
        assert_eq!(assigned(&store, ids[2]), Some(corrected));
        // Move another batch, then merge the duplicate group and ignore a detection.
        tauri::async_runtime::block_on(people_edit(
            id,
            Edit::Assign {
                face_ids: vec![ids[1], ids[3]],
                person_id: Some(corrected),
                name: None,
            },
            None,
        ))
        .unwrap();
        tauri::async_runtime::block_on(people_edit(
            id,
            Edit::Merge {
                source_id: old,
                target_id: corrected,
            },
            None,
        ))
        .unwrap();
        tauri::async_runtime::block_on(people_edit(
            id,
            Edit::Ignore {
                face_ids: vec![ids[4]],
            },
            None,
        ))
        .unwrap();
        assign_with_edits(id, &mut store, &mut groups, &policy, &mut edits, &ids).unwrap();
        assert_eq!(assigned(&store, ids[4]), None);
        assert_eq!(assigned(&store, ids[5]), Some(corrected));
        drop(store);
        query::close_for_test();
        let mut store = Store::open(&scan, true).unwrap().unwrap();
        // Persistent undo restores ignore, merge, and batch move in reverse order.
        for _ in 0..3 {
            tauri::async_runtime::block_on(people_undo(id)).unwrap();
        }
        assert_eq!(assigned(&store, ids[1]), Some(old));
        assert_eq!(assigned(&store, ids[3]), None);
        let mut groups = grouping::Groups::load(id, policy).unwrap();
        groups.assign(&mut store, &ids).unwrap();
        assert_eq!(assigned(&store, ids[0]), Some(corrected));
        assert_eq!(assigned(&store, ids[1]), Some(old));
        assert_eq!(assigned(&store, ids[3]), Some(corrected));
        tauri::async_runtime::block_on(people_edit(
            id,
            Edit::Rename {
                person_id: corrected,
                name: "家人".into(),
            },
            None,
        ))
        .unwrap();
        assert_eq!(query::people(id, "家人", 0).unwrap().items[0].id, corrected);
    }
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
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
    let person = query::people(id, "", 0).unwrap().items[0].id;
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
    let named = query::people(id, "样例人物", 0).unwrap().items[0].id;

    let gpu = Arc::new(FaceRuntime::load_with_device(&model, spec, &device).unwrap());
    fs::copy(&portrait, album.path().join("b.jpg")).unwrap();
    let id = open(album.path());
    let added = update(id, true, &progress, || Ok(gpu.clone())).unwrap();
    assert_eq!((added.indexed, added.reused, added.failed), (2, 1, 0));
    assert!(!added.needs_rebuild);
    let same = update(id, true, &progress, || Ok(cpu.clone())).unwrap();
    assert_eq!(same.reused, 2);
    let person = &query::people(id, "样例人物", 0).unwrap().items[0];
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
    assert_eq!(initial.pending_faces, 0);
    assert!(initial.auto_grouping);
    assert_eq!(query::people(id, "", 0).unwrap().total, 1);
    // Emulate an existing pre-grouping index and exercise the real automatic update path.
    {
        let scan = scan(id).unwrap();
        let store = Store::open(&scan, true).unwrap().unwrap();
        store
            .connection
            .execute_batch("UPDATE faces SET person_id=NULL; DELETE FROM people;")
            .unwrap();
    }
    assert_eq!(status(id).unwrap().pending_faces, 3);
    let same = update(id, true, &channel, || Ok(runtime.clone())).unwrap();
    assert_eq!(same.reused, 3);
    assert_eq!(same.pending_faces, 0);
    assert_eq!(query::people(id, "", 0).unwrap().total, 1);
    let person = query::people(id, "", 0).unwrap().items[0].id;
    let f = query::faces(id, FaceScope::Person, Some(person), None, 0).unwrap();
    let face_id = f.items[0].id;
    let scan = scan(id).unwrap();
    let mut s = Store::open(&scan, true).unwrap().unwrap();
    s.connection
        .execute("UPDATE faces SET person_id=NULL WHERE id=?1", [face_id])
        .unwrap();
    assert_eq!(suggested_people(id, &[face_id])[&face_id], [person]);
    tauri::async_runtime::block_on(people_edit(
        id,
        Edit::Assign {
            face_ids: vec![face_id],
            person_id: Some(person),
            name: None,
        },
        Some(runtime.spec.feature_version.into()),
    ))
    .unwrap();
    assert_eq!(assigned(&s, face_id), Some(person));
    s.edit(Edit::Assign {
        face_ids: vec![face_id],
        person_id: None,
        name: Some("样例人物".into()),
    })
    .unwrap();
    update(id, true, &channel, || Ok(runtime.clone())).unwrap();
    assert_eq!(
        query::people(id, "样例人物", 0).unwrap().items[0].photo_count,
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
        let persons = query::people(id, "", 0).unwrap();
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
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))
            .unwrap(),
        2
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
    let person = query::people(id, "", 0).unwrap().items[0].id;
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
    let person = query::people(id, "家人", 0).unwrap().items[0].id;
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
    let person = query::people(scan_id, "家人", 0).unwrap().items[0].id;
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
fn database_backup_restores_real_people_corrections_undo_and_closes_old_reader() {
    use crate::database_backup;
    let _session = TEST_SESSION.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    image::RgbImage::new(8, 8)
        .save(source.join("a.jpg"))
        .unwrap();
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
    store
        .edit(Edit::Ignore {
            face_ids: vec![faces[1]],
        })
        .unwrap();
    drop(store);
    // Keep the reusable read connection alive just as the people UI does.
    assert_eq!(query::people(id, "", 0).unwrap().items[0].name, "妈妈");
    let zip = temp.path().join("backup.zip");
    database_backup::export(&scan.root, &zip, |_| {}).unwrap();
    let checked = tempfile::tempdir().unwrap();
    let inspection = database_backup::inspect(&zip, checked.path(), &|_| {}).unwrap();
    let original = fs::read(source.join("a.jpg")).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    store
        .edit(Edit::Rename {
            person_id: 1,
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
    assert!(query::people(id, "", 0).is_err());
    assert_eq!(query::people(new_id, "", 0).unwrap().items[0].name, "妈妈");
    let scan = super::scan(new_id).unwrap();
    let mut store = Store::open(&scan, true).unwrap().unwrap();
    assert_eq!(
        store
            .connection
            .query_row("SELECT manual FROM faces WHERE id=?1", [faces[0]], |r| r
                .get::<_, bool>(
                0
            ))
            .unwrap(),
        true
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT ignored FROM faces WHERE id=?1", [faces[1]], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap(),
        true
    );
    store.undo().unwrap();
    assert_eq!(
        store
            .connection
            .query_row("SELECT ignored FROM faces WHERE id=?1", [faces[1]], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap(),
        false
    );
    assert_eq!(fs::read(source.join("a.jpg")).unwrap(), original);
    query::close_for_test();
}
