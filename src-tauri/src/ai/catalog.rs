//! Pinned, offline model contracts. The JSON is shared with developer packaging tools.
use super::{
    error,
    model::{Asset, ModelKind},
};
use serde::{Deserialize, Serialize};
use std::sync::{
    LazyLock,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelSpec {
    pub id: &'static str,
    pub kind: ModelKind,
    pub name: &'static str,
    pub description: &'static str,
    pub measurement: &'static str,
    pub cpu: &'static str,
    pub license: &'static str,
    pub source: &'static str,
    pub feature_version: &'static str,
    pub dimension: usize,
    pub image_size: usize,
    pub backend: &'static str,
    pub assets: Vec<Asset>,
}
static MODELS: LazyLock<Vec<ModelSpec>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../scripts/ai-model-catalog.json"))
        .expect("bundled model catalog")
});
static SEARCH: AtomicUsize = AtomicUsize::new(0);
static PEOPLE: AtomicUsize = AtomicUsize::new(3);
fn choice(kind: ModelKind) -> &'static AtomicUsize {
    match kind {
        ModelKind::Search => &SEARCH,
        ModelKind::People => &PEOPLE,
    }
}
pub(crate) fn all() -> &'static [ModelSpec] {
    &MODELS
}
pub(crate) fn active(kind: ModelKind) -> &'static ModelSpec {
    &MODELS[choice(kind).load(Ordering::Acquire)]
}
pub(crate) fn find(kind: ModelKind, id: &str) -> Result<&'static ModelSpec, String> {
    MODELS
        .iter()
        .find(|m| m.kind == kind && m.id == id)
        .ok_or_else(|| "不支持的模型或模型用途不匹配。".into())
}
pub(crate) fn select(spec: &ModelSpec) {
    choice(spec.kind).store(
        MODELS
            .iter()
            .position(|m| m.id == spec.id)
            .expect("catalog model"),
        Ordering::Release,
    );
}
#[cfg(test)]
pub(crate) fn default_model(kind: ModelKind) -> &'static ModelSpec {
    &MODELS[if kind == ModelKind::Search { 0 } else { 3 }]
}
pub(crate) fn selection_path(
    app: &tauri::AppHandle,
    kind: ModelKind,
) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    Ok(app
        .path()
        .app_config_dir()
        .map_err(error)?
        .join(if kind == ModelKind::Search {
            "ai-search-model-id"
        } else {
            "ai-people-model-id"
        }))
}
pub(crate) fn initialize(app: &tauri::AppHandle) -> Result<(), String> {
    for kind in [ModelKind::Search, ModelKind::People] {
        match std::fs::read_to_string(selection_path(app, kind)?) {
            Ok(id) => select(find(kind, id.trim())?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(e)),
        }
    }
    Ok(())
}
#[tauri::command]
pub(crate) fn ai_model_catalog(kind: Option<ModelKind>) -> Vec<ModelSpec> {
    all()
        .iter()
        .filter(|m| m.kind == kind.unwrap_or_default())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contracts_are_pinned_and_unique() {
        assert_eq!(all().len(), 5);
        let mut ids = std::collections::HashSet::new();
        let mut versions = std::collections::HashSet::new();
        for m in all() {
            assert!(ids.insert(m.id));
            assert!(versions.insert(m.feature_version));
            assert!(!m.assets.is_empty());
            for a in &m.assets {
                assert_eq!(a.sha.len(), 64);
                assert_eq!(a.revision.len(), 40);
                assert!(a.size > 0);
                assert!(!a.name.contains('/'));
            }
        }
        assert!(find(ModelKind::People, default_model(ModelKind::Search).id).is_err());
        assert_eq!(
            default_model(ModelKind::Search)
                .assets
                .iter()
                .map(|a| a.size)
                .sum::<u64>(),
            755533779
        );
    }
}
