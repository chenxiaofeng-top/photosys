use super::*;
use std::{fs, io::Write, path::Path, sync::atomic::AtomicBool, time::Instant};
use tauri::ipc::Channel;

#[test]
#[ignore = "requires PHOTOSYS_MODEL_OPTIONS_DIR with all five verified model directories"]
fn real_model_options_install_infer_and_reference() {
    let root = std::path::PathBuf::from(
        std::env::var_os("PHOTOSYS_MODEL_OPTIONS_DIR").expect("model directory"),
    );
    let references =
        std::env::var_os("PHOTOSYS_MODEL_OPTIONS_REFERENCE").map(std::path::PathBuf::from);
    let name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    inference::initialize_from(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/onnxruntime")
            .join(std::env::consts::ARCH)
            .join(name),
    )
    .unwrap();
    let mut metrics = Vec::new();
    let parity = |actual: &[f32], expected: &serde_json::Value| {
        assert_eq!(actual.len(), expected.as_array().unwrap().len());
        let cosine: f32 = actual
            .iter()
            .zip(expected.as_array().unwrap())
            .map(|(a, b)| a * b.as_f64().unwrap() as f32)
            .sum();
        assert!(cosine > 0.99999, "ONNX tensor parity {cosine}");
    };
    for spec in catalog::all() {
        let started = Instant::now();
        model::verify_spec(&root.join(spec.id), spec).unwrap();
        let scratch =
            tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("target")).unwrap();
        let zip_path = scratch.path().join("model.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        for a in &spec.assets {
            zip.start_file(
                a.name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            std::io::copy(
                &mut fs::File::open(root.join(spec.id).join(a.name)).unwrap(),
                &mut zip,
            )
            .unwrap();
        }
        zip.start_file(
            "photosys-model.json",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(
            serde_json::json!({"schema":1,"modelId":spec.id})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap();
        let reference = references.as_ref().map(|p| {
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(p.join(spec.id).join("reference.json")).unwrap(),
            )
            .unwrap()
        });
        let staged = scratch.path().join(format!(".{}.partial", spec.id));
        let mut result = serde_json::json!({"modelId":spec.id});
        if spec.kind == ModelKind::Search {
            let (_, runtime) = packages::install(
                scratch.path(),
                spec.id,
                &spec.assets,
                &AtomicBool::new(false),
                Source::Official,
                Some(&zip_path),
                &Channel::new(|_| Ok(())),
                include_str!("../../../scripts/AI-MODEL-NOTICES.md"),
                || inference::Runtime::load_spec(&staged, spec),
                || Ok(()),
            )
            .unwrap();
            let start = Instant::now();
            let text = runtime.encode_text("海边日落").unwrap();
            result["queryMs"] = serde_json::json!(start.elapsed().as_secs_f64() * 1000.);
            assert_eq!(text.len(), spec.dimension);
            let start = Instant::now();
            let image = runtime
                .encode_image(
                    image::RgbImage::from_pixel(96, 64, image::Rgb([40, 120, 200])),
                    90,
                )
                .unwrap();
            result["imageMs"] = serde_json::json!(start.elapsed().as_secs_f64() * 1000.);
            assert_eq!(image.len(), spec.dimension);
            if let Some(reference) = reference.as_ref() {
                for t in reference["texts"].as_array().unwrap() {
                    parity(
                        &runtime.encode_text(t["query"].as_str().unwrap()).unwrap(),
                        &t["vector"],
                    );
                }
                let mut vectors = Vec::new();
                for entry in reference["images"].as_array().unwrap() {
                    let pixels = fs::read(entry["pixels"].as_str().unwrap())
                        .unwrap()
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                        .collect();
                    parity(&runtime.encode_pixels(pixels).unwrap(), &entry["vector"]);
                    let decoded = image::open(entry["path"].as_str().unwrap())
                        .unwrap()
                        .to_rgb8();
                    let v = runtime.encode_image(decoded, 0).unwrap();
                    let expected: Vec<f32> = entry["vector"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_f64().unwrap() as f32)
                        .collect();
                    assert!(
                        v.iter().zip(&expected).map(|(a, b)| a * b).sum::<f32>() > 0.995,
                        "image preprocessing parity"
                    );
                    vectors.push(v);
                }
                for (i, entry) in reference["images"].as_array().unwrap().iter().enumerate() {
                    let query = runtime
                        .encode_text(entry["query"].as_str().unwrap())
                        .unwrap();
                    let best = vectors
                        .iter()
                        .enumerate()
                        .map(|(j, v)| (j, v.iter().zip(&query).map(|(a, b)| a * b).sum::<f32>()))
                        .max_by(|a, b| a.1.total_cmp(&b.1))
                        .unwrap()
                        .0;
                    assert_eq!(best, i);
                }
            }
        } else {
            let (_, runtime) = packages::install(
                scratch.path(),
                spec.id,
                &spec.assets,
                &AtomicBool::new(false),
                Source::Official,
                Some(&zip_path),
                &Channel::new(|_| Ok(())),
                include_str!("../../../scripts/PEOPLE-MODEL-NOTICES.md"),
                || faces::FaceRuntime::load_spec(&staged, spec),
                || Ok(()),
            )
            .unwrap();
            let start = Instant::now();
            let vector = runtime
                .encode(&image::RgbImage::from_pixel(
                    112,
                    112,
                    image::Rgb([40, 120, 200]),
                ))
                .unwrap();
            result["faceMs"] = serde_json::json!(start.elapsed().as_secs_f64() * 1000.);
            assert_eq!(vector.len(), spec.dimension);
            assert!(
                runtime
                    .analyze(image::RgbImage::new(640, 640), 0, &AtomicBool::new(false))
                    .unwrap()
                    .is_empty()
            );
            if let Some(reference) = reference.as_ref() {
                for entry in reference["faces"].as_array().unwrap() {
                    parity(
                        &runtime
                            .encode(
                                &image::open(entry["path"].as_str().unwrap())
                                    .unwrap()
                                    .to_rgb8(),
                            )
                            .unwrap(),
                        &entry["vector"],
                    );
                }
            }
        }
        assert!(scratch.path().join(spec.id).join("README.txt").is_file());
        assert!(!staged.exists());
        result["installAndChecksSeconds"] = serde_json::json!(started.elapsed().as_secs_f64());
        println!("{}", result);
        metrics.push(result);
    }
    if let Some(path) = std::env::var_os("PHOTOSYS_MODEL_OPTIONS_METRICS") {
        fs::write(path, serde_json::to_vec_pretty(&metrics).unwrap()).unwrap();
    }
}

#[test]
fn wrong_model_offline_package_preserves_existing_installation() {
    let base = tempfile::tempdir().unwrap();
    let spec = &catalog::all()[1];
    let installed = base.path().join(spec.id);
    fs::create_dir(&installed).unwrap();
    fs::write(installed.join("keep"), b"old model").unwrap();
    let path = base.path().join("wrong.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    zip.start_file(
        "photosys-model.json",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(br#"{"schema":1,"modelId":"different-model"}"#)
        .unwrap();
    zip.finish().unwrap();
    let result = packages::install(
        base.path(),
        spec.id,
        &spec.assets,
        &AtomicBool::new(false),
        Source::Official,
        Some(&path),
        &Channel::new(|_| Ok(())),
        "license",
        || Ok(()),
        || panic!("must not publish wrong model"),
    );
    assert!(result.unwrap_err().contains("不匹配"));
    assert_eq!(fs::read(installed.join("keep")).unwrap(), b"old model");
}

#[test]
fn disk_budget_and_cancel_never_replace_an_existing_model() {
    let base = tempfile::tempdir().unwrap();
    let installed = base.path().join("test-model");
    fs::create_dir(&installed).unwrap();
    fs::write(installed.join("keep"), b"existing").unwrap();
    let oversized = [model::Asset {
        name: "large.onnx",
        path: "large.onnx",
        size: u64::MAX / 2,
        sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        repository: "fixture/models",
        revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    }];
    let result = packages::install(
        base.path(),
        "test-model",
        &oversized,
        &AtomicBool::new(false),
        Source::Official,
        None,
        &Channel::new(|_| Ok(())),
        "license",
        || Ok(()),
        || panic!("must not publish"),
    );
    assert!(result.unwrap_err().contains("空间不足"));
    let small = [model::Asset {
        size: 5,
        ..oversized[0]
    }];
    let result = packages::install(
        base.path(),
        "test-model",
        &small,
        &AtomicBool::new(true),
        Source::Official,
        None,
        &Channel::new(|_| Ok(())),
        "license",
        || Ok(()),
        || panic!("must not publish"),
    );
    assert!(result.unwrap_err().contains("取消"));
    assert_eq!(fs::read(installed.join("keep")).unwrap(), b"existing");
}
#[test]
fn failed_inference_self_test_preserves_installed_model() {
    use sha2::{Digest, Sha256};
    let base = tempfile::tempdir().unwrap();
    let installed = base.path().join("fixture");
    fs::create_dir(&installed).unwrap();
    fs::write(installed.join("keep"), b"old").unwrap();
    let zip_path = base.path().join("fixture.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
    zip.start_file("fixture.onnx", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"valid bytes").unwrap();
    zip.finish().unwrap();
    let sha: &'static str =
        Box::leak(format!("{:x}", Sha256::digest(b"valid bytes")).into_boxed_str());
    let assets = [model::Asset {
        name: "fixture.onnx",
        path: "fixture.onnx",
        size: 11,
        sha,
        repository: "fixture/models",
        revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    }];
    let result: Result<(packages::Stamps, ()), String> = packages::install(
        base.path(),
        "fixture",
        &assets,
        &AtomicBool::new(false),
        Source::Official,
        Some(&zip_path),
        &Channel::new(|_| Ok(())),
        "license",
        || Err("inference self-test failed".into()),
        || panic!("must not publish"),
    );
    assert!(result.unwrap_err().contains("self-test failed"));
    assert_eq!(fs::read(installed.join("keep")).unwrap(), b"old");
}
