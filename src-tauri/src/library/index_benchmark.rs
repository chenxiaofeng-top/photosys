use super::*;
use crate::ai::{
    ModelKind, catalog,
    faces::FaceRuntime,
    indexing::{Measurements, Mode, Run, Timings},
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

fn fixture_album(path: &Path, portrait: &Path, people: &FaceRuntime, count: usize) {
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
    let pattern = fixtures.join("portrait-reference.jpg");
    let heic = fixtures.join("portrait-48mp-grid.heic");
    for index in 0..count {
        let (source, stem, extension, number) = match index % 10 {
            0..=2 => (portrait, "portrait", "jpg", (index / 10) * 3 + index % 10),
            3..=5 => (
                &*pattern,
                "pattern",
                "jpg",
                (index / 10) * 3 + index % 10 - 3,
            ),
            6..=8 => (
                &*group_file,
                "group",
                "jpg",
                (index / 10) * 3 + index % 10 - 6,
            ),
            _ => (&*heic, "high-resolution", "heic", index / 10),
        };
        if stem != "group" || number != 0 {
            fs::copy(source, path.join(format!("{stem}-{number:02}.{extension}"))).unwrap();
        }
    }
}

fn initialize_cpu() {
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
}

fn cpu_models(root: &Path) -> (Runtime, FaceRuntime) {
    initialize_cpu();
    (
        Runtime::load(&root.join(crate::ai::MODEL_ID)).unwrap(),
        FaceRuntime::load(&root.join("yunet-sface-fp32-v1")).unwrap(),
    )
}

fn benchmark_device(gpu: bool) -> crate::ai::device::SessionDevice {
    if gpu {
        #[cfg(all(windows, target_arch = "x86_64"))]
        return crate::ai::device_tests::directml_device();
        #[cfg(not(all(windows, target_arch = "x86_64")))]
        panic!("DirectML benchmark requires Windows x64");
    }
    initialize_cpu();
    crate::ai::device::SessionDevice::cpu()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Workload {
    Search,
    People,
    Both,
}

struct JobResult {
    counts: [usize; 3],
    elapsed: Duration,
    stages: Measurements,
}

fn index(
    id: u64,
    mode: Mode,
    automatic: bool,
    search: &Arc<Runtime>,
    people: &Arc<FaceRuntime>,
    workload: Workload,
) -> (JobResult, JobResult) {
    let search_run = Run {
        mode,
        timings: Some(Timings::default()),
    };
    let people_run = Run {
        mode,
        timings: Some(Timings::default()),
    };
    let index_search = || {
        let started = Instant::now();
        let _database = crate::database_access::read().unwrap();
        let status = ai_search::update_with_run(
            id,
            automatic,
            &Channel::new(|_| Ok(())),
            &search_run,
            || Ok(search.clone()),
        )
        .unwrap();
        (
            [status.indexed, status.failed, status.reused],
            started.elapsed(),
        )
    };
    let index_people = || {
        let started = Instant::now();
        let _database = crate::database_access::read().unwrap();
        let status = people::update_with_run(
            id,
            automatic,
            &Channel::new(|_| Ok(())),
            &people_run,
            || Ok(people.clone()),
        )
        .unwrap();
        (
            [status.indexed, status.failed, status.reused],
            started.elapsed(),
        )
    };
    let ((search, search_elapsed), (people, people_elapsed)) = match workload {
        Workload::Search => (index_search(), ([0; 3], Duration::ZERO)),
        Workload::People => (([0; 3], Duration::ZERO), index_people()),
        Workload::Both if matches!(mode, Mode::ConcurrentCpu | Mode::ConcurrentDirectMl) => {
            thread::scope(|scope| {
                let worker = scope.spawn(index_people);
                (index_search(), worker.join().unwrap())
            })
        }
        Workload::Both => (index_search(), index_people()),
    };
    (
        JobResult {
            counts: search,
            elapsed: search_elapsed,
            stages: search_run.timings.unwrap().measurements(),
        },
        JobResult {
            counts: people,
            elapsed: people_elapsed,
            stages: people_run.timings.unwrap().measurements(),
        },
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
            item.preview_available
                && item.kind == Kind::Photo
                && item.file.relative.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg")
                })
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
        if scan.preview(preview_id, false).is_ok() {
            preview_times.push(started.elapsed().as_secs_f64() * 1000.);
        }
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

fn snapshot(data: &Path, workload: Workload) -> (usize, String) {
    let mut hash = Sha256::new();
    if workload != Workload::People {
        let search = Connection::open(data.join("ai.sqlite")).unwrap();
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
    }
    if workload == Workload::Search {
        return (0, format!("{:x}", hash.finalize()));
    }
    let people = Connection::open(data.join("people.sqlite")).unwrap();
    let mut statement = people
        .prepare("SELECT path,person_id,vector,bbox,confidence,thumbnail FROM faces WHERE active=1 ORDER BY path,id")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, f64>(4)?,
                row.get::<_, Vec<u8>>(5)?,
            ))
        })
        .unwrap();
    let mut faces = 0;
    for row in rows {
        let (path, person, vector, bbox, confidence, thumbnail) = row.unwrap();
        hash.update(path);
        hash.update(person.unwrap_or_default().to_le_bytes());
        hash.update(vector.unwrap_or_default());
        hash.update(bbox.as_bytes());
        hash.update(confidence.to_le_bytes());
        hash.update(thumbnail);
        faces += 1;
    }
    (faces, format!("{:x}", hash.finalize()))
}

