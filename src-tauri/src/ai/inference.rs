use std::path::Path;
use std::sync::{Mutex, OnceLock};

use image::{DynamicImage, RgbImage};
use ort::{session::Session, value::Tensor};
use tauri::{AppHandle, Manager};
use tokenizers::{
    Tokenizer, TruncationParams, models::wordpiece::WordPiece, normalizers::bert::BertNormalizer,
    pre_tokenizers::bert::BertPreTokenizer, processors::bert::BertProcessing,
};

use super::{DIM, error};

static INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();

pub(crate) fn initialize(app: &AppHandle) -> Result<(), String> {
    INITIALIZED.get_or_init(|| {
        let directory = format!("onnxruntime/{}", std::env::consts::ARCH);
        let name = if cfg!(target_os = "windows") { "onnxruntime.dll" }
            else if cfg!(target_os = "macos") { "libonnxruntime.dylib" }
            else { "libonnxruntime.so" };
        let resource = app.path().resource_dir().map_err(error)?.join(&directory).join(name);
        let path = if resource.is_file() { resource } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries").join(directory).join(name)
        };
        if !path.is_file() { return Err("缺少随软件提供的 AI 运行时，请重新安装 PhotoSys。开发环境请先执行 python3 scripts/prepare-ai-runtime.py。".into()); }
        initialize_from(&path)
    }).clone()
}

pub(crate) fn initialize_from(path: &Path) -> Result<(), String> {
    ort::init_from(path)
        .map_err(error)?
        .with_telemetry(false)
        .with_name("PhotoSys local search")
        .commit();
    Ok(())
}

pub(crate) struct Runtime {
    vision: Mutex<Session>,
    text: Mutex<Session>,
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

fn session(path: &Path) -> Result<Session, String> {
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .saturating_div(2)
        .clamp(1, 4);
    Session::builder()
        .map_err(error)?
        .with_intra_threads(threads)
        .map_err(error)?
        .with_inter_threads(1)
        .map_err(error)?
        .commit_from_file(path)
        .map_err(error)
}

pub(crate) fn normalize(mut vector: Vec<f32>) -> Result<Vec<f32>, String> {
    if vector.len() != DIM || vector.iter().any(|v| !v.is_finite()) {
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
pub(crate) fn pixels(image: RgbImage, rotation: u16) -> Vec<f32> {
    let image = match rotation {
        90 => image::imageops::rotate90(&image),
        180 => image::imageops::rotate180(&image),
        270 => image::imageops::rotate270(&image),
        _ => image,
    };
    let image = image::imageops::resize(&image, 224, 224, image::imageops::FilterType::CatmullRom);
    let mean = [0.48145466, 0.4578275, 0.40821073];
    let std = [0.26862954, 0.261_302_6, 0.275_777_1];
    let mut tensor = vec![0.0; 3 * 224 * 224];
    for (index, pixel) in image.pixels().enumerate() {
        for c in 0..3 {
            tensor[c * 224 * 224 + index] = (pixel[c] as f32 / 255.0 - mean[c]) / std[c];
        }
    }
    tensor
}

impl Runtime {
    pub(crate) fn load(directory: &Path) -> Result<Self, String> {
        let runtime = Self {
            vision: Mutex::new(session(&directory.join("cn_clip_vision.onnx"))?),
            text: Mutex::new(session(&directory.join("cn_clip_text.onnx"))?),
            tokenizer: tokenizer(&directory.join("vocab.txt"))?,
        };
        runtime.encode_text("照片")?;
        runtime.encode_image(
            RgbImage::from_pixel(224, 224, image::Rgb([128, 128, 128])),
            0,
        )?;
        Ok(runtime)
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
        let types = vec![0_i64; 52];
        let mut session = self
            .text
            .lock()
            .map_err(|_| "文字推理状态异常，请重启软件。")?;
        // Exported models differ in whether the all-zero segment IDs remain as an input.
        let names: Vec<_> = session
            .inputs()
            .iter()
            .map(|input| input.name().to_owned())
            .collect();
        let mut inputs = Vec::new();
        for name in names {
            let values = match name.as_str() {
                "input_ids" => ids.clone(),
                "attention_mask" => mask.clone(),
                "token_type_ids" => types.clone(),
                _ => return Err(format!("不兼容的文字模型输入：{name}")),
            };
            inputs.push((
                name,
                Tensor::from_array(([1_usize, 52], values))
                    .map_err(error)?
                    .into_dyn(),
            ));
        }
        let outputs = session.run(inputs).map_err(error)?;
        normalize(
            outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(error)?
                .1
                .to_vec(),
        )
    }

    pub(crate) fn encode_image(&self, image: RgbImage, rotation: u16) -> Result<Vec<f32>, String> {
        self.encode_pixels(pixels(image, rotation))
    }

    pub(crate) fn encode_pixels(&self, pixels: Vec<f32>) -> Result<Vec<f32>, String> {
        let mut session = self
            .vision
            .lock()
            .map_err(|_| "图片推理状态异常，请重启软件。")?;
        let name = session.inputs()[0].name().to_owned();
        let value = Tensor::from_array(([1_usize, 3, 224, 224], pixels)).map_err(error)?;
        let outputs = session
            .run(ort::inputs![name.as_str() => value])
            .map_err(error)?;
        normalize(
            outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(error)?
                .1
                .to_vec(),
        )
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
