use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

use image::{Rgb, RgbImage};

use super::{
    ModelKind, catalog,
    device::{SessionDevice, load_directml},
    faces::{self, FaceRuntime},
    inference::Runtime,
};

pub(super) fn assert_directml_profile(path: &str) {
    let profile: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let gpu_nodes = profile
        .iter()
        .filter(|event| {
            event["cat"] == "Node" && event["args"]["provider"] == "DmlExecutionProvider"
        })
        .count();
    assert!(gpu_nodes > 0, "No DirectML nodes executed in {path}");
    println!("{path}: {gpu_nodes} DirectML node executions");
}

pub(crate) fn directml_test_device(profile_dir: &Path) -> SessionDevice {
    load_directml(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/onnxruntime/directml/x86_64/onnxruntime.dll"),
    )
    .unwrap();
    let mut device = SessionDevice::cpu();
    device.status.directml = true;
    device.profile_dir = Some(profile_dir.to_owned());
    device
}

pub(crate) fn assert_directml_profiles(paths: Vec<String>) {
    for path in paths {
        assert_directml_profile(&path);
        std::fs::remove_file(path).unwrap();
    }
}

pub(crate) fn benchmark_indexes(
    portrait: &Path,
    mut index: impl FnMut(&Path, bool) -> (usize, usize),
) {
    const PHOTOS: usize = 8;
    for (label, gpu) in [("CPU", false), ("DirectML", true)] {
        let album = tempfile::tempdir().unwrap();
        for photo in 0..PHOTOS {
            std::fs::copy(portrait, album.path().join(format!("{photo}.jpg"))).unwrap();
        }
        let start = Instant::now();
        let (indexed, failed) = index(album.path(), gpu);
        assert_eq!((indexed, failed), (PHOTOS, 0));
        println!("{label} fresh index: {:.2} photos/s ({indexed} photos)", indexed as f64 / start.elapsed().as_secs_f64());
    }
}

fn parity(cpu: &[f32], gpu: &[f32]) {
    assert_eq!(cpu.len(), gpu.len());
    let similarity = faces::cosine(cpu, gpu);
    assert!(
        similarity > 0.9999,
        "CPU/GPU embedding cosine: {similarity}"
    );
}

struct SearchResults {
    images: Vec<Vec<f32>>,
    texts: Vec<Vec<f32>>,
}

fn search_results(runtime: &Runtime, photo: &RgbImage, label: &str) -> SearchResults {
    let photos = [
        photo.clone(),
        RgbImage::from_fn(256, 256, |x, y| Rgb([0, (x ^ y) as u8, 255])),
        RgbImage::from_pixel(256, 256, Rgb([255, 0, 0])),
    ];
    let started = Instant::now();
    let images = photos
        .into_iter()
        .map(|photo| runtime.encode_image(photo, 0).unwrap())
        .collect();
    let image_time = started.elapsed();
    let started = Instant::now();
    let texts = ["人像照片", "蓝色方格", "红色背景"]
        .into_iter()
        .map(|query| runtime.encode_text(query).unwrap())
        .collect();
    println!(
        "{label}: three photos {image_time:?}, three queries {:?}",
        started.elapsed()
    );
    SearchResults { images, texts }
}

fn rankings(text: &[Vec<f32>], images: &[Vec<f32>]) -> Vec<Vec<usize>> {
    text.iter()
        .map(|query| {
            let mut scores: Vec<_> = images
                .iter()
                .enumerate()
                .map(|(i, vector)| (i, faces::cosine(vector, query)))
                .collect();
            scores.sort_by(|a, b| b.1.total_cmp(&a.1));
            scores.into_iter().map(|(i, _)| i).collect()
        })
        .collect()
}

#[test]
#[ignore = "requires real models, an OpenCV test portrait, and a Windows hardware GPU"]
fn real_directml_model_parity() {
    let models = PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_DIR").unwrap());
    let portrait = PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_PORTRAIT").unwrap());
    let selected = std::env::var("PHOTOSYS_DIRECTML_MODEL_ID").ok();
    let photo = image::open(&portrait).unwrap().to_rgb8();
    let profiles = tempfile::tempdir().unwrap();
    let gpu = directml_test_device(profiles.path());
    let mut tested = 0;

    for spec in catalog::all() {
        if selected.as_deref().is_some_and(|id| id != spec.id) {
            continue;
        }
        let directory = models.join(spec.id);
        super::model::verify_spec(&directory, spec).unwrap();
        println!("{} ({})", spec.name, spec.id);
        let paths = match spec.kind {
            ModelKind::Search => {
                let cpu = Runtime::load_spec(&directory, spec).unwrap();
                let baseline = search_results(&cpu, &photo, "CPU");
                drop(cpu);
                let directml = Runtime::load_with_device(&directory, spec, &gpu).unwrap();
                let accelerated = search_results(&directml, &photo, "DirectML");
                for (cpu, gpu) in baseline
                    .images
                    .iter()
                    .chain(&baseline.texts)
                    .zip(accelerated.images.iter().chain(&accelerated.texts))
                {
                    parity(cpu, gpu);
                }
                let expected = rankings(&baseline.texts, &baseline.images);
                assert_eq!(expected, rankings(&accelerated.texts, &baseline.images));
                assert_eq!(expected, rankings(&baseline.texts, &accelerated.images));
                assert_eq!(expected, rankings(&accelerated.texts, &accelerated.images));
                directml.end_profiles()
            }
            ModelKind::People => {
                let cpu = FaceRuntime::load_spec(&directory, spec).unwrap();
                let started = Instant::now();
                let baseline = cpu
                    .analyze(photo.clone(), 0, &AtomicBool::new(false))
                    .unwrap();
                println!("CPU detection and recognition: {:?}", started.elapsed());
                assert!(
                    !baseline.is_empty(),
                    "Expected a face in the reference portrait"
                );
                drop(cpu);
                let directml = FaceRuntime::load_with_device(&directory, spec, &gpu).unwrap();
                let started = Instant::now();
                let detected = directml
                    .analyze(photo.clone(), 0, &AtomicBool::new(false))
                    .unwrap();
                println!(
                    "DirectML detection and recognition: {:?}",
                    started.elapsed()
                );
                assert_eq!(baseline.len(), detected.len());
                for (cpu, gpu) in baseline.iter().zip(&detected) {
                    for (cpu, gpu) in cpu.bbox.into_iter().zip(gpu.bbox) {
                        assert!((cpu - gpu).abs() < 0.001);
                    }
                    parity(
                        cpu.vector.as_deref().unwrap(),
                        gpu.vector.as_deref().unwrap(),
                    );
                }
                directml.end_profiles()
            }
        };
        assert_directml_profiles(paths);
        tested += 1;
    }
    assert!(tested > 0, "Unknown model filter {selected:?}");
}
