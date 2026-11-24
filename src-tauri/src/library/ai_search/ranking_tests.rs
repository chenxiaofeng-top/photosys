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

// Keep the allocating decode and full sort as an independent ranking oracle.
fn reference(
    scan: &Scan,
    text: &[f32],
    filter: &Filter,
    favorites: &HashSet<usize>,
    rotations: &HashMap<usize, u16>,
) -> Ranked {
    let state = status(scan.id).unwrap();
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
    Ranked {
        ids: ranked.into_iter().map(|(_, id)| id).collect(),
        indexed: state.indexed,
        failed: state.failed,
        pending: state.pending,
    }
}

fn synthetic_album(
    path: &Path,
    count: usize,
    spec: &'static ai::catalog::ModelSpec,
) -> (Arc<Scan>, Vec<f32>) {
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
                    relative: Path::new(&format!("album-{:02}", i % 18)).join(&name),
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
    super::storage_tests::start_legacy_database(&index_path(&scan).unwrap());
    let mut cache = Cache::open(&scan, true).unwrap().unwrap();
    cache.spec = spec;
    for chunk in scan.items.chunks(256) {
        let entries = chunk
            .iter()
            .filter(|item| item.file.id % 101 != 0)
            .map(|item| {
                let id = item.file.id;
                // Repeat patterns to guarantee ties and fill all dimensions to exercise rounding.
                let value = if id % 97 == 0 {
                    Err("test decoding failure".into())
                } else {
                    Ok((0..spec.dimension)
                        .map(|i| (((id % 129 + i * 79) * (i + 1)) % 101) as f32 / 50. - 1.)
                        .collect())
                };
                (
                    favorites::path_key(&item.file.relative),
                    if id % 107 == 0 {
                        vec![]
                    } else {
                        item.file.stamp.portable_key()
                    },
                    if id % 109 == 0 { 90 } else { 0 },
                    value,
                )
            })
            .collect::<Vec<_>>();
        cache.save(&entries).unwrap();
    }
    let embedding = ai::inference::normalize_dim(
        (0..spec.dimension).map(|i| (i % 31) as f32 - 15.).collect(),
        spec.dimension,
    )
    .unwrap();
    (scan, embedding)
}

#[test]
fn reusable_vector_buffer_preserves_values_and_validation() {
    for dim in [512, 768] {
        let mut buffer = Vec::with_capacity(dim);
        let allocation = buffer.as_ptr();
        let encode = |values: Vec<f32>| {
            values
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect::<Vec<_>>()
        };
        for bytes in [
            encode((0..dim).map(|i| (i % 129) as f32 - 63.5).collect()),
            encode(vec![1.; dim]),
            vec![0; dim * 4],
            encode(vec![f32::NAN; dim]),
            encode(vec![f32::INFINITY; dim]),
            encode(vec![f32::MAX; dim]),
            vec![0; dim * 4 - 1],
        ] {
            let result = decode_vector(&bytes, dim, &mut buffer);
            match vector_dim(&bytes, dim) {
                Ok(expected) => {
                    result.unwrap();
                    assert!(
                        buffer
                            .iter()
                            .zip(expected)
                            .all(|(new, old)| new.to_bits() == old.to_bits())
                    );
                }
                Err(expected) => assert_eq!(result.unwrap_err(), expected),
            }
            assert_eq!(buffer.as_ptr(), allocation);
        }
    }
}

fn open_reader(path: &Path, scan_id: u64, cache_statements: bool) -> SearchReader {
    let connection = Cache::at(path, false).unwrap().unwrap().connection;
    if !cache_statements {
        connection.set_prepared_statement_cache_capacity(0);
    }
    SearchReader {
        scan_id: Some(scan_id),
        connection: Some(connection),
        ..SearchReader::default()
    }
}

fn percentile(times: &mut [Duration], percent: usize) -> f64 {
    times.sort_unstable();
    times[(times.len() * percent).div_ceil(100) - 1].as_secs_f64() * 1000.
}

#[test]
#[ignore = "requires an installed model at PHOTOSYS_AI_MODEL_DIR"]
fn benchmark_content_search_text_encoding() {
    let model = std::path::PathBuf::from(std::env::var_os("PHOTOSYS_AI_MODEL_DIR").unwrap());
    let id = model.file_name().unwrap().to_str().unwrap();
    let spec = ai::catalog::find(ai::ModelKind::Search, id).unwrap();
    let library = format!(
        "{}onnxruntime{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    ai::inference::initialize_from(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/onnxruntime")
            .join(std::env::consts::ARCH)
            .join(library),
    )
    .unwrap();
    let runtime = ai::inference::Runtime::load_spec(&model, spec).unwrap();
    let mut times = Vec::new();
    for query in ["照片", "海边日落", "雪地里的小狗", "春节的红灯笼"] {
        runtime.encode_text(query).unwrap();
        for _ in 0..6 {
            let start = Instant::now();
            assert_eq!(runtime.encode_text(query).unwrap().len(), spec.dimension);
            times.push(start.elapsed());
        }
    }
    println!(
        "{} CPU text encoding P50 {:.3} ms, P95 {:.3} ms",
        spec.id,
        percentile(&mut times, 50),
        percentile(&mut times, 95),
    );
}

