//! Local person metadata and resumable face analysis. Original media is never changed.
mod grouping;
mod query;
mod store;
#[cfg(test)]
mod tests;

use super::{Filter, Issue, ItemView, Page, Sort, current, favorites, scan, session};
use crate::ai::{
    self, PEOPLE_GENERATION as MODEL_GENERATION, error,
    faces::{self, FaceRuntime},
};
use query::{FaceScope, FaceView, Month, PersonView};
use serde::Serialize;
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
use store::{Edit, Store};
use tauri::{AppHandle, ipc::Channel};
static SERIAL: Mutex<()> = Mutex::new(());
static MUTATION: Mutex<()> = Mutex::new(());
pub(crate) fn stop_and_wait() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    cancel_all();
    SERIAL.lock().map_err(error)
}
static ACTIVE: Mutex<Option<(u64, Arc<AtomicBool>)>> = Mutex::new(None);
static EDIT_GENERATION: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IndexStatus {
    pub scan_id: u64,
    pub enabled: bool,
    pub running: bool,
    pub total: usize,
    pub indexed: usize,
    pub pending: usize,
    pub failed: usize,
    pub reused: usize,
    pub writable: bool,
    pub message: Option<String>,
    pub pending_faces: usize,
    pub ignored_faces: usize,
    pub undo_description: Option<String>,
    pub auto_grouping: bool,
    pub model_id: &'static str,
    pub needs_rebuild: bool,
    pub rebuild_reason: Option<String>,
}
pub(super) fn cancel_all() {
    if let Ok(a) = ACTIVE.lock()
        && let Some((_, c)) = a.as_ref()
    {
        c.store(true, Ordering::Relaxed);
    }
}
#[tauri::command]
pub(crate) fn people_cancel_index(scan_id: u64) {
    if let Ok(a) = ACTIVE.lock()
        && let Some((id, c)) = a.as_ref()
        && *id == scan_id
    {
        c.store(true, Ordering::Relaxed);
    }
}
struct ActiveGuard;
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if let Ok(mut a) = ACTIVE.lock() {
            *a = None;
        }
    }
}
fn status(scan_id: u64) -> Result<IndexStatus, String> {
    let scan = scan(scan_id)?;
    let rotations = { current(&*session()?, scan_id)?.rotations.clone() };
    let spec = ai::catalog::active(ai::ModelKind::People);
    let mut state = IndexStatus {
        scan_id,
        model_id: spec.id,
        needs_rebuild: spec.feature_version != faces::VERSION,
        total: scan
            .items
            .iter()
            .filter(|i| i.kind == super::Kind::Photo && i.preview_available)
            .count(),
        writable: Store::writable(&scan),
        ..Default::default()
    };
    state.running = ACTIVE
        .lock()
        .map_err(error)?
        .as_ref()
        .is_some_and(|(id, _)| *id == scan_id);
    if let Some(s) = Store::open(&scan, false)? {
        state.enabled = s.enabled()?;
        state.needs_rebuild = ai::index_policy::needs_rebuild(&s.connection, spec, faces::VERSION)?;
        let records = s.records()?;
        for item in scan
            .items
            .iter()
            .filter(|i| i.kind == super::Kind::Photo && i.preview_available)
        {
            if let Some(r) = records.get(&favorites::path_key(&item.file.relative))
                && r.stamp == item.file.stamp.portable_key()
                && r.rotation == rotations.get(&item.file.id).copied().unwrap_or(0)
                && r.geometry_version == faces::GEOMETRY_VERSION
                && r.version == ai::catalog::active(ai::ModelKind::People).feature_version
            {
                if r.error.is_some() {
                    state.failed += 1;
                } else {
                    state.indexed += 1;
                }
            }
        }
    }
    if state.needs_rebuild {
        state.rebuild_reason = Some("当前模型需要重新计算人脸特征，请手动重建人物索引。".into());
    }
    state.pending = state.total.saturating_sub(state.indexed + state.failed);
    let (pending, ignored, undo) = query::counts(scan_id)?;
    state.pending_faces = pending;
    state.ignored_faces = ignored;
    state.undo_description = undo;
    let policy = grouping::Policy::shipped();
    state.auto_grouping = policy.allowed();
    if !state.auto_grouping {
        state.message = Some(policy.reason);
    }
    Ok(state)
}
pub(crate) fn model_impact(scan_id: u64, spec: &ai::catalog::ModelSpec) -> Result<usize, String> {
    let scan = scan(scan_id)?;
    let rotations = current(&*session()?, scan_id)?.rotations.clone();
    let records = Store::open(&scan, false)?
        .map(|s| s.records())
        .transpose()?
        .unwrap_or_default();
    Ok(scan
        .items
        .iter()
        .filter(|i| i.kind == super::Kind::Photo && i.preview_available)
        .filter(|i| {
            records
                .get(&favorites::path_key(&i.file.relative))
                .is_none_or(|r| {
                    r.version != spec.feature_version
                        || r.stamp != i.file.stamp.portable_key()
                        || r.rotation != rotations.get(&i.file.id).copied().unwrap_or(0)
                        || r.error.is_some()
                })
        })
        .count())
}

