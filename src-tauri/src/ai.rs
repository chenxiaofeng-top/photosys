//! Local-only Chinese photo search. Network access is confined to model installation.
pub(crate) mod catalog;
pub(crate) mod device;
pub(crate) mod download;
pub(crate) mod inference;
mod model;
pub(crate) use catalog::initialize as initialize_choices;
pub(crate) mod faces;
mod image_input;
pub(crate) mod index_policy;
pub(crate) mod indexing;
mod packages;
pub(crate) mod people_model;

pub(crate) use model::*;

#[cfg(test)]
pub(crate) const MODEL_ID: &str = "chinese-clip-vit-b16-fp32-v1";
// Increment whenever decoding, rotation, tokenization or preprocessing changes.
pub(crate) const FEATURE_VERSION: &str = "cnclip-b16-fp32-rgb224-bicubic-v1";
#[cfg(test)]
pub(crate) const DIM: usize = 512;

pub(crate) fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod model_options_tests;

#[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
pub(crate) mod device_tests;