#[test]
#[ignore = "10k/100k synthetic exact-search benchmark"]
fn benchmark_content_search_ranking_and_pagination() {
    let _session = TEST_SESSION.lock().unwrap();
    struct RestoreModel(&'static ai::catalog::ModelSpec);
    impl Drop for RestoreModel {
        fn drop(&mut self) {
            ai::catalog::select(self.0);
        }
    }
    let _restore = RestoreModel(ai::catalog::active(ai::ModelKind::Search));
    let large = ai::catalog::find(ai::ModelKind::Search, "chinese-clip-vit-l14-fp32-v1").unwrap();
    for spec in [ai::catalog::default_model(ai::ModelKind::Search), large] {
        ai::catalog::select(spec);
        for count in [10_000, 100_000] {
            benchmark_album(count, spec);
        }
    }
}

fn benchmark_album(count: usize, spec: &'static ai::catalog::ModelSpec) {
    let album = tempfile::tempdir().unwrap();
    let (scan, embedding) = synthetic_album(album.path(), count, spec);
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
    let expected = [&all(), &combined]
        .map(|filter| reference(&scan, &embedding, filter, &favorites, &rotations));
    let optimized = index_path(&scan).unwrap();
    let legacy = album.path().join("legacy.sqlite");
    fs::copy(&optimized, &legacy).unwrap();
    optimize_index_storage(scan.id, |directory| fs2::available_space(directory)).unwrap();
    let paths = [&legacy, &optimized];
    for (label, filter, expected) in [
        ("unfiltered", all(), &expected[0]),
        ("combined", combined, &expected[1]),
    ] {
        for (index, path) in paths.into_iter().enumerate() {
            let mut reader = open_reader(path, scan.id, index != 0);
            let map = reader
                .search(scan.id, "照片", filter.clone(), 0, 200, |_| {
                    Ok(embedding.clone())
                })
                .unwrap();
            assert_eq!(ids(&map.items), expected.ids);
            assert_eq!(
                (map.indexed, map.failed, map.pending),
                (expected.indexed, expected.failed, expected.pending)
            );
            for (page, items) in expected.ids.chunks(24).enumerate() {
                let page = reader
                    .search(scan.id, "照片", filter.clone(), page, 24, |_| {
                        panic!("cached text")
                    })
                    .unwrap();
                assert_eq!(ids(&page.items), items);
            }
        }
        let mut cold: [Vec<Duration>; 2] = Default::default();
        let mut warm: [Vec<Duration>; 2] = Default::default();
        for round in 0..12 {
            let mut readers = [SearchReader::default(), SearchReader::default()];
            for offset in 0..2 {
                let index = (round + offset) % 2;
                let start = Instant::now();
                let mut reader = open_reader(paths[index], scan.id, index != 0);
                let result = reader
                    .search(scan.id, "照片", filter.clone(), 0, 24, |_| {
                        Ok(embedding.clone())
                    })
                    .unwrap();
                cold[index].push(start.elapsed());
                assert_eq!(result.total, expected.ids.len());
                readers[index] = reader;
            }
            for page in [1, 0, 1, 0, 1, 0, 1, 0] {
                for offset in 0..2 {
                    let index = (round + offset + page) % 2;
                    let start = Instant::now();
                    let result = readers[index]
                        .search(scan.id, "照片", filter.clone(), page, 24, |_| {
                            panic!("cached text")
                        })
                        .unwrap();
                    warm[index].push(start.elapsed());
                    assert_eq!(
                        ids(&result.items),
                        expected.ids.chunks(24).nth(result.page).unwrap()
                    );
                }
            }
        }
        for (index, layout) in ["4KiB", "16KiB"].into_iter().enumerate() {
            for (phase, times) in [("first", &mut cold[index]), ("page", &mut warm[index])] {
                println!(
                    "{count:>6} {}d {label:<10} {layout:<5} {phase:<5} P50 {:.3} ms, P95 {:.3} ms",
                    spec.dimension,
                    percentile(times, 50),
                    percentile(times, 95),
                );
            }
        }
    }
    *session().unwrap() = None;
}
