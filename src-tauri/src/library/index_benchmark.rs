use super::*;
use crate::ai::{
    ModelKind, catalog,
    faces::FaceRuntime,
    indexing::{Mode, Run, Timings},
    inference::{Runtime, initialize_from},
};
use image::{RgbImage, imageops};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::sync::mpsc;

fn filter() -> Filter {
    serde_json::from_value(serde_json::json!({"unknownDate": false, "favoritesOnly": false}))
        .unwrap()
}

fn fixture_album(path: &Path, portrait: &Path, people: &FaceRuntime) -> usize {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let single = image::open(portrait).unwrap().to_rgb8();
    let detected = people.detect(&single).unwrap();
    let [x, y, width, height] = detected
        .first()
        .expect("portrait fixture needs a face")
        .bbox;
    let edge = ((width.max(height) * 1.35).ceil() as u32).min(single.width().min(single.height()));
    let left = ((x + width / 2.0 - edge as f32 / 2.0).round() as u32).min(single.width() - edge);
    let top = ((y + height / 2.0 - edge as f32 / 2.0).round() as u32).min(single.height() - edge);
    let crop = imageops::crop_imm(&single, left, top, edge, edge).to_image();
    let tile = imageops::resize(&crop, 256, 256, imageops::FilterType::Triangle);
    let mut group = RgbImage::from_pixel(640, 640, image::Rgb([128; 3]));
    for (left, top) in [(32, 32), (352, 32), (32, 352)] {
        imageops::replace(&mut group, &tile, left, top);
    }
    let group_file = path.join("group-00.jpg");
    group.save(&group_file).unwrap();
    let group = image::open(&group_file).unwrap().to_rgb8();
    assert!(
        people.detect(&group).unwrap().len() >= 2,
        "group fixture needs multiple faces"
    );
    for index in 0..12 {
        fs::copy(portrait, path.join(format!("portrait-{index:02}.jpg"))).unwrap();
        fs::copy(
            fixtures.join("portrait-reference.jpg"),
            path.join(format!("pattern-{index:02}.jpg")),
        )
        .unwrap();
        if index != 0 {
            fs::copy(&group_file, path.join(format!("group-{index:02}.jpg"))).unwrap();
        }
    }
    for index in 0..4 {
        fs::copy(
            fixtures.join("portrait-48mp-grid.heic"),
            path.join(format!("high-resolution-{index:02}.heic")),
        )
        .unwrap();
    }
    40
}

fn cpu_models(root: &Path) -> (Runtime, FaceRuntime) {
    let runtime_file = if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    initialize_from(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/onnxruntime")
            .join(std::env::consts::ARCH)
            .join(runtime_file),
    )
    .unwrap();
    (
        Runtime::load(&root.join(crate::ai::MODEL_ID)).unwrap(),
        FaceRuntime::load(&root.join("yunet-sface-fp32-v1")).unwrap(),
    )
}

fn gpu_models(root: &Path) -> (Runtime, FaceRuntime) {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        let device = crate::ai::device_tests::directml_device();
        let search = catalog::active(ModelKind::Search);
        let people = catalog::active(ModelKind::People);
        (
            Runtime::load_with_device(&root.join(search.id), search, &device).unwrap(),
            FaceRuntime::load_with_device(&root.join(people.id), people, &device).unwrap(),
        )
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    {
        let _ = root;
        panic!("DirectML benchmark requires Windows x64");
    }
}

fn index_both(
    id: u64,
    mode: Mode,
    automatic: bool,
    search: &Arc<Runtime>,
    people: &Arc<FaceRuntime>,
) -> ([usize; 3], [usize; 3], [f64; 5], [f64; 5]) {
    let search_run = Run {
        mode,
        timings: Some(Timings::default()),
    };
    let people_run = Run {
        mode,
        timings: Some(Timings::default()),
    };
    let index_search = || {
        let _database = crate::database_access::read().unwrap();
        let status = ai_search::update_with_run(
            id,
            automatic,
            &Channel::new(|_| Ok(())),
            &search_run,
            || Ok(search.clone()),
        )
        .unwrap();
        [status.indexed, status.failed, status.reused]
    };
    let index_people = || {
        let _database = crate::database_access::read().unwrap();
        let status = people::update_with_run(
            id,
            automatic,
            &Channel::new(|_| Ok(())),
            &people_run,
            || Ok(people.clone()),
        )
        .unwrap();
        [status.indexed, status.failed, status.reused]
    };
    let (search, people) = if matches!(mode, Mode::ConcurrentCpu | Mode::ConcurrentDirectMl) {
        thread::scope(|scope| {
            let worker = scope.spawn(index_people);
            (index_search(), worker.join().unwrap())
        })
    } else {
        (index_search(), index_people())
    };
    (
        search,
        people,
        search_run.timings.unwrap().millis(),
        people_run.timings.unwrap().millis(),
    )
}

fn p95(samples: &mut [f64]) -> f64 {
    assert!(!samples.is_empty());
    samples.sort_by(f64::total_cmp);
    samples[(samples.len() * 95).div_ceil(100) - 1]
}

