//! Local-only Chinese photo search. Network access is confined to model installation.
pub(crate) mod catalog;
pub(crate) mod device;
pub(crate) mod download;
pub(crate) mod inference;
mod model;
pub(crate) use catalog::initialize as initialize_choices;
pub(crate) mod faces;
pub(crate) mod index_policy;
mod packages;
pub(crate) mod people_model;

// Indexing jobs share one slot; browsing remains independent.
pub(crate) static ANALYSIS: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Wait without blocking cancellation while another indexer is running.
pub(crate) fn analysis_slot(
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Option<std::sync::MutexGuard<'static, ()>>, String> {
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(None);
        }
        match ANALYSIS.try_lock() {
            Ok(slot) => return Ok(Some(slot)),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(25))
            }
            Err(e) => return Err(error(e)),
        }
    }
}

#[cfg(test)]
mod scheduling_tests {
    #[test]
    fn queued_analysis_can_be_cancelled_without_loading_a_model() {
        let slot = super::ANALYSIS.lock().unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert!(super::analysis_slot(&cancelled).unwrap().is_none());
        drop(slot);
    }
}

#[cfg(test)]
mod model_options_tests;

#[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
pub(crate) mod device_tests;
