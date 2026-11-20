use super::*;
use crate::library::{OpenMode, SCAN_ID, Session, TEST_SESSION, locations};
use std::time::Instant;

fn all() -> Filter {
    serde_json::from_value(serde_json::json!({
        "unknownDate": false,
        "favoritesOnly": false,
    }))
    .unwrap()
}

fn ids(items: &[ItemView]) -> Vec<usize> {
    items.iter().map(|item| item.id).collect()
}

fn index_entry(cache: &mut Cache, item: &Item, rotation: u16, value: Result<Vec<f32>, String>) {
    cache
        .save(&[(
            favorites::path_key(&item.file.relative),
            item.file.stamp.portable_key(),
            rotation,
            value,
        )])
        .unwrap();
}

#[test]
fn pending_index_batch_discards_rotated_or_replaced_photos() {
    let _session = TEST_SESSION.lock().unwrap();
    let album = tempfile::tempdir().unwrap();
    for name in ["keep.png", "rotate.png", "replace.png"] {
        image::RgbImage::new(2, 2)
            .save(album.path().join(name))
            .unwrap();
    }
    let id = super::super::open_directory(album.path().to_str().unwrap(), OpenMode::Select, |_| {})
        .unwrap()
        .scan_id;
    let scan = scan(id).unwrap();
    let mut cache = Cache::open(&scan, true).unwrap().unwrap();
    let mut batch = scan
        .items
        .iter()
        .map(|item| {
            (
                item.file.id,
                (
                    favorites::path_key(&item.file.relative),
                    item.file.stamp.portable_key(),
                    0,
                    Ok(vec![1.; DIM]),
                ),
            )
        })
        .collect();
    let rotated = scan
        .items
        .iter()
        .find(|item| item.file.relative == Path::new("rotate.png"))
        .unwrap();
    tauri::async_runtime::block_on(super::super::set_library_rotation(id, rotated.file.id, 90))
        .unwrap();
    image::RgbImage::new(5, 5)
        .save(album.path().join("replace.png"))
        .unwrap();
    let mut state = IndexStatus {
        indexed: 3,
        ..Default::default()
    };
    save_current_batch(
        &scan,
        MODEL_GENERATION.load(Ordering::Relaxed),
        &mut cache,
        &mut batch,
        &mut state,
    )
    .unwrap();
    assert_eq!(state.indexed, 1);
    let records = cache.records().unwrap();
    assert_eq!(records.len(), 1);
    assert!(records.contains_key(b"keep.png".as_slice()));
    drop(cache);
    *session().unwrap() = None;
}

#[test]
fn search_reuses_ranking_and_refreshes_on_index_metadata_or_scan_changes() {
    let _session = TEST_SESSION.lock().unwrap();
    let album = tempfile::tempdir().unwrap();
    for i in 0..3 {
        image::RgbImage::new(2, 2)
            .save(album.path().join(format!("{i}.png")))
            .unwrap();
    }
    let scan_id =
        super::super::open_directory(album.path().to_str().unwrap(), OpenMode::Select, |_| {})
            .unwrap()
            .scan_id;
    let scan = scan(scan_id).unwrap();
    let mut reader = SearchReader::default();
    let mut embedding = vec![0.; DIM];
    embedding[0] = 1.;
    let empty = reader
        .search(scan_id, " 照片 ", all(), 0, 24, |_| Ok(embedding.clone()))
        .unwrap();
    assert_eq!((empty.indexed, empty.failed, empty.pending), (0, 0, 3));
    let mut writer = Cache::open(&scan, true).unwrap().unwrap();
    index_entry(&mut writer, &scan.items[0], 0, Ok(embedding.clone()));
    index_entry(&mut writer, &scan.items[1], 0, Ok(embedding.clone()));
    index_entry(&mut writer, &scan.items[2], 0, Err("sample error".into()));
    let first = reader
        .search(scan_id, "照片", all(), 0, 24, |_| panic!("cached text"))
        .unwrap();
    assert_eq!(ids(&first.items), [0, 1]);
    assert_eq!((first.indexed, first.failed, first.pending), (2, 1, 0));
    let cached_ids = reader.ranking.as_ref().unwrap().result.ids.as_ptr();
    // Same helper as the map entry point. A cache miss would allocate a new ID vector.
    let map = reader
        .search(scan_id, "照片", all(), 0, 200, |_| panic!("cached text"))
        .unwrap();
    assert_eq!(ids(&map.items), ids(&first.items));
    assert_eq!(
        reader.ranking.as_ref().unwrap().result.ids.as_ptr(),
        cached_ids
    );

    let mut filter = all();
    filter.favorites_only = true;
    assert!(
        reader
            .search(scan_id, "照片", filter.clone(), 0, 24, |_| panic!(
                "cached text"
            ))
            .unwrap()
            .items
            .is_empty()
    );
    tauri::async_runtime::block_on(super::super::set_library_favorite(scan_id, 1, true)).unwrap();
    let favorite = reader
        .search(scan_id, "照片", filter.clone(), 0, 24, |_| {
            panic!("cached text")
        })
        .unwrap();
    assert_eq!(ids(&favorite.items), [1]);
    assert!(favorite.items[0].favorite);
    tauri::async_runtime::block_on(super::super::set_library_rotation(scan_id, 1, 90)).unwrap();
    let rotated = reader
        .search(scan_id, "照片", filter.clone(), 0, 24, |_| {
            panic!("cached text")
        })
        .unwrap();
    assert!(rotated.items.is_empty());
    assert_eq!(
        (rotated.indexed, rotated.failed, rotated.pending),
        (1, 1, 1)
    );
    index_entry(&mut writer, &scan.items[1], 90, Ok(embedding.clone()));
    let updated = reader
        .search(scan_id, "照片", filter, 0, 24, |_| panic!("cached text"))
        .unwrap();
    assert_eq!(ids(&updated.items), [1]);
    assert_eq!(updated.items[0].rotation, 90);
    drop(writer);
    let new_scan =
        super::super::open_directory(album.path().to_str().unwrap(), OpenMode::Select, |_| {})
            .unwrap()
            .scan_id;
    assert!(
        reader
            .search(scan_id, "照片", all(), 0, 24, |_| panic!("stale scan"))
            .is_err()
    );
    let reopened = reader
        .search(new_scan, "照片", all(), 0, 24, |_| Ok(embedding))
        .unwrap();
    assert_eq!(ids(&reopened.items), [0, 1]);
    *session().unwrap() = None;
}

