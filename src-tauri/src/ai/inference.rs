use std::path::{Path, PathBuf};
use std::sync::Mutex;

use image::{DynamicImage, RgbImage};
use ort::{
    session::{OutputSelector, RunOptions, Session, SessionOutputs},
    value::{DynValue, Tensor},
};
use tokenizers::{
    Tokenizer, TruncationParams, models::wordpiece::WordPiece, normalizers::bert::BertNormalizer,
    pre_tokenizers::bert::BertPreTokenizer, processors::bert::BertProcessing,
};

#[cfg(test)]
use super::DIM;
#[cfg(test)]
pub(crate) use super::device::initialize_from;
use super::{
    catalog::ModelSpec,
    device::{SessionDevice, SessionStatus},
    error,
};

pub(crate) struct Runtime {
    vision: InferenceSession,
    text: Option<InferenceSession>,
    pub(crate) spec: &'static ModelSpec,
    tokenizer: Tokenizer,
}

pub(crate) fn tokenizer(vocab: &Path) -> Result<Tokenizer, String> {
    let model = WordPiece::from_file(vocab.to_str().ok_or("模型路径无法显示。")?)
        .unk_token("[UNK]".into())
        .build()
        .map_err(error)?;
    let mut tokenizer = Tokenizer::new(model);
    tokenizer.with_normalizer(Some(BertNormalizer::new(true, true, Some(true), true)));
    tokenizer.with_pre_tokenizer(Some(BertPreTokenizer));
    tokenizer.with_post_processor(Some(BertProcessing::new(
        ("[SEP]".into(), 102),
        ("[CLS]".into(), 101),
    )));
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: 52,
            ..Default::default()
        }))
        .map_err(error)?;
    Ok(tokenizer)
}

