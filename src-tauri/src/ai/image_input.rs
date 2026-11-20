use image::RgbImage;
use image::imageops;

/// View the same pixels as a right-angle rotation without copying the full photo.
#[derive(Clone, Copy)]
pub(super) struct RgbView<'a> {
    image: &'a RgbImage,
    rotation: u16,
}

impl<'a> RgbView<'a> {
    pub(super) fn new(image: &'a RgbImage, rotation: u16) -> Self {
        Self { image, rotation }
    }

    pub(super) fn dimensions(self) -> (u32, u32) {
        let (width, height) = self.image.dimensions();
        match self.rotation {
            90 | 270 => (height, width),
            _ => (width, height),
        }
    }

    pub(super) fn pixel(self, x: u32, y: u32) -> [u8; 3] {
        let (width, height) = self.image.dimensions();
        let (source_x, source_y) = match self.rotation {
            90 => (y, height - 1 - x),
            180 => (width - 1 - x, height - 1 - y),
            270 => (width - 1 - y, x),
            _ => (x, y),
        };
        self.image.get_pixel(source_x, source_y).0
    }

    fn row<'row>(self, y: u32, scratch: &'row mut Vec<u8>) -> &'row [u8]
    where
        'a: 'row,
    {
        let width = self.dimensions().0 as usize;
        if !matches!(self.rotation, 90 | 180 | 270) {
            let start = y as usize * width * 3;
            return &self.image.as_raw()[start..start + width * 3];
        }
        scratch.clear();
        for x in 0..width as u32 {
            scratch.extend_from_slice(&self.pixel(x, y));
        }
        scratch
    }
}

#[derive(Clone, Copy)]
pub(super) enum Filter {
    CatmullRom,
    Triangle,
}

pub(super) fn resize_for_model(
    image: &RgbImage,
    rotation: u16,
    width: u32,
    height: u32,
    filter: Filter,
    optimized: bool,
) -> RgbImage {
    if optimized {
        return resize(RgbView::new(image, rotation), width, height, filter);
    }
    let rotated = match rotation {
        90 => Some(imageops::rotate90(image)),
        180 => Some(imageops::rotate180(image)),
        270 => Some(imageops::rotate270(image)),
        _ => None,
    };
    let filter = match filter {
        Filter::CatmullRom => imageops::FilterType::CatmullRom,
        Filter::Triangle => imageops::FilterType::Triangle,
    };
    imageops::resize(rotated.as_ref().unwrap_or(image), width, height, filter)
}

impl Filter {
    fn weight(self, distance: f32) -> f32 {
        let distance = distance.abs();
        match self {
            Self::Triangle => (1.0 - distance).max(0.0),
            Self::CatmullRom => {
                // Same cubic spline coefficients and evaluation order as image::imageops.
                let squared = distance * distance;
                let cubed = squared * distance;
                let value = if distance < 1.0 {
                    9.0 * cubed - 15.0 * squared + 6.0
                } else if distance < 2.0 {
                    -3.0 * cubed + 15.0 * squared - 24.0 * distance + 12.0
                } else {
                    0.0
                };
                value / 6.0
            }
        }
    }
}

struct Samples {
    first: usize,
    weights: Vec<f32>,
}

fn samples(source: u32, target: u32, filter: Filter) -> impl Iterator<Item = Samples> {
    let ratio = source as f32 / target as f32;
    let scale = ratio.max(1.0);
    let support = scale
        * match filter {
            Filter::CatmullRom => 2.0,
            Filter::Triangle => 1.0,
        };
    (0..target).map(move |pixel| {
        let center = (pixel as f32 + 0.5) * ratio;
        let first = ((center - support).floor() as i64).clamp(0, i64::from(source) - 1);
        let end = ((center + support).ceil() as i64).clamp(first + 1, i64::from(source));
        let center = center - 0.5;
        let mut weights = Vec::with_capacity((end - first) as usize);
        let mut sum = 0.0;
        for index in first..end {
            let weight = filter.weight((index as f32 - center) / scale);
            weights.push(weight);
            sum += weight;
        }
        for weight in &mut weights {
            *weight /= sum;
        }
        Samples {
            first: first as usize,
            weights,
        }
    })
}