// Reference from before the optimization, including a separate status pass and a full sort.
fn reference(
    scan: &Scan,
    text: &[f32],
    filter: &Filter,
    favorites: &HashSet<usize>,
    rotations: &HashMap<usize, u16>,
    page: usize,
) -> Vec<usize> {
    let _ = status(scan.id).unwrap();
    let cache = Cache::open(scan, false).unwrap().unwrap();
    let from = parse_date(filter.from.as_deref()).unwrap();
    let to = parse_date(filter.to.as_deref()).unwrap();
    let candidates: HashMap<_, _> = scan
        .items
        .iter()
        .filter(|i| eligible(i) && filter.matches_location(i))
        .filter(|i| !filter.favorites_only || favorites.contains(&i.file.id))
        .filter(|i| {
            if filter.unknown_date {
                i.captured_on.is_none()
            } else {
                from.is_none_or(|d| i.captured_on >= Some(d))
                    && to.is_none_or(|d| i.captured_on.is_some_and(|v| v <= d))
            }
        })
        .map(|i| (favorites::path_key(&i.file.relative), i))
        .collect();
    let mut statement = cache.connection.prepare(
        "SELECT path, stamp, rotation, vector FROM embeddings WHERE version=?1 AND vector IS NOT NULL"
    ).unwrap();
    let mut rows = statement.query([cache.spec.feature_version]).unwrap();
    let mut ranked = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let path: Vec<u8> = row.get(0).unwrap();
        let Some(item) = candidates.get(&path) else {
            continue;
        };
        let stamp: Vec<u8> = row.get(1).unwrap();
        let rotation: u16 = row.get(2).unwrap();
        if stamp != item.file.stamp.portable_key()
            || rotation != rotations.get(&item.file.id).copied().unwrap_or(0)
        {
            continue;
        }
        let bytes: Vec<u8> = row.get(3).unwrap();
        let vector = vector_dim(&bytes, cache.spec.dimension).unwrap();
        let score: f32 = text.iter().zip(vector).map(|(a, b)| a * b).sum();
        ranked.push((score, item.file.id));
    }
    ranked.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    ranked.truncate(RESULT_LIMIT);
    if page != 0 {
        let (_, offset) = Page::<ItemView>::bounds(page, ranked.len());
        ranked = ranked
            .into_iter()
            .skip(offset)
            .take(super::super::PAGE_SIZE)
            .collect();
    }
    ranked.into_iter().map(|(_, id)| id).collect()
}