fn session(path: &Path, device: &SessionDevice, directml: bool) -> Result<Session, String> {
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .saturating_div(2)
        .clamp(1, 4);
    let builder = Session::builder()
        .map_err(error)?
        .with_intra_threads(threads)
        .map_err(error)?
        .with_inter_threads(1)
        .map_err(error)?;
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    let builder = if directml {
        use ort::ep::{DirectML, directml::PerformancePreference};
        builder
            .with_parallel_execution(false)
            .map_err(error)?
            .with_memory_pattern(false)
            .map_err(error)?
            // The app always feeds one sample. Resolve B/16's symbolic batch for DirectML Reshape.
            .with_dimension_override("batch", 1)
            .map_err(error)?
            .with_execution_providers([DirectML::default()
                .with_performance_preference(PerformancePreference::HighPerformance)
                .build()
                .error_on_failure()])
            .map_err(error)?
    } else {
        builder
    };
    #[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
    let builder = if let Some(dir) = &device.profile_dir {
        builder
            .with_profiling(dir.join(path.file_stem().ok_or("缺少模型文件名。")?))
            .map_err(error)?
    } else {
        builder
    };
    let _ = (device, directml);
    let mut builder = builder;
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    if directml {
        let attributes = match path.file_name().and_then(|name| name.to_str()) {
            Some("cn_clip_text.onnx") => 48,
            Some("cn_clip_vision.onnx") => 49,
            _ => 0,
        };
        if attributes != 0 {
            let model = directml_clip_model(path, attributes)?;
            return builder.commit_from_memory(&model).map_err(error);
        }
    }
    builder.commit_from_file(path).map_err(error)
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn directml_clip_model(path: &Path, expected_attributes: usize) -> Result<Vec<u8>, String> {
    // DirectML 1.24.2 rejects the pinned B/16 graphs' Reshape allowzero=1 attribute.
    // B/16 derives these shapes from fixed image/token sizes and batch=1, with no zero dimensions.
    // Normalize the encoded attributes in memory. The pinned asset files and weights stay intact.
    const ALLOW_ZERO: &[u8] = b"\x2a\x10\x0a\x09allowzero\x18\x01\xa0\x01\x02";
    const VALUE_OFFSET: usize = 14;
    let mut model = std::fs::read(path).map_err(error)?;
    // The nodes in both pinned files precede the weights and fit in the first 2 MiB.
    let offsets: Vec<_> = model[..model.len().min(2 * 1024 * 1024)]
        .windows(ALLOW_ZERO.len())
        .enumerate()
        .filter_map(|(offset, bytes)| (bytes == ALLOW_ZERO).then_some(offset + VALUE_OFFSET))
        .collect();
    if offsets.len() != expected_attributes {
        return Err("B/16 模型的 Reshape 属性与固定版本不匹配。".into());
    }
    for offset in offsets {
        model[offset] = 0;
    }
    Ok(model)
}

struct ActiveSession {
    session: Option<Session>,
    status: SessionStatus,
}

pub(super) struct InferenceSession {
    path: PathBuf,
    device: SessionDevice,
    active: Mutex<ActiveSession>,
    inputs: Vec<String>,
}

impl InferenceSession {
    pub(super) fn open(path: PathBuf, device: &SessionDevice) -> Result<Self, String> {
        let mut status = device.status.clone();
        let session = match session(&path, device, status.directml) {
            Err(e) if status.directml => {
                status.directml = false;
                status.fallback = Some(format!("DirectML 会话初始化失败。{e}"));
                session(&path, device, false)?
            }
            result => result?,
        };
        let inputs = session
            .inputs()
            .iter()
            .map(|input| input.name().into())
            .collect();
        Ok(Self {
            path,
            device: device.clone(),
            active: Mutex::new(ActiveSession {
                session: Some(session),
                status,
            }),
            inputs,
        })
    }

    pub(super) fn inputs(&self) -> &[String] {
        &self.inputs
    }

    pub(super) fn status(&self) -> Result<SessionStatus, String> {
        Ok(self.active.lock().map_err(error)?.status.clone())
    }

    pub(super) fn run<T>(
        &self,
        inputs: Vec<(String, DynValue)>,
        output: Option<&str>,
        read: impl Fn(SessionOutputs<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let options = output
            .map(|name| {
                RunOptions::new()
                    .map(|options| options.with_outputs(OutputSelector::no_default().with(name)))
            })
            .transpose()
            .map_err(error)?;
        // Borrow the input tensors so the CPU can retry the same tensors after a GPU failure.
        let execute = |session: &mut Session| {
            let inputs: Vec<_> = inputs
                .iter()
                .map(|(name, value)| (name.as_str(), value))
                .collect();
            match &options {
                Some(options) => session.run_with_options(inputs, options),
                None => session.run(inputs),
            }
            .map_err(error)
            .and_then(&read)
        };

        let mut active = self.active.lock().map_err(error)?;
        if active.session.is_none() {
            active.session = Some(session(&self.path, &self.device, false)?);
        }
        match execute(active.session.as_mut().unwrap()) {
            Err(e) if active.status.directml => {
                active.status.directml = false;
                active.status.fallback = Some(format!("DirectML 推理失败。{e}"));
                // Drop the GPU session before loading CPU weights.
                active.session = None;
                self.device.notify_fallback();
                let mut cpu = session(&self.path, &self.device, false)?;
                let result = execute(&mut cpu);
                active.session = Some(cpu);
                result
            }
            result => result,
        }
    }

    #[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
    pub(super) fn end_profile(&self) -> String {
        self.active
            .lock()
            .unwrap()
            .session
            .as_mut()
            .unwrap()
            .end_profiling()
            .unwrap()
    }
}

#[cfg(test)]
pub(crate) fn normalize(vector: Vec<f32>) -> Result<Vec<f32>, String> {
    normalize_dim(vector, DIM)
}

pub(crate) fn normalize_dim(mut vector: Vec<f32>, dimension: usize) -> Result<Vec<f32>, String> {
    if vector.len() != dimension || vector.iter().any(|v| !v.is_finite()) {
        return Err("模型返回了不兼容的向量。".into());
    }
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= 1e-12 {
        return Err("模型返回了无效的向量。".into());
    }
    for value in &mut vector {
        *value /= norm;
    }
    Ok(vector)
}

/// Matches the pinned ChineseCLIPImageProcessor: RGB, square resize, no center crop.
#[cfg(test)]
pub(crate) fn pixels(image: RgbImage, rotation: u16) -> Vec<f32> {
    pixels_at(image, rotation, 224)
}

fn pixels_at(image: RgbImage, rotation: u16, edge: usize) -> Vec<f32> {
    let image = match rotation {
        90 => image::imageops::rotate90(&image),
        180 => image::imageops::rotate180(&image),
        270 => image::imageops::rotate270(&image),
        _ => image,
    };
    let image = image::imageops::resize(
        &image,
        edge as u32,
        edge as u32,
        image::imageops::FilterType::CatmullRom,
    );
    let mean = [0.48145466, 0.4578275, 0.40821073];
    let std = [0.26862954, 0.261_302_6, 0.275_777_1];
    let mut tensor = vec![0.0; 3 * edge * edge];
    for (index, pixel) in image.pixels().enumerate() {
        for c in 0..3 {
            tensor[c * edge * edge + index] = (pixel[c] as f32 / 255.0 - mean[c]) / std[c];
        }
    }
    tensor
}

impl Runtime {
    #[cfg(test)]
    pub(crate) fn load(directory: &Path) -> Result<Self, String> {
        Self::load_spec(
            directory,
            super::catalog::default_model(super::ModelKind::Search),
        )
    }
    pub(crate) fn load_spec(directory: &Path, spec: &'static ModelSpec) -> Result<Self, String> {
        Self::load_with_device(directory, spec, &SessionDevice::cpu())
    }
    pub(crate) fn load_with_device(
        directory: &Path,
        spec: &'static ModelSpec,
        device: &SessionDevice,
    ) -> Result<Self, String> {
        let combined = spec.backend == "combined";
        let runtime = Self {
            vision: InferenceSession::open(
                directory.join(if combined {
                    "model.onnx"
                } else {
                    "cn_clip_vision.onnx"
                }),
                device,
            )?,
            text: if combined {
                None
            } else {
                Some(InferenceSession::open(
                    directory.join("cn_clip_text.onnx"),
                    device,
                )?)
            },
            tokenizer: tokenizer(&directory.join("vocab.txt"))?,
            spec,
        };
        runtime.encode_text("照片")?;
        runtime.encode_image(
            RgbImage::from_pixel(
                spec.image_size as u32,
                spec.image_size as u32,
                image::Rgb([128, 128, 128]),
            ),
            0,
        )?;
        Ok(runtime)
    }
    pub(super) fn inference_status(&self) -> Result<Vec<SessionStatus>, String> {
        let mut sessions = vec![self.vision.status()?];
        if let Some(text) = &self.text {
            sessions.push(text.status()?);
        }
        Ok(sessions)
    }
    #[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
    pub(crate) fn end_profiles(&self) -> Vec<String> {
        let mut paths = vec![self.vision.end_profile()];
        if let Some(text) = &self.text {
            paths.push(text.end_profile());
        }
        paths
    }
    pub(crate) fn encode_text(&self, query: &str) -> Result<Vec<f32>, String> {
        if query.trim().is_empty() || query.len() > 4096 {
            return Err("请输入不超过 4096 字节的照片描述。".into());
        }
        let encoding = self.tokenizer.encode(query, true).map_err(error)?;
        let mut ids = vec![0_i64; 52];
        let mut mask = vec![0_i64; 52];
        for (i, id) in encoding.get_ids().iter().take(52).enumerate() {
            ids[i] = i64::from(*id);
            mask[i] = 1;
        }
        let session = self.text.as_ref().unwrap_or(&self.vision);
        let mut inputs = Vec::new();
        for name in session.inputs() {
            if name == "pixel_values" {
                let e = self.spec.image_size;
                inputs.push((
                    name.clone(),
                    Tensor::from_array(([1, 3, e, e], vec![0_f32; 3 * e * e]))
                        .map_err(error)?
                        .into_dyn(),
                ));
            } else {
                let values = match name.as_str() {
                    "input_ids" => ids.clone(),
                    "attention_mask" => mask.clone(),
                    "token_type_ids" => vec![0_i64; 52],
                    _ => return Err(format!("不兼容的文字模型输入：{name}")),
                };
                inputs.push((
                    name.clone(),
                    Tensor::from_array(([1_usize, 52], values))
                        .map_err(error)?
                        .into_dyn(),
                ));
            }
        }
        let output = self.text.is_none().then_some("text_embeds");
        session.run(inputs, output, |outputs| {
            normalize_dim(
                outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(error)?
                    .1
                    .to_vec(),
                self.spec.dimension,
            )
        })
    }
    pub(crate) fn prepare_image(&self, image: RgbImage, rotation: u16) -> Vec<f32> {
        pixels_at(image, rotation, self.spec.image_size)
    }
    pub(crate) fn encode_image(&self, image: RgbImage, rotation: u16) -> Result<Vec<f32>, String> {
        self.encode_pixels(self.prepare_image(image, rotation))
    }
    pub(crate) fn encode_pixels(&self, pixels: Vec<f32>) -> Result<Vec<f32>, String> {
        let session = &self.vision;
        let e = self.spec.image_size;
        let mut inputs = vec![(
            "pixel_values".to_owned(),
            Tensor::from_array(([1_usize, 3, e, e], pixels))
                .map_err(error)?
                .into_dyn(),
        )];
        if self.text.is_none() {
            for input in session.inputs() {
                if input == "pixel_values" {
                    continue;
                }
                let values = if input == "attention_mask" {
                    vec![1_i64; 52]
                } else {
                    vec![0_i64; 52]
                };
                inputs.push((
                    input.clone(),
                    Tensor::from_array(([1_usize, 52], values))
                        .map_err(error)?
                        .into_dyn(),
                ));
            }
        } else {
            inputs[0].0 = session.inputs()[0].clone();
        }
        let output = self.text.is_none().then_some("image_embeds");
        session.run(inputs, output, |outputs| {
            normalize_dim(
                outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(error)?
                    .1
                    .to_vec(),
                self.spec.dimension,
            )
        })
    }
}

pub(crate) fn decode_rgb(bytes: &[u8]) -> Result<RgbImage, String> {
    let image = image::load_from_memory(bytes).map_err(error)?;
    Ok(match image {
        DynamicImage::ImageRgb8(image) => image,
        image => image.to_rgb8(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    mod windows {
        use super::*;
        use crate::ai::device;
        use std::fs;

        // ONNX IR 8, opset 13: x: float[3] -> Relu -> y: float[3].
        const RELU: &[u8] = b"\x08\x08\x3a\x37\x0a\x0c\x0a\x01\x78\x12\x01\x79\x22\x04\x52\x65\x6c\x75\x12\x05\x73\x6d\x6f\x6b\x65\x5a\x0f\x0a\x01\x78\x12\x0a\x0a\x08\x08\x01\x12\x04\x0a\x02\x08\x03\x62\x0f\x0a\x01\x79\x12\x0a\x0a\x08\x08\x01\x12\x04\x0a\x02\x08\x03\x42\x02\x10\x0d";

        fn smoke_input() -> Vec<(String, DynValue)> {
            vec![(
                "x".into(),
                Tensor::from_array(([3], vec![-2.0_f32, 0.0, 3.0]))
                    .unwrap()
                    .into_dyn(),
            )]
        }

        fn smoke_output(outputs: SessionOutputs<'_>) -> Result<Vec<f32>, String> {
            Ok(outputs["y"]
                .try_extract_tensor::<f32>()
                .map_err(error)?
                .1
                .to_vec())
        }

        #[test]
        #[ignore = "requires prepared Windows CPU runtime and its native DLL dependencies"]
        fn bundled_cpu_inference_and_directml_fallback() {
            let directory = tempfile::tempdir().unwrap();
            let model = directory.path().join("relu.onnx");
            fs::write(&model, RELU).unwrap();

            assert!(device::load_directml(&directory.path().join("onnxruntime.dll")).is_err());
            initialize_from(
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("binaries/onnxruntime/x86_64/onnxruntime.dll"),
            )
            .unwrap();
            let mut device = SessionDevice::cpu();
            device.status.directml = true;
            let session = InferenceSession::open(model, &device).unwrap();
            assert!(!session.status().unwrap().directml);
            assert!(
                session
                    .status()
                    .unwrap()
                    .fallback
                    .unwrap()
                    .contains("初始化失败")
            );
            assert_eq!(
                session.run(smoke_input(), None, smoke_output).unwrap(),
                [0., 0., 3.]
            );

            // Simulate an execution failure with a session whose graph expects a different input.
            let mut wrong_input = RELU.to_vec();
            for byte in &mut wrong_input {
                if *byte == b'x' {
                    *byte = b'a';
                }
            }
            let wrong_model = directory.path().join("other-input.onnx");
            fs::write(&wrong_model, wrong_input).unwrap();
            {
                let mut active = session.active.lock().unwrap();
                active.session = Some(super::super::session(&wrong_model, &device, false).unwrap());
                active.status.directml = true;
            }
            assert_eq!(
                session.run(smoke_input(), None, smoke_output).unwrap(),
                [0., 0., 3.]
            );
            assert!(!session.status().unwrap().directml);
            assert!(
                session
                    .status()
                    .unwrap()
                    .fallback
                    .unwrap()
                    .contains("推理失败")
            );
            assert_eq!(
                session.run(smoke_input(), None, smoke_output).unwrap(),
                [0., 0., 3.]
            );
        }

        #[test]
        #[ignore = "requires Windows hardware GPU and prepared DirectML runtime"]
        fn bundled_directml_inference_smoke() {
            let directory = tempfile::tempdir().unwrap();
            let model = directory.path().join("relu.onnx");
            fs::write(&model, RELU).unwrap();
            device::load_directml(
                &Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("binaries/onnxruntime/directml/x86_64/onnxruntime.dll"),
            )
            .unwrap();
            let mut device = SessionDevice::cpu();
            device.status.directml = true;
            device.profile_dir = Some(directory.path().to_path_buf());
            let session = InferenceSession::open(model, &device).unwrap();
            assert_eq!(
                session.run(smoke_input(), None, smoke_output).unwrap(),
                [0., 0., 3.]
            );
            super::super::super::device_tests::assert_directml_profile(&session.end_profile());
        }
    }

    #[test]
    fn rejects_invalid_vectors() {
        assert!(normalize(vec![0.0; DIM]).is_err());
        assert!(normalize(vec![f32::NAN; DIM]).is_err());
        let vector = normalize(vec![2.0; DIM]).unwrap();
        assert!((vector.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    #[ignore = "requires PHOTOSYS_AI_REFERENCE generated by scripts/verify-ai-reference.py and downloaded models"]
    fn real_model_reference() {
        let reference_dir = std::path::PathBuf::from(
            std::env::var_os("PHOTOSYS_AI_REFERENCE").expect("PHOTOSYS_AI_REFERENCE"),
        );
        let reference: serde_json::Value =
            serde_json::from_slice(&std::fs::read(reference_dir.join("reference.json")).unwrap())
                .unwrap();
        let model_dir = Path::new(reference["model_directory"].as_str().unwrap());
        super::super::verify_assets(model_dir, &std::sync::atomic::AtomicBool::new(false)).unwrap();
        let name = if cfg!(target_os = "windows") {
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
                .join(name),
        )
        .unwrap();
        let started = std::time::Instant::now();
        let runtime = Runtime::load(model_dir).unwrap();
        println!("model load/self-test: {:?}", started.elapsed());
        let cosine = |actual: &[f32], expected: &serde_json::Value| -> f32 {
            actual
                .iter()
                .zip(expected.as_array().unwrap())
                .map(|(a, b)| a * b.as_f64().unwrap() as f32)
                .sum()
        };
        for entry in reference["texts"].as_array().unwrap() {
            let query = entry["query"].as_str().unwrap();
            let encoding = runtime.tokenizer.encode(query, true).unwrap();
            let mut ids = encoding.get_ids().to_vec();
            ids.resize(52, 0);
            assert_eq!(
                ids,
                entry["ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u32)
                    .collect::<Vec<_>>()
            );
            let vector = runtime.encode_text(query).unwrap();
            assert!(
                cosine(&vector, &entry["vector"]) > 0.99999,
                "text parity: {query}"
            );
        }
        let mut vectors = Vec::new();
        for entry in reference["images"].as_array().unwrap() {
            let image = image::open(entry["path"].as_str().unwrap())
                .unwrap()
                .to_rgb8();
            let reference_pixels: Vec<_> =
                std::fs::read(reference_dir.join(entry["pixels"].as_str().unwrap()))
                    .unwrap()
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                    .collect();
            let actual_pixels = pixels(image.clone(), 0);
            let mean_error: f32 = actual_pixels
                .iter()
                .zip(&reference_pixels)
                .map(|(a, b)| (a - b).abs())
                .sum::<f32>()
                / actual_pixels.len() as f32;
            println!("{} pixel mean error: {}", entry["query"], mean_error);
            assert!(mean_error < 0.02);
            let vector = runtime.encode_pixels(reference_pixels).unwrap();
            assert!(
                cosine(&vector, &entry["vector"]) > 0.99999,
                "ONNX image parity"
            );
            let started = std::time::Instant::now();
            let vector = runtime.encode_image(image, 0).unwrap();
            println!(
                "{} actual image cosine: {}, elapsed: {:?}",
                entry["query"],
                cosine(&vector, &entry["vector"]),
                started.elapsed()
            );
            assert!(
                cosine(&vector, &entry["vector"]) > 0.995,
                "preprocessor image parity"
            );
            vectors.push(vector);
        }
        for (expected, entry) in reference["images"].as_array().unwrap().iter().enumerate() {
            let started = std::time::Instant::now();
            let query = runtime
                .encode_text(entry["query"].as_str().unwrap())
                .unwrap();
            let best = vectors
                .iter()
                .enumerate()
                .map(|(i, v)| (i, v.iter().zip(&query).map(|(a, b)| a * b).sum::<f32>()))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap()
                .0;
            println!(
                "{} rank 1: {}, search: {:?}",
                entry["query"],
                best,
                started.elapsed()
            );
            assert_eq!(best, expected);
        }
    }
}