/// Match image::imageops::resize's vertical-then-horizontal f32 filtering. Work
/// on one packed RGB row at a time instead of allocating a floating-point image.
pub(super) fn resize(view: RgbView<'_>, width: u32, height: u32, filter: Filter) -> RgbImage {
    let (source_width, source_height) = view.dimensions();
    if source_width == 0 || source_height == 0 || width == 0 || height == 0 {
        return RgbImage::new(width, height);
    }
    if (source_width, source_height) == (width, height) {
        return RgbImage::from_fn(width, height, |x, y| image::Rgb(view.pixel(x, y)));
    }

    let horizontal: Vec<_> = samples(source_width, width, filter).collect();
    let mut row = vec![0.0_f32; source_width as usize * 3];
    let mut scratch = Vec::new();
    let mut output = RgbImage::new(width, height);
    for (target, vertical) in output
        .rows_mut()
        .zip(samples(source_height, height, filter))
    {
        row.fill(0.0);
        for (offset, weight) in vertical.weights.into_iter().enumerate() {
            let source = view.row((vertical.first + offset) as u32, &mut scratch);
            for (sum, pixel) in row.iter_mut().zip(source) {
                *sum += f32::from(*pixel) * weight;
            }
        }
        let (pixels, _) = row.as_chunks::<3>();
        for (pixel, taps) in target.zip(&horizontal) {
            let mut rgb = [0.0; 3];
            for (source, weight) in pixels[taps.first..].iter().zip(&taps.weights) {
                rgb[0] += source[0] * weight;
                rgb[1] += source[1] * weight;
                rgb[2] += source[2] * weight;
            }
            pixel.0 = rgb.map(|value| value.clamp(0.0, 255.0).round() as u8);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::imageops::{self, FilterType};

    #[test]
    fn rgb_resize_preserves_model_pixels_and_rotation() {
        let large = if cfg!(debug_assertions) {
            (1296, 972)
        } else {
            (8064, 6048)
        };
        for (w, h) in [(1, 19), (17, 1), (13, 9), (224, 224), (601, 457), large] {
            let input = RgbImage::from_fn(w, h, |x, y| {
                image::Rgb([
                    (x ^ (y * 13)) as u8,
                    (y ^ (x * 47)) as u8,
                    (x * 3 + y * 11) as u8,
                ])
            });
            for rotation in [0, 90, 180, 270] {
                // Large unrotated photos exercise the long accumulation at the decode limit.
                if w > 1000 && rotation != 0 {
                    continue;
                }
                let oriented = match rotation {
                    90 => imageops::rotate90(&input),
                    180 => imageops::rotate180(&input),
                    270 => imageops::rotate270(&input),
                    _ => input.clone(),
                };
                let view = RgbView::new(&input, rotation);
                for (filter, reference, width, height) in [
                    (Filter::CatmullRom, FilterType::CatmullRom, 224, 224),
                    (Filter::CatmullRom, FilterType::CatmullRom, 336, 336),
                    (Filter::Triangle, FilterType::Triangle, 640, 480),
                    (Filter::CatmullRom, FilterType::CatmullRom, 5, 11),
                ] {
                    let actual = resize(view, width, height, filter);
                    let expected = imageops::resize(&oriented, width, height, reference);
                    if let Some(((x, y, actual), expected)) = actual
                        .enumerate_pixels()
                        .zip(expected.pixels())
                        .find(|((_, _, actual), expected)| actual != expected)
                    {
                        panic!(
                            "{w}x{h} rotated {rotation} to {width}x{height} at {x},{y}: {actual:?} != {expected:?}"
                        );
                    }
                }
            }
        }
    }
}
