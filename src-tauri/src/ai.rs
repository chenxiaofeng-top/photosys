//! Local-only Chinese photo search. Network access is confined to model installation.
pub(crate) mod download;
pub(crate) mod inference;
mod model;

pub(crate) use model::*;

pub(crate) const MODEL_ID: &str = "chinese-clip-vit-b16-fp32-v1";
// Increment whenever decoding, rotation, tokenization or preprocessing changes.
pub(crate) const FEATURE_VERSION: &str = "cnclip-b16-fp32-rgb224-bicubic-v1";
pub(crate) const DIM: usize = 512;

pub(crate) fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
