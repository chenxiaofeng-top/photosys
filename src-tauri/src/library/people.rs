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
    indexing::{Active, Mode, Run, Stage},
};
use query::{FaceScope, FaceView, Month, PersonView};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use store::{Edit, Store};
use tauri::{AppHandle, ipc::Channel};
static SERIAL: Mutex<()> = Mutex::new(());
static MUTATION: Mutex<()> = Mutex::new(());
pub(crate) fn lock_edits() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    MUTATION.lock().map_err(error)
}
pub(crate) fn stop_and_wait() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    cancel_all();
    SERIAL.lock().map_err(error)
}
static ACTIVE: Active = Active::new();
static EDIT_GENERATION: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IndexStatus {
    pub scan_id: u64,
    pub enabled: bool,
    pub running: bool,
    pub queued: bool,
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
pub(crate) fn cancel_all() {
    ACTIVE.cancel(None);
}
#[tauri::command]
pub(crate) fn people_cancel_index(scan_id: u64) {
    ACTIVE.cancel(Some(scan_id));
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
    (state.running, state.queued) = ACTIVE.status(scan_id)?;
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
    if !policy.accuracy_validated() {
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
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        status(scan_id)
    })
    .await
    .map_err(error)?
}
#[cfg(test)]
fn update(
    scan_id: u64,
    automatic: bool,
    progress: &Channel<IndexStatus>,
    load: impl FnOnce() -> Result<Arc<FaceRuntime>, String>,
) -> Result<IndexStatus, String> {
    update_with_run(scan_id, automatic, progress, &Mode::Pipeline.into(), load)
}

