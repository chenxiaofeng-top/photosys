//! Per-library consent to build a particular feature space. Read-only checks never migrate.
use super::{catalog::ModelSpec, error};
use rusqlite::{Connection, OptionalExtension, params};
pub(crate) fn needs_rebuild(
    c: &Connection,
    spec: &ModelSpec,
    legacy: &str,
) -> Result<bool, String> {
    let approved: Option<String> = c
        .query_row(
            "SELECT CAST(value AS TEXT) FROM settings WHERE key='approved_feature_version'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(approved.as_deref().unwrap_or(legacy) != spec.feature_version)
}
pub(crate) fn approve(c: &Connection, spec: &ModelSpec) -> Result<(), String> {
    c.execute(
        "INSERT OR REPLACE INTO settings(key,value) VALUES('approved_feature_version',?1)",
        params![spec.feature_version],
    )
    .map_err(error)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{ModelKind, catalog};
    #[test]
    fn approval_survives_restart_and_switch_back_requires_manual_start() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("index.sqlite");
        let c = Connection::open(&p).unwrap();
        c.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value INTEGER NOT NULL)")
            .unwrap();
        let old = catalog::default_model(ModelKind::Search);
        let new = catalog::find(ModelKind::Search, "chinese-clip-vit-l14-fp32-v1").unwrap();
        assert!(!needs_rebuild(&c, old, old.feature_version).unwrap());
        assert!(needs_rebuild(&c, new, old.feature_version).unwrap());
        approve(&c, new).unwrap();
        drop(c);
        let c = Connection::open(&p).unwrap();
        assert!(!needs_rebuild(&c, new, old.feature_version).unwrap());
        assert!(needs_rebuild(&c, old, old.feature_version).unwrap());
    }
}