fn album_manifest(scan: &Scan) -> Vec<(String, String)> {
    let mut photos: Vec<_> = scan
        .items
        .iter()
        .filter(|item| item.kind == Kind::Photo && item.preview_available)
        .map(|item| {
            let stamp = item.file.stamp.cache_key();
            (
                item.file.path_label(),
                format!("{:x}", Sha256::digest(&stamp)),
            )
        })
        .collect();
    photos.sort_unstable();
    photos
}

fn select_benchmark_model(kind: ModelKind, name: &str) -> &'static catalog::ModelSpec {
    let model = std::env::var(name)
        .map(|id| catalog::find(kind, &id).unwrap())
        .unwrap_or_else(|_| catalog::default_model(kind));
    catalog::select(model);
    model
}

#[test]
#[ignore = "requires installed models; run in release mode in a separate process per configuration"]
fn benchmark_index_pipeline() {
    let _session = TEST_SESSION.lock().unwrap();
    let models = PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_DIR").unwrap());
    let real_album = std::env::var_os("PHOTOSYS_AI_BENCHMARK_ALBUM").map(PathBuf::from);
    let data_parent = real_album.as_ref().map(|album| {
        let parent = PathBuf::from(std::env::var_os("PHOTOSYS_AI_BENCHMARK_OUTPUT").unwrap())
            .canonicalize()
            .unwrap();
        assert!(!parent.starts_with(album.canonicalize().unwrap()));
        parent
    });
    let portrait = real_album
        .is_none()
        .then(|| PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_PORTRAIT").unwrap()));
    let search_model =
        select_benchmark_model(ModelKind::Search, "PHOTOSYS_AI_BENCHMARK_SEARCH_MODEL");
    let people_model =
        select_benchmark_model(ModelKind::People, "PHOTOSYS_AI_BENCHMARK_PEOPLE_MODEL");
    let gpu = std::env::var("PHOTOSYS_AI_BENCHMARK_BACKEND").as_deref() == Ok("directml");
    let mode = match std::env::var("PHOTOSYS_AI_BENCHMARK_MODE").as_deref() {
        Ok("serial") => Mode::Serial,
        Ok("pipeline") => Mode::Pipeline,
        Ok("concurrent") if gpu => Mode::ConcurrentDirectMl,
        Ok("concurrent") => Mode::ConcurrentCpu,
        _ => panic!("Set PHOTOSYS_AI_BENCHMARK_MODE to serial, pipeline, or concurrent"),
    }
    .for_active_models();
    let trials = std::env::var("PHOTOSYS_AI_BENCHMARK_TRIALS")
        .ok()
        .map_or(3, |value| value.parse::<usize>().unwrap());
    assert!(trials > 0);
    let count = std::env::var("PHOTOSYS_AI_BENCHMARK_PHOTOS")
        .ok()
        .map_or(40, |value| value.parse::<usize>().unwrap());
    assert!(count >= 40 && count.is_multiple_of(10));
    let workload = match std::env::var("PHOTOSYS_AI_BENCHMARK_WORKLOAD").as_deref() {
        Ok("search") => Workload::Search,
        Ok("people") => Workload::People,
        Ok("both") | Err(_) => Workload::Both,
        _ => panic!("Set PHOTOSYS_AI_BENCHMARK_WORKLOAD to search, people, or both"),
    };
    println!(
        "pid={} mode={mode:?} workload={workload:?} synthetic_photos={count} requested_backend={} threads={} search_model={} people_model={}",
        std::process::id(),
        if gpu { "DirectML" } else { "CPU" },
        thread::available_parallelism().unwrap(),
        search_model.id,
        people_model.id,
    );
    let load_started = Instant::now();
    let device = benchmark_device(gpu);
    let search = Arc::new(
        Runtime::load_with_device(&models.join(search_model.id), search_model, &device).unwrap(),
    );
    let search_load = load_started.elapsed();
    let started = Instant::now();
    let people = Arc::new(
        FaceRuntime::load_with_device(&models.join(people_model.id), people_model, &device)
            .unwrap(),
    );
    let people_load = started.elapsed();
    println!(
        "model_load_and_warmup={:.3}s search_load={:.3}s people_load={:.3}s",
        load_started.elapsed().as_secs_f64(),
        search_load.as_secs_f64(),
        people_load.as_secs_f64(),
    );
    let mut totals = Vec::new();
    let mut preview_samples = Vec::new();
    let mut search_samples = Vec::new();
    let mut fingerprint = None;
    for trial in 1..=trials {
        let synthetic_album = portrait.as_ref().map(|portrait| {
            let album = tempfile::tempdir().unwrap();
            fixture_album(album.path(), portrait, &people, count);
            album
        });
        let scan_started = Instant::now();
        let (summary, data) =
            if let Some((album, parent)) = real_album.as_ref().zip(data_parent.as_ref()) {
                let data = parent.join(format!("trial-{trial}"));
                fs::create_dir(&data).expect("The benchmark requires a fresh data directory");
                let mut root = Root::new(album).unwrap();
                root.test_data_directory = Some(data.clone());
                let keys = favorites::initialize(&root).unwrap();
                (open_library(root, keys, |_| {}).unwrap(), data)
            } else {
                let album = synthetic_album.as_ref().unwrap().path();
                (
                    open_directory(album.to_str().unwrap(), OpenMode::Select, |_| {}).unwrap(),
                    album.join(".photosys"),
                )
            };
        let scan_id = summary.scan_id;
        let scanned = scan(scan_id).unwrap();
        let manifest = album_manifest(&scanned);
        let eligible = manifest.len();
        assert!(eligible > 0);
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        let manifest_digest = Sha256::digest(&manifest_bytes);
        if real_album.is_some() {
            fs::write(data.join("photos.json"), manifest_bytes).unwrap();
        }
        println!(
            "scan={:.3}s media={} issues={} eligible_photos={eligible} manifest={manifest_digest:x}",
            scan_started.elapsed().as_secs_f64(),
            summary.total,
            summary.issues,
        );
        println!("index_started trial={trial}");
        #[cfg(windows)]
        let codec_cpu_before = crate::media_codec::benchmark_cpu_micros();
        let (elapsed, (search_job, people_job), foreground_samples) = if real_album.is_some() {
            let started = Instant::now();
            let statuses = index(scan_id, mode, false, &search, &people, workload);
            (started.elapsed(), statuses, None)
        } else {
            let (signal, stop) = mpsc::channel();
            thread::scope(|scope| {
                let observer = scope.spawn(|| foreground(scanned.clone(), &search, stop));
                let started = Instant::now();
                let statuses = index(scan_id, mode, false, &search, &people, workload);
                let elapsed = started.elapsed();
                signal.send(()).unwrap();
                (elapsed, statuses, Some(observer.join().unwrap()))
            })
        };
        println!(
            "index_finished trial={trial} total={:.3}s search={:.3}s people={:.3}s search_counts={:?} people_counts={:?}",
            elapsed.as_secs_f64(),
            search_job.elapsed.as_secs_f64(),
            people_job.elapsed.as_secs_f64(),
            search_job.counts,
            people_job.counts,
        );
        #[cfg(windows)]
        println!(
            "codec_cpu_seconds={:.3}",
            (crate::media_codec::benchmark_cpu_micros() - codec_cpu_before) as f64 / 1e6,
        );
        let started = Instant::now();
        let (search_reused, people_reused) = index(scan_id, mode, true, &search, &people, workload);
        let incremental = started.elapsed();
        for (run, reused) in [(&search_job, &search_reused), (&people_job, &people_reused)] {
            assert_eq!(reused.stages.decodes, [0, 0]);
            let [indexed, failed, reused_before] = run.counts;
            assert_eq!(reused_before, 0);
            assert_eq!(reused.counts, [indexed, failed, indexed]);
        }
        for run in [
            (workload != Workload::People).then_some(&search_job),
            (workload != Workload::Search).then_some(&people_job),
        ]
        .into_iter()
        .flatten()
        {
            assert_eq!(run.counts[0] + run.counts[1], eligible);
            if real_album.is_none() {
                assert_eq!(run.counts, [count, 0, 0]);
            }
        }
        if real_album.is_some() {
            let current = Scan::read(scan_id, scanned.root.clone(), |_| {});
            assert_eq!(
                manifest,
                album_manifest(&current),
                "Album changed during indexing"
            );
        }
        let (faces, digest) = snapshot(&data, workload);
        assert!(
            real_album.is_some() || workload == Workload::Search || faces > count * 3 / 10,
            "multiface fixture yielded only {faces} faces"
        );
        if let Some(expected) = &fingerprint {
            assert_eq!(&digest, expected);
        }
        println!(
            "trial {trial}: total={:.3}s unchanged={:.1}ms faces={faces} fingerprint={digest}",
            elapsed.as_secs_f64(),
            incremental.as_secs_f64() * 1000.
        );
        println!(
            "stages ms [decode, prepare, inference, write, group, decode wait, GPU wait, model load]: search {:.1?}, people {:.1?}; decode [actual, reused]: search {:?}, people {:?}",
            search_job.stages.millis,
            people_job.stages.millis,
            search_job.stages.decodes,
            people_job.stages.decodes
        );
        fingerprint = Some(digest);
        totals.push(elapsed);
        if let Some((preview, queries)) = foreground_samples {
            preview_samples.extend(preview);
            search_samples.extend(queries);
        }
        *session().unwrap() = None;
    }
    totals.sort_unstable();
    println!(
        "median_total={:.3}s preview_p95={:.3}ms search_p95={:.3}ms samples={}",
        totals[totals.len() / 2].as_secs_f64(),
        if preview_samples.is_empty() {
            f64::NAN
        } else {
            p95(&mut preview_samples)
        },
        if search_samples.is_empty() {
            f64::NAN
        } else {
            p95(&mut search_samples)
        },
        search_samples.len()
    );
    println!(
        "actual_backends={}",
        serde_json::json!({
            "search": search.inference_status().unwrap(),
            "people": people.inference_status().unwrap(),
        })
    );
    println!("fixed_clip_shape={}", search.uses_fixed_image_shape());
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
    let (search_done, people_done) = index(
        id,
        Mode::ConcurrentCpu,
        true,
        &search,
        &people,
        Workload::Both,
    );
    assert_eq!(search_done.counts, [count, 0, search_partial]);
    assert_eq!(people_done.counts, [count, 0, people_partial]);
    *session().unwrap() = None;
}