fn foreground(scan: Arc<Scan>, search: &Runtime, stop: mpsc::Receiver<()>) -> (Vec<f64>, Vec<f64>) {
    let preview_ids: Vec<_> = scan
        .items
        .iter()
        .filter(|item| {
            item.file
                .relative
                .extension()
                .is_some_and(|ext| ext == "jpg")
        })
        .map(|item| item.file.id)
        .collect();
    let mut preview_times = Vec::new();
    let mut search_times = Vec::new();
    let mut query = ai_search::benchmark_search(search, scan.id, filter());
    for (preview_id, text) in preview_ids
        .into_iter()
        .cycle()
        .zip(["人像", "红蓝方格", "三个人"].into_iter().cycle())
    {
        let started = Instant::now();
        scan.preview(preview_id, false).unwrap();
        preview_times.push(started.elapsed().as_secs_f64() * 1000.);
        let started = Instant::now();
        query(text).unwrap();
        search_times.push(started.elapsed().as_secs_f64() * 1000.);
        if !matches!(
            stop.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            break;
        }
    }
    (preview_times, search_times)
}

fn snapshot(album: &Path) -> (usize, String) {
    let mut hash = Sha256::new();
    let search = Connection::open(album.join(".photosys/ai.sqlite")).unwrap();
    let mut statement = search
        .prepare("SELECT path,vector FROM embeddings ORDER BY path")
        .unwrap();
    for row in statement
        .query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .unwrap()
    {
        let (path, vector) = row.unwrap();
        hash.update(path);
        hash.update(vector);
    }
    let people = Connection::open(album.join(".photosys/people.sqlite")).unwrap();
    let mut statement = people
        .prepare("SELECT path,person_id,vector FROM faces WHERE active=1 ORDER BY path,id")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })
        .unwrap();
    let mut faces = 0;
    for row in rows {
        let (path, person, vector) = row.unwrap();
        hash.update(path);
        hash.update(person.unwrap_or_default().to_le_bytes());
        hash.update(vector.unwrap_or_default());
        faces += 1;
    }
    (faces, format!("{:x}", hash.finalize()))
}

#[test]
#[ignore = "requires existing B/16, SFace and a portrait fixture; run in release mode in a separate process per backend and mode"]
fn benchmark_index_pipeline() {
    let _session = TEST_SESSION.lock().unwrap();
    let models = PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_DIR").unwrap());
    let portrait = PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_PORTRAIT").unwrap());
    let gpu = std::env::var("PHOTOSYS_AI_BENCHMARK_BACKEND").as_deref() == Ok("directml");
    let mode = match std::env::var("PHOTOSYS_AI_BENCHMARK_MODE").as_deref() {
        Ok("serial") => Mode::Serial,
        Ok("pipeline") => Mode::Pipeline,
        Ok("concurrent") if gpu => Mode::ConcurrentDirectMl,
        Ok("concurrent") => Mode::ConcurrentCpu,
        _ => panic!("Set PHOTOSYS_AI_BENCHMARK_MODE to serial, pipeline, or concurrent"),
    };
    let trials = std::env::var("PHOTOSYS_AI_BENCHMARK_TRIALS")
        .ok()
        .map_or(3, |value| value.parse::<usize>().unwrap());
    assert!(trials > 0);
    println!(
        "pid={} mode={mode:?} backend={} threads={}",
        std::process::id(),
        if gpu { "DirectML" } else { "CPU" },
        thread::available_parallelism().unwrap()
    );
    let (search, people) = if gpu {
        gpu_models(&models)
    } else {
        cpu_models(&models)
    };
    let search = Arc::new(search);
    let people = Arc::new(people);
    let mut totals = Vec::new();
    let mut preview_samples = Vec::new();
    let mut search_samples = Vec::new();
    let mut fingerprint = None;
    for trial in 1..=trials {
        let album = tempfile::tempdir().unwrap();
        let count = fixture_album(album.path(), &portrait, &people);
        let scan_id = open_directory(album.path().to_str().unwrap(), OpenMode::Select, |_| {})
            .unwrap()
            .scan_id;
        let scan = scan(scan_id).unwrap();
        let (signal, stop) = mpsc::channel();
        let (elapsed, statuses, (preview, queries)) = thread::scope(|scope| {
            let observer = scope.spawn(|| foreground(scan, &search, stop));
            let started = Instant::now();
            let statuses = index_both(scan_id, mode, false, &search, &people);
            let elapsed = started.elapsed();
            signal.send(()).unwrap();
            (elapsed, statuses, observer.join().unwrap())
        });
        let (search_status, people_status, search_stages, people_stages) = statuses;
        assert_eq!(search_status, [count, 0, 0]);
        assert_eq!(people_status, [count, 0, 0]);
        let started = Instant::now();
        let (search_reused, people_reused, _, _) =
            index_both(scan_id, mode, true, &search, &people);
        let incremental = started.elapsed();
        assert_eq!(search_reused, [count, 0, count]);
        assert_eq!(people_reused, [count, 0, count]);
        let (faces, digest) = snapshot(album.path());
        assert!(faces > 12, "multiface fixture yielded only {faces} faces");
        if let Some(expected) = &fingerprint {
            assert_eq!(&digest, expected);
        }
        println!(
            "trial {trial}: total={:.3}s unchanged={:.1}ms faces={faces} fingerprint={digest}",
            elapsed.as_secs_f64(),
            incremental.as_secs_f64() * 1000.
        );
        println!(
            "stages ms [decode, prepare, inference, write, group]: search {search_stages:.1?}, people {people_stages:.1?}"
        );
        fingerprint = Some(digest);
        totals.push(elapsed);
        preview_samples.extend(preview);
        search_samples.extend(queries);
        *session().unwrap() = None;
    }
    totals.sort_unstable();
    println!(
        "median_total={:.3}s preview_p95={:.3}ms search_p95={:.3}ms samples={}",
        totals[totals.len() / 2].as_secs_f64(),
        p95(&mut preview_samples),
        p95(&mut search_samples),
        search_samples.len()
    );
    #[cfg(all(windows, target_arch = "x86_64"))]
    if gpu {
        crate::ai::device_tests::assert_directml_active(&search, &people);
    }
}