pub(super) fn update_with_run(
    scan_id: u64,
    automatic: bool,
    progress: &Channel<IndexStatus>,
    run: &Run,
    load: impl FnOnce() -> Result<Arc<FaceRuntime>, String>,
) -> Result<IndexStatus, String> {
    let _serial = SERIAL.lock().map_err(error)?;
    let mode = run.mode.for_active_models();
    let scan = scan(scan_id)?;
    let mut state = status(scan_id)?;
    if automatic && (!state.enabled || !state.writable || state.needs_rebuild) {
        return Ok(state);
    }
    if !state.writable {
        return Err("此相册的人物索引不可写，可以浏览已有结果。".into());
    }
    let active = ACTIVE.start(scan_id)?;
    let cancel = &active.job.cancel;
    state.running = true;
    let Some(_slot) = mode.admit(&active.job, || {
        state.queued = true;
        let _ = progress.send(state.clone());
    })?
    else {
        drop(active);
        return status(scan_id);
    };
    let decodes = scan.ai_decodes.join(mode)?;
    state.queued = false;
    let _ = progress.send(state.clone());
    let generation = MODEL_GENERATION.load(Ordering::Relaxed);
    let interrupted = || {
        cancel.load(Ordering::Relaxed)
            || MODEL_GENERATION.load(Ordering::Relaxed) != generation
            || super::scan(scan_id).is_err()
    };
    if interrupted() {
        drop(active);
        return status(scan_id);
    }
    let Some(runtime) = mode.load_model(cancel, || run.measure(Stage::ModelLoad, load))? else {
        drop(active);
        return status(scan_id);
    };
    let runtime = runtime?;
    if interrupted() {
        drop(active);
        return status(scan_id);
    }
    if runtime.spec.id != state.model_id {
        return Err("人物模型已变化，请重试。".into());
    }
    let optimized = mode.optimized(|| runtime.uses_directml_for_index())?;
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
    let (mut groups, mut edits) = {
        let _edit = MUTATION.lock().map_err(error)?;
        (
            grouping::Groups::load(scan_id, policy.clone())?,
            EDIT_GENERATION.load(Ordering::Relaxed),
        )
    };
    let mut jobs = Vec::new();
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
            && r.version == runtime.spec.feature_version
            && (r.error.is_none() || automatic)
        {
            if r.error.is_none() {
                state.indexed += 1;
                state.reused += 1;
            } else {
                state.failed += 1;
            }
        } else {
            jobs.push((item, path, rotation));
        }
    }
    drop(records);
    if !jobs.is_empty() && !interrupted() {
        let mut last = Instant::now();
        mode.with_prepared(
            optimized,
            jobs.into_iter(),
            |(item, path, rotation)| {
                if interrupted() {
                    return None;
                }
                let prepared = super::ai_search::decode_with_slot(
                    &scan, item, &decodes, cancel, run,
                )?
                .map(|image| {
                    let input = optimized.then(|| {
                        run.measure(Stage::Prepare, || {
                            FaceRuntime::prepare_detection(image.image(), rotation, true)
                        })
                    });
                    (image, input)
                });
                Some((item, path, rotation, prepared))
            },
            |prepared| {
                for (item, path, rotation, image) in prepared {
                    if interrupted() {
                        break;
                    }
                    let result = match image {
                        Ok((image, input)) => {
                            let input = input.unwrap_or_else(|| {
                                run.measure(Stage::Prepare, || {
                                    FaceRuntime::prepare_detection(image.image(), rotation, false)
                                })
                            });
                            runtime.analyze_prepared(
                                image.image(),
                                rotation,
                                input,
                                cancel,
                                Some(run.inference(mode, cancel)),
                            )
                        }
                        Err(error) => Err(error),
                    };
                    if result.is_ok() && !super::ai_search::index_file_current(&scan, item.file.id)
                    {
                        continue;
                    }
                    let _edit = lock_edits()?;
                    if interrupted() {
                        break;
                    }
                    let guard = session()?;
                    let Ok(library) = current(&guard, scan_id) else {
                        break;
                    };
                    if library.rotations.get(&item.file.id).copied().unwrap_or(0) != rotation {
                        continue;
                    }
                    let ids = run.measure(Stage::Write, || {
                        store.save_for_model(
                            &path,
                            &item.file.stamp.portable_key(),
                            rotation,
                            &result,
                            runtime.spec.feature_version,
                        )
                    })?;
                    drop(guard);
                    run.measure(Stage::Group, || {
                        assign_with_edits(
                            scan_id,
                            &mut store,
                            &mut groups,
                            &policy,
                            &mut edits,
                            &ids,
                        )
                    })?;
                    drop(_edit);
                    if result.is_ok() {
                        state.indexed += 1;
                    } else {
                        state.failed += 1;
                    }
                    state.pending = state.total.saturating_sub(state.indexed + state.failed);
                    if last.elapsed().as_millis() >= 150 {
                        let _ = progress.send(state.clone());
                        last = Instant::now();
                    }
                }
                Ok(())
            },
        )?;
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
    // Reuse existing features to organize faces detected before automatic grouping was enabled.
    if policy.allowed() && !interrupted() {
        let ids = grouping::Groups::pending_ids(scan_id)?;
        for chunk in ids.chunks(64) {
            if interrupted() {
                break;
            }
            let _edit = MUTATION.lock().map_err(error)?;
            run.measure(Stage::Group, || {
                assign_with_edits(scan_id, &mut store, &mut groups, &policy, &mut edits, chunk)
            })?;
        }
    }
    drop(active);
    let mut final_state = status(scan_id)?;
    final_state.reused = state.reused;
    Ok(final_state)
}
// Caller holds MUTATION so representative reload and assignment see the same manual edits.
fn assign_with_edits(
    scan_id: u64,
    store: &mut Store,
    groups: &mut grouping::Groups,
    policy: &grouping::Policy,
    edits: &mut u64,
    ids: &[i64],
) -> Result<(), String> {
    let current_edits = EDIT_GENERATION.load(Ordering::Relaxed);
    if *edits != current_edits {
        *groups = grouping::Groups::load(scan_id, policy.clone())?;
        *edits = current_edits;
    }
    groups.assign(store, ids)
}
#[tauri::command]
pub(crate) async fn people_update_index(
    app: AppHandle,
    scan_id: u64,
    automatic: bool,
    progress: Channel<IndexStatus>,
) -> Result<IndexStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        update_with_run(
            scan_id,
            automatic,
            &progress,
            &Mode::configured(&app).into(),
            || ai::people_model::runtime(&app),
        )
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
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        query::people(scan_id, &query, page)
    })
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
        let _database = crate::database_access::read()?;
        query::faces(scan_id, scope, person_id, file_id, page)
    })
    .await
    .map_err(error)?
}