fn synthetic_album(path: &Path, count: usize) -> (Arc<Scan>, Vec<f32>) {
    let root = crate::duplicates::Root::new(path).unwrap();
    favorites::initialize(&root).unwrap();
    let file = fs::File::create(path.join("seed")).unwrap();
    let stamp = crate::duplicates::FileStamp::read(&file).unwrap();
    let id = SCAN_ID.fetch_add(1, Ordering::Relaxed);
    let items = (0..count)
        .map(|i| {
            let name = format!("photo-{i:06}.jpg");
            Item {
                file: crate::duplicates::Candidate {
                    id: i,
                    relative: name.clone().into(),
                    stamp: stamp.clone(),
                },
                search_path: name.clone(),
                name_key: name,
                kind: Kind::Photo,
                captured_on: (i % 11 != 0)
                    .then(|| chrono::NaiveDate::from_ymd_opt(2018 + (i % 5) as i32, 1, 1).unwrap()),
                location: Some(locations::Location {
                    coordinates: crate::media_date::Coordinates::new(30., 100. + (i % 7) as f64)
                        .unwrap(),
                    address: None,
                }),
                preview_available: true,
                is_live_photo: false,
            }
        })
        .collect();
    let scan = Scan {
        id,
        root,
        items,
        issues: Vec::new(),
        previews: Mutex::default(),
        ai_decodes: Default::default(),
    };
    let favorites = scan
        .items
        .iter()
        .step_by(2)
        .map(|item| favorites::path_key(&item.file.relative))
        .collect();
    *session().unwrap() = Some(Session::from_scan(scan, favorites, HashMap::new()));
    let scan = super::scan(id).unwrap();
    let mut cache = Cache::open(&scan, true).unwrap().unwrap();
    for chunk in scan.items.chunks(256) {
        let entries = chunk
            .iter()
            .map(|item| {
                let mut vector = vec![0.; DIM];
                vector[0] = (item.file.id % 129) as f32;
                vector[1] = 129. - vector[0];
                (
                    favorites::path_key(&item.file.relative),
                    item.file.stamp.portable_key(),
                    0,
                    Ok(vector),
                )
            })
            .collect::<Vec<_>>();
        cache.save(&entries).unwrap();
    }
    let mut embedding = vec![0.; DIM];
    embedding[0] = 1.;
    (scan, embedding)
}

#[test]
#[ignore = "10k/100k synthetic exact-search benchmark"]
fn benchmark_content_search_ranking_and_pagination() {
    let _session = TEST_SESSION.lock().unwrap();
    let percentile = |times: &mut [Duration], percent: usize| {
        times.sort_unstable();
        times[(times.len() * percent).div_ceil(100) - 1].as_secs_f64() * 1000.
    };
    for count in [10_000, 100_000] {
        let album = tempfile::tempdir().unwrap();
        let (scan, embedding) = synthetic_album(album.path(), count);
        let (favorites, rotations) = {
            let lock = session().unwrap();
            let state = current(&lock, scan.id).unwrap();
            (state.favorites.clone(), state.rotations.clone())
        };
        let mut combined = all();
        combined.favorites_only = true;
        combined.from = Some("2020-01-01".into());
        combined.to = Some("2021-12-31".into());
        combined.location_selection = Some(locations::Selection::Bounds {
            west: 101.,
            south: 29.,
            east: 104.,
            north: 31.,
        });
        for filter in [all(), combined] {
            let expected = reference(&scan, &embedding, &filter, &favorites, &rotations, 0);
            let mut reader = SearchReader::default();
            let map = reader
                .search(scan.id, "照片", filter.clone(), 0, 200, |_| {
                    Ok(embedding.clone())
                })
                .unwrap();
            assert_eq!(ids(&map.items), expected);
            for (page, items) in expected.chunks(24).enumerate() {
                assert_eq!(
                    ids(&reader
                        .search(scan.id, "照片", filter.clone(), page, 24, |_| panic!(
                            "cached text"
                        ))
                        .unwrap()
                        .items),
                    items
                );
            }
        }
        let (mut old_first, mut old_page, mut cold, mut warm) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for _ in 0..12 {
            let start = Instant::now();
            std::hint::black_box(reference(
                &scan,
                &embedding,
                &all(),
                &favorites,
                &rotations,
                0,
            ));
            old_first.push(start.elapsed());
            let start = Instant::now();
            std::hint::black_box(reference(
                &scan,
                &embedding,
                &all(),
                &favorites,
                &rotations,
                1,
            ));
            old_page.push(start.elapsed());
            let mut reader = SearchReader::default();
            let start = Instant::now();
            assert_eq!(
                reader
                    .search(scan.id, "照片", all(), 0, 24, |_| Ok(embedding.clone()))
                    .unwrap()
                    .total,
                RESULT_LIMIT
            );
            cold.push(start.elapsed());
            let start = Instant::now();
            assert_eq!(
                reader
                    .search(scan.id, "照片", all(), 1, 24, |_| panic!("cached text"))
                    .unwrap()
                    .items
                    .len(),
                24
            );
            warm.push(start.elapsed());
        }
        for (label, times) in [
            ("before first", &mut old_first),
            ("before page", &mut old_page),
            ("after first", &mut cold),
            ("after page", &mut warm),
        ] {
            println!(
                "{count:>6} {label:<12} P50 {:.3} ms, P95 {:.3} ms",
                percentile(times, 50),
                percentile(times, 95)
            );
        }
        assert!(percentile(&mut warm, 95) <= percentile(&mut old_page, 95) / 2.);
        *session().unwrap() = None;
    }
}