#[tauri::command]
pub(crate) async fn people_index_status(scan_id: u64) -> Result<IndexStatus, String> {
    tauri::async_runtime::spawn_blocking(move || status(scan_id))
        .await
        .map_err(error)?
}
fn update(
    scan_id: u64,
    automatic: bool,
    progress: &Channel<IndexStatus>,
    load: impl FnOnce() -> Result<Arc<FaceRuntime>, String>,
) -> Result<IndexStatus, String> {
    let _serial = SERIAL.lock().map_err(error)?;
    let scan = scan(scan_id)?;
    let mut state = status(scan_id)?;
    if automatic && (!state.enabled || !state.writable || state.needs_rebuild) {
        return Ok(state);
    }
    if !state.writable {
        return Err("此相册的人物索引不可写，可以浏览已有结果。".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *ACTIVE.lock().map_err(error)? = Some((scan_id, cancel.clone()));
    let _active = ActiveGuard;
    state.running = true;
    let _ = progress.send(state.clone());
    let Some(_slot) = ai::analysis_slot(&cancel)? else {
        drop(_active);
        return status(scan_id);
    };
    if cancel.load(Ordering::Relaxed) {
        drop(_active);
        return status(scan_id);
    }
    super::scan(scan_id)?;
    let generation = MODEL_GENERATION.load(Ordering::Relaxed);
    let runtime = load()?;
    if runtime.spec.id != state.model_id {
        return Err("人物模型已变化，请重试。".into());
    }
    let mut store = Store::open(&scan, true)?.ok_or("无法创建人物索引。")?;
    store
        .connection
        .execute("INSERT OR REPLACE INTO settings VALUES('enabled','1')", [])
        .map_err(error)?;
    ai::index_policy::approve(&store.connection, runtime.spec)?;
    state.enabled = true;
    state.needs_rebuild = false;
    state.rebuild_reason = None;
    state.indexed = 0;
    state.failed = 0;
    let records = store.records()?;
    let rotations = { current(&*session()?, scan_id)?.rotations.clone() };
    let policy = grouping::Policy::shipped();
    let mut groups = grouping::Groups::load(scan_id, policy.clone())?;
    let mut edits = EDIT_GENERATION.load(Ordering::Relaxed);
    let interrupted = || {
        cancel.load(Ordering::Relaxed)
            || MODEL_GENERATION.load(Ordering::Relaxed) != generation
            || super::scan(scan_id).is_err()
    };
    let mut last = Instant::now();
    for item in scan
        .items
        .iter()
        .filter(|i| i.kind == super::Kind::Photo && i.preview_available)
    {
        if interrupted() {
            break;
        }
        let path = favorites::path_key(&item.file.relative);
        let rotation = rotations.get(&item.file.id).copied().unwrap_or(0);
        if let Some(r) = records.get(&path)
            && r.stamp == item.file.stamp.portable_key()
            && r.rotation == rotation
            && r.geometry_version == faces::GEOMETRY_VERSION
            && r.version == ai::catalog::active(ai::ModelKind::People).feature_version
            && (r.error.is_none() || automatic)
        {
            if r.error.is_none() {
                state.indexed += 1;
                state.reused += 1;
            } else {
                state.failed += 1;
            }
        } else {
            let result = super::ai_search::decode(&scan, item, &cancel)
                .and_then(|im| runtime.analyze(im, rotation, &cancel));
            if interrupted() {
                break;
            }
            let _edit = MUTATION.lock().map_err(error)?;
            let latest = current(&*session()?, scan_id)?
                .rotations
                .get(&item.file.id)
                .copied()
                .unwrap_or(0);
            if latest != rotation {
                continue;
            }
            let ids = store.save_for_model(
                &path,
                &item.file.stamp.portable_key(),
                rotation,
                &result,
                runtime.spec.feature_version,
            )?;
            let current_edits = EDIT_GENERATION.load(Ordering::Relaxed);
            if current_edits != edits {
                groups = grouping::Groups::load(scan_id, policy.clone())?;
                edits = current_edits;
            }
            groups.assign(&mut store, &ids)?;
            if result.is_ok() {
                state.indexed += 1;
            } else {
                state.failed += 1;
            }
        }
        state.pending = state.total.saturating_sub(state.indexed + state.failed);
        if last.elapsed().as_millis() >= 150 {
            let _ = progress.send(state.clone());
            last = Instant::now();
        }
    }
    if !interrupted() {
        let _edit = MUTATION.lock().map_err(error)?;
        store.prune(
            &scan
                .items
                .iter()
                .filter(|i| i.kind == super::Kind::Photo && i.preview_available)
                .map(|i| favorites::path_key(&i.file.relative))
                .collect::<HashSet<_>>(),
        )?;
    }
    // Also organize previously detected faces after a policy becomes validated.
    if policy.allowed() && !interrupted() {
        let ids = grouping::Groups::pending_ids(scan_id)?;
        for chunk in ids.chunks(64) {
            if interrupted() {
                break;
            }
            let _edit = MUTATION.lock().map_err(error)?;
            let current_edits = EDIT_GENERATION.load(Ordering::Relaxed);
            if edits != current_edits {
                groups = grouping::Groups::load(scan_id, policy.clone())?;
                edits = current_edits;
            }
            groups.assign(&mut store, chunk)?;
        }
    }
    drop(_active);
    let mut final_state = status(scan_id)?;
    final_state.reused = state.reused;
    Ok(final_state)
}
#[tauri::command]
pub(crate) async fn people_update_index(
    app: AppHandle,
    scan_id: u64,
    automatic: bool,
    progress: Channel<IndexStatus>,
) -> Result<IndexStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        update(scan_id, automatic, &progress, || {
            ai::people_model::runtime(&app)
        })
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn people_list(
    scan_id: u64,
    query: String,
    page: usize,
) -> Result<Page<PersonView>, String> {
    tauri::async_runtime::spawn_blocking(move || query::people(scan_id, &query, page))
        .await
        .map_err(error)?
}
#[tauri::command]
pub(crate) async fn people_faces(
    scan_id: u64,
    scope: FaceScope,
    person_id: Option<i64>,
    file_id: Option<usize>,
    page: usize,
) -> Result<Page<FaceView>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        query::faces(scan_id, scope, person_id, file_id, page)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn people_photos(
    scan_id: u64,
    person_id: i64,
    filter: Filter,
    sort: Sort,
    page: usize,
) -> Result<Page<ItemView>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        query::photos(scan_id, person_id, &filter, sort, page)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn people_months(
    scan_id: u64,
    person_id: i64,
    filter: Filter,
    sort: Sort,
) -> Result<Vec<Month>, String> {
    tauri::async_runtime::spawn_blocking(move || query::months(scan_id, person_id, &filter, sort))
        .await
        .map_err(error)?
}
#[tauri::command]
pub(crate) async fn people_edit(scan_id: u64, edit: Edit) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _edit = MUTATION.lock().map_err(error)?;
        match &edit {
            Edit::Assign { face_ids, .. }
            | Edit::Ignore { face_ids }
            | Edit::Restore { face_ids } => {
                if face_ids.is_empty() || face_ids.len() > 1000 {
                    return Err("请选择 1～1000 张人脸。".into());
                }
                query::validate_faces(scan_id, face_ids, matches!(&edit, Edit::Assign { .. }))?;
            }
            _ => {}
        }
        let s = session()?;
        let scan = &current(&s, scan_id)?.scan;
        let mut store = Store::open(scan, true)?.ok_or("无法打开人物数据库。")?;
        store.edit(edit)?;
        EDIT_GENERATION.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn people_undo(scan_id: u64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _edit = MUTATION.lock().map_err(error)?;
        let s = session()?;
        let scan = &current(&s, scan_id)?.scan;
        Store::open(scan, true)?.ok_or("没有人物索引。")?.undo()?;
        EDIT_GENERATION.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn people_index_issues(scan_id: u64, page: usize) -> Result<Page<Issue>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let scan = scan(scan_id)?;
        let rotations = current(&*session()?, scan_id)?.rotations.clone();
        let records = Store::open(&scan, false)?
            .map(|s| s.records())
            .transpose()?
            .unwrap_or_default();
        let mut issues = Vec::new();
        for i in &scan.items {
            if let Some(r) = records.get(&favorites::path_key(&i.file.relative))
                && r.stamp == i.file.stamp.portable_key()
                && r.rotation == rotations.get(&i.file.id).copied().unwrap_or(0)
                && r.geometry_version == faces::GEOMETRY_VERSION
                && r.version == ai::catalog::active(ai::ModelKind::People).feature_version
                && let Some(reason) = &r.error
            {
                issues.push(Issue {
                    path: i.file.path_label(),
                    reason: reason.clone(),
                });
            }
        }
        let total = issues.len();
        let (page, offset) = Page::<Issue>::bounds(page, total);
        Ok(Page {
            page,
            total,
            items: issues.into_iter().skip(offset).take(24).collect(),
        })
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub(crate) async fn people_photo_people(
    scan_id: u64,
    file_id: usize,
) -> Result<Vec<query::Identity>, String> {
    tauri::async_runtime::spawn_blocking(move || query::photo_people(scan_id, file_id))
        .await
        .map_err(error)?
}

pub(super) fn map_photo_ids(
    scan_id: u64,
    person_id: i64,
    filter: &super::Filter,
) -> Result<Vec<usize>, String> {
    query::photo_ids(scan_id, person_id, filter)
}
