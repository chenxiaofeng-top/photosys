//! YuNet face detection and SFace or AuraFace recognition.
use super::{
    device::{SessionDevice, SessionStatus},
    error,
    inference::InferenceSession,
};
use image::{
    RgbImage,
    codecs::jpeg::JpegEncoder,
    imageops::{self, FilterType},
};
use ort::{session::SessionOutputs, value::Tensor};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
#[cfg(test)]
pub(crate) const DIM: usize = 128;
pub(crate) const VERSION: &str = "yunet640-sface112-align5-v1";
pub(crate) const GEOMETRY_VERSION: &str = "yunet640-bgr-align5-v1";
const EDGE: usize = 640;
pub(crate) const TEMPLATE: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];
#[derive(Clone)]
pub(crate) struct Face {
    pub bbox: [f32; 4], // normalized x,y,width,height in displayed orientation
    pub confidence: f32,
    pub vector: Option<Vec<f32>>,
    pub thumbnail: Vec<u8>,
}
#[derive(Clone, Debug)]
pub(crate) struct Detection {
    pub bbox: [f32; 4],
    pub points: [[f32; 2]; 5],
    pub score: f32,
}
pub(crate) struct FaceRuntime {
    detector: InferenceSession,
    recognizer: InferenceSession,
    pub(crate) spec: &'static super::catalog::ModelSpec,
}
#[cfg(test)]
pub(crate) fn normalize(v: Vec<f32>) -> Result<Vec<f32>, String> {
    normalize_dim(v, DIM)
}
pub(crate) fn normalize_dim(mut v: Vec<f32>, dimension: usize) -> Result<Vec<f32>, String> {
    if v.len() != dimension || v.iter().any(|x| !x.is_finite()) {
        return Err("人物模型返回无效特征。".into());
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-12 || !norm.is_finite() {
        return Err("人物模型返回零特征。".into());
    }
    for x in &mut v {
        *x /= norm;
    }
    Ok(v)
}
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
pub(crate) fn rotate(image: RgbImage, rotation: u16) -> RgbImage {
    match rotation {
        90 => imageops::rotate90(&image),
        180 => imageops::rotate180(&image),
        270 => imageops::rotate270(&image),
        _ => image,
    }
}
fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let w = ((a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0])).max(0.0);
    let h = ((a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1])).max(0.0);
    w * h / (a[2] * a[3] + b[2] * b[3] - w * h).max(1e-9)
}
// Least-squares, orientation-preserving similarity transform, source -> template.
pub(crate) fn transform(points: &[[f32; 2]; 5]) -> Result<[f64; 4], String> {
    let mut sx = 0.;
    let mut sy = 0.;
    let mut tx = 0.;
    let mut ty = 0.;
    for i in 0..5 {
        sx += points[i][0] as f64 / 5.;
        sy += points[i][1] as f64 / 5.;
        tx += TEMPLATE[i][0] as f64 / 5.;
        ty += TEMPLATE[i][1] as f64 / 5.;
    }
    let (mut d, mut a, mut b) = (0., 0., 0.);
    for i in 0..5 {
        let x = points[i][0] as f64 - sx;
        let y = points[i][1] as f64 - sy;
        let u = TEMPLATE[i][0] as f64 - tx;
        let v = TEMPLATE[i][1] as f64 - ty;
        d += x * x + y * y;
        a += x * u + y * v;
        b += x * v - y * u;
    }
    if d < 1e-8 || !d.is_finite() {
        return Err("人脸关键点无效。".into());
    }
    a /= d;
    b /= d;
    Ok([a, b, tx - a * sx + b * sy, ty - b * sx - a * sy])
}
pub(crate) fn align(image: &RgbImage, points: &[[f32; 2]; 5]) -> Result<RgbImage, String> {
    let [a, b, tx, ty] = transform(points)?;
    let d = a * a + b * b;
    if d < 1e-12 {
        return Err("无法对齐人脸。".into());
    }
    Ok(RgbImage::from_fn(112, 112, |x, y| {
        let u = x as f64 - tx;
        let v = y as f64 - ty;
        let sx = (a * u + b * v) / d;
        let sy = (-b * u + a * v) / d;
        let x0 = sx.floor() as i64;
        let y0 = sy.floor() as i64;
        let dx = sx - x0 as f64;
        let dy = sy - y0 as f64;
        let mut rgb = [0u8; 3];
        for c in 0..3 {
            let mut sum = 0.;
            for (ox, oy, w) in [
                (0, 0, (1. - dx) * (1. - dy)),
                (1, 0, dx * (1. - dy)),
                (0, 1, (1. - dx) * dy),
                (1, 1, dx * dy),
            ] {
                let px = x0 + ox;
                let py = y0 + oy;
                if px >= 0 && py >= 0 && px < image.width() as i64 && py < image.height() as i64 {
                    sum += image.get_pixel(px as u32, py as u32)[c] as f64 * w;
                }
            }
            rgb[c] = sum.round().clamp(0., 255.) as u8;
        }
        image::Rgb(rgb)
    }))
}
impl FaceRuntime {
    #[cfg(test)]
    pub(crate) fn load(directory: &Path) -> Result<Self, String> {
        Self::load_spec(
            directory,
            super::catalog::default_model(super::ModelKind::People),
        )
    }
    pub(crate) fn load_spec(
        directory: &Path,
        spec: &'static super::catalog::ModelSpec,
    ) -> Result<Self, String> {
        Self::load_with_device(directory, spec, &SessionDevice::cpu())
    }
    pub(crate) fn load_with_device(
        directory: &Path,
        spec: &'static super::catalog::ModelSpec,
        device: &SessionDevice,
    ) -> Result<Self, String> {
        let runtime = Self {
            spec,
            detector: InferenceSession::open(
                directory.join("face_detection_yunet_2023mar.onnx"),
                device,
            )?,
            recognizer: InferenceSession::open(
                directory.join(if spec.backend == "auraface" {
                    "glintr100.onnx"
                } else {
                    "face_recognition_sface_2021dec.onnx"
                }),
                device,
            )?,
        };
        runtime.detect(&RgbImage::new(640, 640))?;
        runtime.encode(&RgbImage::from_pixel(112, 112, image::Rgb([127, 127, 127])))?;
        Ok(runtime)
    }
    pub(super) fn inference_status(&self) -> Result<Vec<SessionStatus>, String> {
        Ok(vec![self.detector.status()?, self.recognizer.status()?])
    }
    #[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
    pub(crate) fn end_profiles(&self) -> Vec<String> {
        vec![self.detector.end_profile(), self.recognizer.end_profile()]
    }
    pub(crate) fn detect(&self, image: &RgbImage) -> Result<Vec<Detection>, String> {
        let scale = (EDGE as f32 / image.width() as f32).min(EDGE as f32 / image.height() as f32);
        let w = (image.width() as f32 * scale)
            .round()
            .clamp(1., EDGE as f32) as u32;
        let h = (image.height() as f32 * scale)
            .round()
            .clamp(1., EDGE as f32) as u32;
        let resized = imageops::resize(image, w, h, FilterType::Triangle);
        let mut pixels = vec![0.; 3 * EDGE * EDGE];
        for (x, y, p) in resized.enumerate_pixels() {
            for c in 0..3 {
                pixels[c * EDGE * EDGE + y as usize * EDGE + x as usize] = p[2 - c] as f32;
            }
        }
        let input = Tensor::from_array(([1, 3, EDGE, EDGE], pixels))
            .map_err(error)?
            .into_dyn();
        self.detector
            .run(vec![("input".into(), input)], None, |out| {
                Self::decode_detections(out, w, h, scale)
            })
    }
    fn decode_detections(
        out: SessionOutputs<'_>,
        w: u32,
        h: u32,
        scale: f32,
    ) -> Result<Vec<Detection>, String> {
        let mut detections = Vec::new();
        for stride in [8usize, 16, 32] {
            let cls = out[format!("cls_{stride}")]
                .try_extract_tensor::<f32>()
                .map_err(error)?
                .1;
            let obj = out[format!("obj_{stride}")]
                .try_extract_tensor::<f32>()
                .map_err(error)?
                .1;
            let bbox = out[format!("bbox_{stride}")]
                .try_extract_tensor::<f32>()
                .map_err(error)?
                .1;
            let kps = out[format!("kps_{stride}")]
                .try_extract_tensor::<f32>()
                .map_err(error)?
                .1;
            let cols = EDGE / stride;
            let n = cols * cols;
            if cls.len() != n || obj.len() != n || bbox.len() != n * 4 || kps.len() != n * 10 {
                return Err("人脸检测模型输出形状不兼容。".into());
            }
            for i in 0..n {
                let score = (cls[i].clamp(0., 1.) * obj[i].clamp(0., 1.)).sqrt();
                if !score.is_finite() || score < 0.9 {
                    continue;
                }
                let s = stride as f32;
                let c = (i % cols) as f32;
                let r = (i / cols) as f32;
                let cx = (c + bbox[i * 4]) * s;
                let cy = (r + bbox[i * 4 + 1]) * s;
                let bw = bbox[i * 4 + 2].exp() * s;
                let bh = bbox[i * 4 + 3].exp() * s;
                if ![cx, cy, bw, bh].iter().all(|x| x.is_finite())
                    || cx > w as f32
                    || cy > h as f32
                    || bw < 8.
                    || bh < 8.
                {
                    continue;
                }
                let mut points = [[0.; 2]; 5];
                for j in 0..5 {
                    points[j] = [
                        (kps[i * 10 + j * 2] + c) * s / scale,
                        (kps[i * 10 + j * 2 + 1] + r) * s / scale,
                    ];
                }
                if points.iter().flatten().any(|x| !x.is_finite()) {
                    continue;
                }
                detections.push(Detection {
                    bbox: [
                        (cx - bw / 2.) / scale,
                        (cy - bh / 2.) / scale,
                        bw / scale,
                        bh / scale,
                    ],
                    points,
                    score,
                });
            }
        }
        detections.sort_by(|a, b| b.score.total_cmp(&a.score));
        detections.truncate(5000);
        let mut kept: Vec<Detection> = Vec::new();
        for d in detections {
            if kept.iter().all(|k| iou(&d.bbox, &k.bbox) <= 0.3) {
                kept.push(d);
                if kept.len() == 100 {
                    break;
                }
            }
        }
        Ok(kept)
    }
    pub(crate) fn encode(&self, image: &RgbImage) -> Result<Vec<f32>, String> {
        if image.dimensions() != (112, 112) {
            return Err("人脸输入大小错误。".into());
        }
        let mut pixels = vec![0.; 3 * 112 * 112];
        for (i, p) in image.pixels().enumerate() {
            for c in 0..3 {
                pixels[c * 112 * 112 + i] = if self.spec.backend == "auraface" {
                    (p[c] as f32 - 127.5) / 127.5
                } else {
                    p[c] as f32
                };
            }
        }
        let input = Tensor::from_array(([1usize, 3, 112, 112], pixels))
            .map_err(error)?
            .into_dyn();
        let name = self.recognizer.inputs()[0].clone();
        self.recognizer.run(vec![(name, input)], None, |out| {
            normalize_dim(
                out[0]
                    .try_extract_tensor::<f32>()
                    .map_err(error)?
                    .1
                    .to_vec(),
                self.spec.dimension,
            )
        })
    }
    pub(crate) fn analyze(
        &self,
        image: RgbImage,
        rotation: u16,
        cancel: &AtomicBool,
    ) -> Result<Vec<Face>, String> {
        let image = rotate(image, rotation);
        let detections = self.detect(&image)?;
        let mut faces = Vec::new();
        for d in detections {
            if cancel.load(Ordering::Relaxed) {
                return Err("已停止人物分析。".into());
            }
            let aligned = align(&image, &d.points)?;
            let eyes = ((d.points[1][0] - d.points[0][0]).powi(2)
                + (d.points[1][1] - d.points[0][1]).powi(2))
            .sqrt();
            let vector = if d.bbox[2].min(d.bbox[3]) >= 32. && eyes >= 10. {
                Some(self.encode(&aligned)?)
            } else {
                None
            };
            let mut thumbnail = Vec::new();
            JpegEncoder::new_with_quality(&mut thumbnail, 80)
                .encode_image(&aligned)
                .map_err(error)?;
            let iw = image.width() as f32;
            let ih = image.height() as f32;
            faces.push(Face {
                bbox: [
                    d.bbox[0] / iw,
                    d.bbox[1] / ih,
                    d.bbox[2] / iw,
                    d.bbox[3] / ih,
                ],
                confidence: d.score,
                vector,
                thumbnail,
            });
        }
        Ok(faces)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alignment_and_invalid_landmarks() {
        let t = transform(&TEMPLATE).unwrap();
        assert!((t[0] - 1.).abs() < 1e-6);
        assert!(t[1].abs() < 1e-6);
        assert!(transform(&[[0., 0.]; 5]).is_err());
        let im = RgbImage::from_fn(112, 112, |x, y| image::Rgb([x as u8, y as u8, 30]));
        let out = align(&im, &TEMPLATE).unwrap();
        assert_eq!(im, out);
    }
    #[test]
    fn rejects_invalid_features() {
        assert!(normalize(vec![0.; DIM]).is_err());
        assert!(normalize(vec![f32::NAN; DIM]).is_err());
        assert!(normalize(vec![1.; 512]).is_err());
        assert!(
            (cosine(
                &normalize(vec![1.; DIM]).unwrap(),
                &normalize(vec![1.; DIM]).unwrap()
            ) - 1.)
                .abs()
                < 1e-6
        );
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    use crate::ai::inference;
    #[test]
    #[ignore = "requires PHOTOSYS_PEOPLE_REFERENCE from verify-people-reference.py"]
    fn real_model_face_reference() {
        let path = std::path::PathBuf::from(
            std::env::var_os("PHOTOSYS_PEOPLE_REFERENCE").expect("PHOTOSYS_PEOPLE_REFERENCE"),
        );
        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("reference.json")).unwrap()).unwrap();
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
        let runtime =
            FaceRuntime::load(Path::new(data["model_directory"].as_str().unwrap())).unwrap();
        let mut compared = 0;
        for case in data["cases"].as_array().unwrap() {
            let im = image::open(case["path"].as_str().unwrap())
                .unwrap()
                .to_rgb8();
            let actual = runtime.detect(&im).unwrap();
            let expected = case["faces"].as_array().unwrap();
            assert_eq!(actual.len(), expected.len(), "detection count");
            for e in expected {
                let bbox: Vec<f32> = serde_json::from_value(e["box"].clone()).unwrap();
                let detected = actual
                    .iter()
                    .min_by(|a, b| {
                        let dist = |d: &Detection| {
                            d.bbox
                                .iter()
                                .zip(&bbox)
                                .map(|(x, y)| (x - y).powi(2))
                                .sum::<f32>()
                        };
                        dist(a).total_cmp(&dist(b))
                    })
                    .unwrap();
                assert!(
                    detected
                        .bbox
                        .iter()
                        .zip(&bbox)
                        .all(|(a, b)| (a - b).abs() < 5.),
                    "detector box differs: {:?} vs {:?}",
                    detected.bbox,
                    bbox
                );
                let points: [[f32; 2]; 5] = serde_json::from_value(e["points"].clone()).unwrap();
                let aligned = align(&im, &points).unwrap();
                let expected_image = image::open(path.join(e["aligned"].as_str().unwrap()))
                    .unwrap()
                    .to_rgb8();
                let mae = aligned
                    .as_raw()
                    .iter()
                    .zip(expected_image.as_raw())
                    .map(|(a, b)| (*a as f64 - *b as f64).abs())
                    .sum::<f64>()
                    / (112. * 112. * 3.);
                assert!(mae < 2.0, "alignment MAE {mae}");
                let ours = runtime.encode(&aligned).unwrap();
                let theirs: Vec<f32> = serde_json::from_value(e["vector"].clone()).unwrap();
                let score = cosine(&ours, &theirs);
                println!("alignment MAE={mae:.4}; embedding cosine={score:.8}");
                assert!(score > 0.995);
                let pipeline = runtime
                    .encode(&align(&im, &detected.points).unwrap())
                    .unwrap();
                let pipeline_score = cosine(&pipeline, &theirs);
                println!("full pipeline cosine={pipeline_score:.8}");
                assert!(pipeline_score > 0.98);
                compared += 1;
            }
            let faces = runtime.analyze(im, 0, &AtomicBool::new(false)).unwrap();
            assert_eq!(faces.len(), expected.len());
        }
        assert!(compared > 0);
    }
}