fn validate_suggestion_selection(ids: &[i64]) -> Result<(), String> {
    if ids.is_empty()
        || ids.len() > super::PAGE_SIZE
        || ids.iter().copied().collect::<HashSet<_>>().len() != ids.len()
    {
        return Err("请选择本页 1～24 张不同的人脸。".into());
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn people_face_suggestions(
    scan_id: u64,
    face_ids: Vec<i64>,
) -> Result<grouping::FaceSuggestions, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        validate_suggestion_selection(&face_ids)?;
        let _edit = lock_edits()?;
        grouping::suggestions(scan_id, &face_ids)
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
        let _database = crate::database_access::read()?;
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
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        query::months(scan_id, person_id, &filter, sort)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn people_edit(
    scan_id: u64,
    edit: Edit,
    suggestion_version: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        let _edit = lock_edits()?;
        if let Some(version) = suggestion_version {
            apply_suggestion(scan_id, edit, &version)?;
        } else {
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
            Store::open(scan, true)?
                .ok_or("无法打开人物数据库。")?
                .edit(edit)?;
        }
        EDIT_GENERATION.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .await
    .map_err(error)?
}

// The caller holds MUTATION so publishing a new model or assigning a face cannot interleave.
fn apply_suggestion(scan_id: u64, edit: Edit, version: &str) -> Result<(), String> {
    let Edit::Assign {
        ref face_ids,
        person_id: Some(person_id),
        ..
    } = edit
    else {
        return Err(grouping::SUGGESTION_CHANGED.into());
    };
    validate_suggestion_selection(face_ids)?;
    let spec = ai::catalog::active(ai::ModelKind::People);
    if spec.feature_version != version {
        return Err(grouping::SUGGESTION_CHANGED.into());
    }
    let sources = grouping::suggestion_sources(scan_id, face_ids, version)?;
    if sources.len() != face_ids.len()
        || sources
            .iter()
            .map(|source| source.file_id)
            .collect::<HashSet<_>>()
            .len()
            != sources.len()
    {
        return Err(grouping::SUGGESTION_CHANGED.into());
    }
    let lock = session()?;
    let state = current(&lock, scan_id)?;
    let mut store = Store::open(&state.scan, true)?.ok_or("无法打开人物数据库。")?;
    store.edit_checked(edit, |store| {
        let geometry = store.geometry_expression()?;
        let mut pending = store
            .connection
            .prepare(&format!(
            "SELECT f.path,m.stamp,m.rotation,f.vector FROM faces f JOIN media m ON m.path=f.path
             WHERE f.id=?1 AND {} AND ({geometry})=?3",
            grouping::PENDING_FACE,
        ))
            .map_err(error)?;
        for source in sources {
            let item = state.scan.item(source.file_id)?;
            state
                .scan
                .root
                .open_matching(&item.file)
                .map_err(|_| grouping::SUGGESTION_CHANGED.to_string())?;
            let row: Option<(Vec<u8>, Vec<u8>, u16, Vec<u8>)> = pending
                .query_row(
                    params![source.id, version, faces::GEOMETRY_VERSION],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(error)?;
            let Some((path, stamp, rotation, vector)) = row else {
                return Err(grouping::SUGGESTION_CHANGED.into());
            };
            if path != favorites::path_key(&item.file.relative)
                || stamp != item.file.stamp.portable_key()
                || rotation != state.rotations.get(&source.file_id).copied().unwrap_or(0)
                || grouping::vector(&vector).is_none()
                || grouping::target_blocked(&store.connection, person_id, &path, source.id)?
            {
                return Err(grouping::SUGGESTION_CHANGED.into());
            }
        }
        Ok(())
    })
}
#[tauri::command]
pub(crate) async fn people_undo(scan_id: u64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
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
        let _database = crate::database_access::read()?;
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
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        query::photo_people(scan_id, file_id)
    })
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

pub(crate) fn close_reader() -> Result<(), String> {
    query::close()
}