#[test]
#[ignore = "requires existing B/16, SFace and a portrait fixture"]
fn maintenance_stops_both_indexes_and_preserves_completed_work() {
    let _session = TEST_SESSION.lock().unwrap();
    let models = PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_DIR").unwrap());
    let portrait = PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_PORTRAIT").unwrap());
    let (search, people) = cpu_models(&models);
    let (search, people) = (Arc::new(search), Arc::new(people));
    let album = tempfile::tempdir().unwrap();
    let count = 12;
    for index in 0..count {
        fs::copy(
            &portrait,
            album.path().join(format!("portrait-{index:02}.jpg")),
        )
        .unwrap();
    }
    let backup = tempfile::tempdir().unwrap();
    let zip = backup.path().join("album.zip");
    let id = open_directory(album.path().to_str().unwrap(), OpenMode::Select, |_| {})
        .unwrap()
        .scan_id;

    let (search_partial, people_partial) = thread::scope(|scope| {
        let (send, received) = mpsc::channel();
        let progress = |kind: &'static str| {
            let send = Mutex::new(Some(send.clone()));
            move |message| {
                if let tauri::ipc::InvokeResponseBody::Json(json) = message
                    && serde_json::from_str::<serde_json::Value>(&json).unwrap()["indexed"]
                        .as_u64()
                        .unwrap_or(0)
                        > 0
                    && let Some(send) = send.lock().unwrap().take()
                {
                    send.send(kind).unwrap();
                    // Keep both jobs inside the admission guard until maintenance cancels them.
                    let started = Instant::now();
                    while !crate::database_access::maintenance_pending() {
                        assert!(started.elapsed() < Duration::from_secs(15));
                        thread::sleep(Duration::from_millis(5));
                    }
                }
                Ok(())
            }
        };
        let search_progress = Channel::new(progress("search"));
        let people_progress = Channel::new(progress("people"));
        let (search, people) = (&search, &people);
        let search_worker = scope.spawn(move || {
            let _database = crate::database_access::read().unwrap();
            ai_search::update_with_run(
                id,
                false,
                &search_progress,
                &Mode::ConcurrentCpu.into(),
                || Ok(search.clone()),
            )
            .unwrap()
        });
        let people_worker = scope.spawn(move || {
            let _database = crate::database_access::read().unwrap();
            people::update_with_run(
                id,
                false,
                &people_progress,
                &Mode::ConcurrentCpu.into(),
                || Ok(people.clone()),
            )
            .unwrap()
        });
        let first = received.recv_timeout(Duration::from_secs(15)).unwrap();
        assert_ne!(
            first,
            received.recv_timeout(Duration::from_secs(15)).unwrap()
        );
        let _maintenance = crate::database_access::maintenance().unwrap();
        let search = search_worker.join().unwrap();
        let people = people_worker.join().unwrap();
        let album = scan(id).unwrap();
        crate::database_backup::export(&album.root, &zip, |_| {}).unwrap();
        let checked = tempfile::tempdir().unwrap();
        let inspection = crate::database_backup::inspect(&zip, checked.path(), &|_| {}).unwrap();
        people::close_reader().unwrap();
        ai_search::close_reader().unwrap();
        crate::database_backup::restore(
            &album.root,
            &zip,
            &inspection.fingerprint,
            "restore-backup-12345678-1234-1234-1234-123456789abc",
            |_| {},
            |_| Ok(()),
        )
        .unwrap();
        invalidate().unwrap();
        (search.indexed, people.indexed)
    });
    assert!((1..count).contains(&search_partial));
    assert!((1..count).contains(&people_partial));
    assert!(scan(id).is_err());
    let id = open_directory(album.path().to_str().unwrap(), OpenMode::Select, |_| {})
        .unwrap()
        .scan_id;
    let (search_done, people_done, _, _) =
        index_both(id, Mode::ConcurrentCpu, true, &search, &people);
    assert_eq!(search_done, [count, 0, search_partial]);
    assert_eq!(people_done, [count, 0, people_partial]);
    *session().unwrap() = None;
}
