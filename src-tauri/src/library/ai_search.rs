use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::ValueRef};
use serde::Serialize;
use tauri::{AppHandle, ipc::Channel};

use super::{
    Filter, Issue, Item, ItemView, Kind, Page, STALE_SCAN, Scan, current, favorites, parse_date,
    scan, session,
};
#[cfg(test)]
use crate::ai::DIM;
use crate::ai::indexing::{Active, DecodeAccess, Decoded, Mode, Run, Stage};
use crate::ai::{self, FEATURE_VERSION, MODEL_GENERATION, error};

static SERIAL: Mutex<()> = Mutex::new(());
pub(crate) fn stop_and_wait() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    cancel_all();
    SERIAL.lock().map_err(error)
}

static ACTIVE: Active = Active::new();

pub(crate) fn cancel_all() {
    ACTIVE.cancel(None);
}

#[tauri::command]
pub(crate) fn ai_cancel_index(scan_id: u64) {
    ACTIVE.cancel(Some(scan_id));
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IndexStatus {
    scan_id: u64,
    enabled: bool,
    running: bool,
    queued: bool,
    total: usize,
    pub(super) indexed: usize,
    pub(super) failed: usize,
    pending: usize,
    pub(super) reused: usize,
    writable: bool,
    message: Option<String>,
    model_id: &'static str,
    needs_rebuild: bool,
    rebuild_reason: Option<String>,
}

type IndexEntry = (Vec<u8>, Vec<u8>, u16, Result<Vec<f32>, String>);

struct Cache {
    connection: Connection,
    spec: &'static ai::catalog::ModelSpec,
}

impl Cache {
    fn open(scan: &Scan, writable: bool) -> Result<Option<Self>, String> {
        let directory = favorites::data_directory(&scan.root)?
            .ok_or("找不到媒体库数据目录，请重新选择相册。")?;
        Self::at(&directory.join("ai.sqlite"), writable)
    }

    fn at(path: &Path, writable: bool) -> Result<Option<Self>, String> {
        match fs::symlink_metadata(path) {
            Ok(m) if !m.is_file() || crate::duplicates::is_link(&m) => {
                return Err("AI 索引必须是普通数据库文件。".into());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !writable => return Ok(None),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(error(e)),
            _ => {}
        }
        let flags = if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let connection = Connection::open_with_flags(path, flags).map_err(error)?;
        connection
            .busy_timeout(Duration::from_secs(3))
            .map_err(error)?;
        if writable {
            connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS embeddings (
               path BLOB PRIMARY KEY, stamp BLOB NOT NULL, rotation INTEGER NOT NULL,
               version TEXT NOT NULL, vector BLOB, error TEXT
             ) WITHOUT ROWID;"
        ).map_err(error)?;
        }
        Ok(Some(Self {
            connection,
            spec: ai::catalog::active(ai::ModelKind::Search),
        }))
    }

    fn enabled(&self) -> Result<bool, String> {
        self.connection
            .query_row("SELECT value FROM settings WHERE key='enabled'", [], |r| {
                r.get::<_, bool>(0)
            })
            .optional()
            .map(|v| v.unwrap_or(false))
            .map_err(error)
    }

    fn records(&self) -> Result<HashMap<Vec<u8>, Record>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT path, stamp, rotation, version, vector IS NOT NULL, error FROM embeddings",
            )
            .map_err(error)?;
        statement
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    Record {
                        stamp: r.get(1)?,
                        rotation: r.get(2)?,
                        version: r.get(3)?,
                        indexed: r.get(4)?,
                        error: r.get(5)?,
                    },
                ))
            })
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)
    }

    fn save(&mut self, entries: &[IndexEntry]) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(error)?;
        for (path, stamp, rotation, result) in entries {
            let (vector, issue) = match result {
                Ok(v) => (
                    Some(v.iter().flat_map(|n| n.to_le_bytes()).collect::<Vec<_>>()),
                    None,
                ),
                Err(e) => (None, Some(e.as_str())),
            };
            transaction
                .execute(
                    "INSERT OR REPLACE INTO embeddings VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        path,
                        stamp,
                        rotation,
                        self.spec.feature_version,
                        vector,
                        issue
                    ],
                )
                .map_err(error)?;
        }
        transaction.commit().map_err(error)
    }

    fn prune(&mut self, paths: &HashSet<Vec<u8>>) -> Result<(), String> {
        let keys = self
            .records()?
            .into_keys()
            .filter(|p| !paths.contains(p))
            .collect::<Vec<_>>();
        let tx = self.connection.transaction().map_err(error)?;
        for path in keys {
            tx.execute("DELETE FROM embeddings WHERE path=?1", [path])
                .map_err(error)?;
        }
        tx.commit().map_err(error)
    }
}

struct Record {
    stamp: Vec<u8>,
    rotation: u16,
    version: String,
    indexed: bool,
    error: Option<String>,
}
impl Record {
    fn matches(&self, item: &Item, rotation: u16) -> bool {
        self.stamp == item.file.stamp.portable_key()
            && self.rotation == rotation
            && self.version == ai::catalog::active(ai::ModelKind::Search).feature_version
    }
}

fn eligible(item: &Item) -> bool {
    item.kind == Kind::Photo && item.preview_available
}

fn rotations(scan_id: u64) -> Result<HashMap<usize, u16>, String> {
    Ok(current(&*session()?, scan_id)?.rotations.clone())
}

fn is_writable(scan: &Scan) -> bool {
    let Ok(Some(dir)) = favorites::data_directory(&scan.root) else {
        return false;
    };
    if tempfile::NamedTempFile::new_in(&dir).is_err() {
        return false;
    }
    let path = dir.join("ai.sqlite");
    if path.exists() {
        Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .and_then(|c| c.execute_batch("BEGIN IMMEDIATE; ROLLBACK;"))
        .is_ok()
    } else {
        true
    }
}

fn status(scan_id: u64) -> Result<IndexStatus, String> {
    let scan = scan(scan_id)?;
    let rotations = rotations(scan_id)?;
    let spec = ai::catalog::active(ai::ModelKind::Search);
    let mut status = IndexStatus {
        scan_id,
        model_id: spec.id,
        needs_rebuild: spec.feature_version != FEATURE_VERSION,
        total: scan.items.iter().filter(|i| eligible(i)).count(),
        writable: is_writable(&scan),
        ..Default::default()
    };
    (status.running, status.queued) = ACTIVE.status(scan_id)?;
    match Cache::open(&scan, false) {
        Ok(Some(cache)) => {
            status.enabled = cache.enabled()?;
            status.needs_rebuild =
                ai::index_policy::needs_rebuild(&cache.connection, spec, FEATURE_VERSION)?;
            let records = cache.records()?;
            for item in scan.items.iter().filter(|i| eligible(i)) {
                let rotation = rotations.get(&item.file.id).copied().unwrap_or(0);
                if let Some(record) = records
                    .get(&favorites::path_key(&item.file.relative))
                    .filter(|r| r.matches(item, rotation))
                {
                    if record.indexed {
                        status.indexed += 1;
                    } else {
                        status.failed += 1;
                    }
                }
            }
        }
        Ok(None) => {}
        Err(e) => status.message = Some(e),
    }
    if status.needs_rebuild {
        status.rebuild_reason = Some("当前模型需要重新建立照片内容索引，请手动开始重建。".into());
    }
    status.pending = status.total.saturating_sub(status.indexed + status.failed);
    Ok(status)
}

pub(crate) fn model_impact(scan_id: u64, spec: &ai::catalog::ModelSpec) -> Result<usize, String> {
    let scan = scan(scan_id)?;
    let rotations = rotations(scan_id)?;
    let records = Cache::open(&scan, false)?
        .map(|c| c.records())
        .transpose()?
        .unwrap_or_default();
    Ok(scan
        .items
        .iter()
        .filter(|i| eligible(i))
        .filter(|i| {
            records
                .get(&favorites::path_key(&i.file.relative))
                .is_none_or(|r| {
                    r.version != spec.feature_version
                        || r.stamp != i.file.stamp.portable_key()
                        || r.rotation != rotations.get(&i.file.id).copied().unwrap_or(0)
                        || !r.indexed
                })
        })
        .count())
}

#[tauri::command]
pub(crate) async fn ai_index_status(scan_id: u64) -> Result<IndexStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        status(scan_id)
    })
    .await
    .map_err(error)?
}

pub(super) fn decode(
    scan: &Scan,
    item: &Item,
    cancel: &AtomicBool,
) -> Result<image::RgbImage, String> {
    let mut file = scan
        .root
        .open_matching(&item.file)
        .map_err(|_| STALE_SCAN)?;
    let image = if item.is_live_photo {
        let preview = super::live::cover(&mut file, true)?;
        let (_, encoded) = preview.split_once(',').ok_or("无法解码实况封面。")?;
        ai::inference::decode_rgb(&STANDARD.decode(encoded).map_err(error)?)?
    } else {
        crate::similar::decode_rgb(
            &mut file,
            &item.file.relative,
            item.file.stamp.size,
            u32::MAX,
            Some(cancel),
        )?
        .0
    };
    scan.root
        .verify_read(&file, &item.file)
        .map_err(|_| STALE_SCAN)?;
    Ok(image)
}

pub(super) fn decode_with_slot(
    scan: &Scan,
    item: &Item,
    decodes: &DecodeAccess<'_>,
    cancel: &AtomicBool,
    run: &Run,
) -> Option<Result<Decoded, String>> {
    decodes.decode(
        &item.file,
        cancel,
        run,
        || {
            scan.root
                .open_matching(&item.file)
                .map(drop)
                .map_err(|_| STALE_SCAN.into())
        },
        || decode(scan, item, cancel),
    )
}

fn save_current_batch(
    scan: &Scan,
    generation: u64,
    cache: &mut Cache,
    batch: &mut Vec<(usize, IndexEntry)>,
    state: &mut IndexStatus,
) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    batch.retain(|(file_id, entry)| {
        if entry.3.is_ok() && !index_file_current(scan, *file_id) {
            state.indexed -= 1;
            return false;
        }
        true
    });
    let guard = session()?;
    let Ok(library) = current(&guard, scan.id) else {
        batch.clear();
        return Ok(());
    };
    let model_current = generation == MODEL_GENERATION.load(Ordering::Relaxed);
    let mut valid = Vec::with_capacity(batch.len());
    for (file_id, entry) in batch.drain(..) {
        if model_current && library.rotations.get(&file_id).copied().unwrap_or(0) == entry.2 {
            valid.push(entry);
        } else if entry.3.is_ok() {
            state.indexed -= 1;
        } else {
            state.failed -= 1;
        }
    }
    // Keep the rotation snapshot valid through the bounded SQLite commit.
    let result = cache.save(&valid);
    drop(guard);
    result
}

pub(super) fn index_file_current(scan: &Scan, file_id: usize) -> bool {
    scan.item(file_id)
        .is_ok_and(|item| scan.root.open_matching(&item.file).is_ok())
}

#[cfg(test)]
fn update(
    scan_id: u64,
    automatic: bool,
    progress: &Channel<IndexStatus>,
    load_model: impl FnOnce() -> Result<Arc<ai::inference::Runtime>, String>,
) -> Result<IndexStatus, String> {
    update_with_run(
        scan_id,
        automatic,
        progress,
        &Mode::Pipeline.into(),
        load_model,
    )
}

pub(super) fn update_with_run(
    scan_id: u64,
    automatic: bool,
    progress: &Channel<IndexStatus>,
    run: &Run,
    load_model: impl FnOnce() -> Result<Arc<ai::inference::Runtime>, String>,
) -> Result<IndexStatus, String> {
    let _serial = SERIAL.lock().map_err(error)?;
    let mode = run.mode.for_active_models();
    let scan = scan(scan_id)?;
    let initial = status(scan_id)?;
    if automatic && (!initial.enabled || !initial.writable || initial.needs_rebuild) {
        return Ok(initial);
    }
    if !initial.writable {
        return Err("此相册的 AI 索引不可写，已有索引仍可用于搜索。".into());
    }
    let generation = MODEL_GENERATION.load(Ordering::Relaxed);
    let active = ACTIVE.start(scan_id)?;
    let cancel = &active.job.cancel;
    let mut state = initial;
    state.running = true;
    let Some(_analysis) = mode.admit(&active.job, || {
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
    let interrupted = || {
        cancel.load(Ordering::Relaxed)
            || generation != MODEL_GENERATION.load(Ordering::Relaxed)
            || super::scan(scan_id).is_err()
    };
    if interrupted() {
        drop(active);
        return status(scan_id);
    }
    let Some(runtime) = mode.load_model(cancel, || run.measure(Stage::ModelLoad, load_model))?
    else {
        drop(active);
        return status(scan_id);
    };
    let runtime = runtime?;
    if interrupted() {
        drop(active);
        return status(scan_id);
    }
    if runtime.spec.id != state.model_id {
        return Err("模型已变化，请重试索引。".into());
    }
    let optimized = mode.optimized(|| runtime.uses_directml_for_index())?;
    let mut cache = Cache::open(&scan, true)?.ok_or("无法创建 AI 索引。")?;
    cache
        .connection
        .execute("INSERT OR REPLACE INTO settings VALUES ('enabled',1)", [])
        .map_err(error)?;
    ai::index_policy::approve(&cache.connection, runtime.spec)?;
    state.enabled = true;
    state.needs_rebuild = false;
    state.rebuild_reason = None;
    let records = cache.records()?;
    let saved_rotations = rotations(scan_id)?;
    let mut jobs = Vec::new();
    state.indexed = 0;
    state.failed = 0;
    for item in scan.items.iter().filter(|i| eligible(i)) {
        if interrupted() {
            break;
        }
        let path = favorites::path_key(&item.file.relative);
        let rotation = saved_rotations.get(&item.file.id).copied().unwrap_or(0);
        if let Some(record) = records.get(&path).filter(|r| r.matches(item, rotation))
            && (record.indexed || automatic)
        {
            if record.indexed {
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
        let mut batch = Vec::with_capacity(8);
        mode.with_prepared(
            optimized,
            jobs.into_iter(),
            |(item, path, rotation)| {
                if interrupted() {
                    return None;
                }
                let input = decode_with_slot(&scan, item, &decodes, cancel, run)?.map(|image| {
                    run.measure(Stage::Prepare, || {
                        runtime.prepare_image(image.image(), rotation, optimized)
                    })
                });
                Some((item, path, rotation, input))
            },
            |prepared| {
                for (item, path, rotation, input) in prepared {
                    if interrupted() {
                        break;
                    }
                    let result = match input {
                        Ok(input) => {
                            runtime.encode_indexed_pixels(input, Some(run.inference(mode, cancel)))
                        }
                        Err(error) => Err(error),
                    };
                    if interrupted() {
                        break;
                    }
                    if result.is_ok() {
                        state.indexed += 1;
                    } else {
                        state.failed += 1;
                    }
                    batch.push((
                        item.file.id,
                        (path, item.file.stamp.portable_key(), rotation, result),
                    ));
                    if batch.len() == 8 {
                        run.measure(Stage::Write, || {
                            save_current_batch(
                                &scan, generation, &mut cache, &mut batch, &mut state,
                            )
                        })?;
                    }
                    state.pending = state.total.saturating_sub(state.indexed + state.failed);
                    let _ = progress.send(state.clone());
                }
                run.measure(Stage::Write, || {
                    save_current_batch(&scan, generation, &mut cache, &mut batch, &mut state)
                })
            },
        )?;
    }
    if !interrupted() {
        cache.prune(
            &scan
                .items
                .iter()
                .filter(|i| eligible(i))
                .map(|i| favorites::path_key(&i.file.relative))
                .collect(),
        )?;
    }
    let reused = state.reused;
    drop(active);
    let mut result = status(scan_id)?;
    result.reused = reused;
    Ok(result)
}

#[tauri::command]
pub(crate) async fn ai_update_index(
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
            || ai::runtime(&app),
        )
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub(crate) async fn ai_index_issues(scan_id: u64, page: usize) -> Result<Page<Issue>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        let scan = scan(scan_id)?;
        let rotations = rotations(scan_id)?;
        let records = Cache::open(&scan, false)?
            .map(|c| c.records())
            .transpose()?
            .unwrap_or_default();
        let issues: Vec<_> = scan
            .items
            .iter()
            .filter_map(|item| {
                let record = records.get(&favorites::path_key(&item.file.relative))?;
                (record.matches(item, rotations.get(&item.file.id).copied().unwrap_or(0)))
                    .then_some(record)?
                    .error
                    .as_ref()
                    .map(|e| Issue {
                        path: item.file.path_label(),
                        reason: e.clone(),
                    })
            })
            .collect();
        let total = issues.len();
        let (page, offset) = Page::<Issue>::bounds(page, total);
        Ok(Page {
            page,
            total,
            items: issues
                .into_iter()
                .skip(offset)
                .take(super::PAGE_SIZE)
                .collect(),
        })
    })
    .await
    .map_err(error)?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchPage {
    page: usize,
    total: usize,
    items: Vec<ItemView>,
    indexed: usize,
    pending: usize,
    failed: usize,
}

#[cfg(test)]
fn vector(bytes: &[u8]) -> Result<Vec<f32>, String> {
    vector_dim(bytes, DIM)
}
fn vector_dim(bytes: &[u8], dim: usize) -> Result<Vec<f32>, String> {
    if bytes.len() != dim * 4 {
        return Err("AI 索引向量损坏，请更新索引。".into());
    }
    ai::inference::normalize_dim(
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect(),
        dim,
    )
}

#[cfg(test)]
fn rank(
    scan: &Scan,
    embedding: &[f32],
    filter: &Filter,
    favorites: &HashSet<usize>,
    rotations: &HashMap<usize, u16>,
    page: usize,
) -> Result<Page<ItemView>, String> {
    rank_for(
        scan,
        embedding,
        filter,
        favorites,
        rotations,
        page,
        ai::catalog::active(ai::ModelKind::Search),
    )
}

#[cfg(test)]
fn rank_for(
    scan: &Scan,
    embedding: &[f32],
    filter: &Filter,
    favorites: &HashSet<usize>,
    rotations: &HashMap<usize, u16>,
    page: usize,
    spec: &'static ai::catalog::ModelSpec,
) -> Result<Page<ItemView>, String> {
    rank_for_limit(
        scan,
        embedding,
        filter,
        favorites,
        rotations,
        page,
        spec,
        super::PAGE_SIZE,
    )
}

#[cfg(test)]
fn rank_for_limit(
    scan: &Scan,
    embedding: &[f32],
    filter: &Filter,
    favorites: &HashSet<usize>,
    rotations: &HashMap<usize, u16>,
    page: usize,
    spec: &'static ai::catalog::ModelSpec,
    page_size: usize,
) -> Result<Page<ItemView>, String> {
    let cache = Cache::open(scan, false)?;
    let ranked = rank_index(
        cache.as_ref().map(|cache| &cache.connection),
        scan,
        embedding,
        filter,
        favorites,
        rotations,
        spec,
    )?;
    Ok(ranked.page(scan, favorites, rotations, page, page_size))
}

const RESULT_LIMIT: usize = 200;

struct Scored {
    score: f32,
    id: usize,
}

impl Ord for Scored {
    // The worst retained match is at the top of the heap.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then(self.id.cmp(&other.id))
    }
}
impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Scored {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for Scored {}

#[derive(Default)]
struct Ranked {
    ids: Vec<usize>,
    indexed: usize,
    failed: usize,
    pending: usize,
}

impl Ranked {
    fn page(
        &self,
        scan: &Scan,
        favorites: &HashSet<usize>,
        rotations: &HashMap<usize, u16>,
        page: usize,
        page_size: usize,
    ) -> Page<ItemView> {
        let total = self.ids.len();
        let (page, offset) = Page::<ItemView>::bounds(page, total);
        let items = self
            .ids
            .iter()
            .skip(offset)
            .take(page_size)
            .map(|&id| {
                scan.items[id].view(
                    favorites.contains(&id),
                    rotations.get(&id).copied().unwrap_or(0),
                )
            })
            .collect();
        Page { page, total, items }
    }
}

fn rank_index(
    connection: Option<&Connection>,
    scan: &Scan,
    embedding: &[f32],
    filter: &Filter,
    favorites: &HashSet<usize>,
    rotations: &HashMap<usize, u16>,
    spec: &'static ai::catalog::ModelSpec,
) -> Result<Ranked, String> {
    filter.validate_location()?;
    let from = parse_date(filter.from.as_deref())?;
    let to = parse_date(filter.to.as_deref())?;
    if matches!((from, to), (Some(a), Some(b)) if a > b) {
        return Err("开始日期应早于或等于结束日期。".into());
    }
    let Some(connection) = connection else {
        return Ok(Ranked {
            pending: scan.items.iter().filter(|item| eligible(item)).count(),
            ..Ranked::default()
        });
    };
    let candidates: HashMap<_, _> = scan
        .items
        .iter()
        .filter(|item| eligible(item))
        .map(|item| {
            let matches_date = if filter.unknown_date {
                item.captured_on.is_none()
            } else {
                from.is_none_or(|d| item.captured_on >= Some(d))
                    && to.is_none_or(|d| item.captured_on.is_some_and(|v| v <= d))
            };
            let matches = matches_date
                && (!filter.favorites_only || favorites.contains(&item.file.id))
                && filter.matches_location(item);
            (favorites::path_key(&item.file.relative), (item, matches))
        })
        .collect();

    let mut result = Ranked::default();
    let mut ranked = BinaryHeap::with_capacity(RESULT_LIMIT);
    let mut statement = connection
        .prepare("SELECT path, stamp, rotation, vector FROM embeddings WHERE version=?1")
        .map_err(error)?;
    let mut rows = statement.query([spec.feature_version]).map_err(error)?;
    while let Some(row) = rows.next().map_err(error)? {
        let path = row.get_ref(0).map_err(error)?.as_blob().map_err(error)?;
        let Some(&(item, matches)) = candidates.get(path) else {
            continue;
        };
        let stamp = row.get_ref(1).map_err(error)?.as_blob().map_err(error)?;
        let rotation: u16 = row.get(2).map_err(error)?;
        if stamp != item.file.stamp.portable_key()
            || rotation != rotations.get(&item.file.id).copied().unwrap_or(0)
        {
            continue;
        }
        let stored = row.get_ref(3).map_err(error)?;
        if stored == ValueRef::Null {
            result.failed += 1;
            continue;
        }
        result.indexed += 1;
        if !matches {
            continue;
        }
        let vector = vector_dim(stored.as_blob().map_err(error)?, spec.dimension)?;
        if embedding.len() != vector.len() {
            return Err("查询与索引模型不兼容。".into());
        }
        let score = embedding.iter().zip(vector).map(|(a, b)| a * b).sum();
        let entry = Scored {
            score,
            id: item.file.id,
        };
        if ranked.len() < RESULT_LIMIT {
            ranked.push(entry);
        } else if let Some(mut worst) = ranked.peek_mut()
            && entry < *worst
        {
            *worst = entry;
        }
    }
    result.pending = candidates.len() - result.indexed - result.failed;
    result.ids = ranked
        .into_sorted_vec()
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    Ok(result)
}

#[derive(Default)]
struct SearchReader {
    scan_id: Option<u64>,
    connection: Option<Connection>,
    text: Option<TextEmbedding>,
    ranking: Option<CachedRanking>,
}

struct TextEmbedding {
    query: String,
    generation: u64,
    version: &'static str,
    vector: Vec<f32>,
}

struct CachedRanking {
    filter: Filter,
    metadata_revision: u64,
    database_version: i64,
    result: Ranked,
}

static SEARCH_READER: Mutex<SearchReader> = Mutex::new(SearchReader {
    scan_id: None,
    connection: None,
    text: None,
    ranking: None,
});

pub(crate) fn close_reader() -> Result<(), String> {
    *SEARCH_READER.lock().map_err(error)? = SearchReader::default();
    Ok(())
}

#[cfg(test)]
pub(super) fn benchmark_search(
    runtime: &ai::inference::Runtime,
    scan_id: u64,
    filter: Filter,
) -> impl FnMut(&str) -> Result<(), String> {
    let mut reader = SearchReader::default();
    move |query| {
        reader
            .search(scan_id, query, filter.clone(), 0, 24, |text| {
                runtime.encode_text(text)
            })
            .map(drop)
    }
}

impl SearchReader {
    fn search(
        &mut self,
        scan_id: u64,
        query: &str,
        mut filter: Filter,
        page: usize,
        page_size: usize,
        encode: impl FnOnce(&str) -> Result<Vec<f32>, String>,
    ) -> Result<SearchPage, String> {
        let scan = scan(scan_id)?;
        if self.scan_id != Some(scan_id) {
            *self = Self {
                scan_id: Some(scan_id),
                ..Self::default()
            };
        }
        let generation = MODEL_GENERATION.load(Ordering::Relaxed);
        let spec = ai::catalog::active(ai::ModelKind::Search);
        let query = query.trim();
        if !self.text.as_ref().is_some_and(|cached| {
            cached.query == query
                && cached.generation == generation
                && cached.version == spec.feature_version
        }) {
            self.ranking = None;
            self.text = Some(TextEmbedding {
                query: query.to_owned(),
                generation,
                version: spec.feature_version,
                vector: encode(query)?,
            });
        }
        let embedding = &self.text.as_ref().unwrap().vector;
        // The search text is keyed above. Content search always targets photos.
        filter.query.clear();
        filter.kind = None;
        if self.connection.is_none() {
            self.connection = Cache::open(&scan, false)?.map(|cache| cache.connection);
        }
        let database_version = self
            .connection
            .as_ref()
            .map(|connection| {
                connection.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            })
            .transpose()
            .map_err(error)?;
        loop {
            let lock = session()?;
            let state = current(&lock, scan_id)?;
            if generation != MODEL_GENERATION.load(Ordering::Relaxed)
                || spec.feature_version
                    != ai::catalog::active(ai::ModelKind::Search).feature_version
            {
                return Err("模型已变化，请重新搜索。".into());
            }
            if let Some(cached) = &self.ranking
                && Some(cached.database_version) == database_version
                && cached.metadata_revision == state.search_revision
                && cached.filter == filter
            {
                return Ok(cached.result.search_page(state, page, page_size));
            }
            let metadata_revision = state.search_revision;
            let (favorites, rotations) = (state.favorites.clone(), state.rotations.clone());
            drop(lock);
            let ranked = rank_index(
                self.connection.as_ref(),
                &scan,
                embedding,
                &filter,
                &favorites,
                &rotations,
                spec,
            )?;
            let lock = session()?;
            let state = current(&lock, scan_id)?;
            if state.search_revision != metadata_revision {
                continue;
            }
            if generation != MODEL_GENERATION.load(Ordering::Relaxed)
                || spec.feature_version
                    != ai::catalog::active(ai::ModelKind::Search).feature_version
            {
                return Err("模型已变化，请重新搜索。".into());
            }
            let result = ranked.search_page(state, page, page_size);
            self.ranking = database_version.map(|database_version| CachedRanking {
                filter,
                metadata_revision,
                database_version,
                result: ranked,
            });
            return Ok(result);
        }
    }
}

impl Ranked {
    fn search_page(&self, state: &super::Session, page: usize, page_size: usize) -> SearchPage {
        let Page { page, total, items } = self.page(
            &state.scan,
            &state.favorites,
            &state.rotations,
            page,
            page_size,
        );
        SearchPage {
            page,
            total,
            items,
            indexed: self.indexed,
            pending: self.pending,
            failed: self.failed,
        }
    }
}

#[tauri::command]
pub(crate) async fn ai_search(
    app: AppHandle,
    scan_id: u64,
    query: String,
    filter: Filter,
    page: usize,
) -> Result<SearchPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        SEARCH_READER.lock().map_err(error)?.search(
            scan_id,
            &query,
            filter,
            page,
            super::PAGE_SIZE,
            |query| ai::runtime(&app)?.encode_text(query),
        )
    })
    .await
    .map_err(error)?
}

#[cfg(test)]
mod ranking_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_high_resolution_heic_for_indexing_and_preview() {
        let album = tempfile::tempdir().unwrap();
        fs::write(
            album.path().join("photo.HEIC"),
            include_bytes!("../../tests/fixtures/portrait-48mp-grid.heic"),
        )
        .unwrap();
        let scan = Scan::read(
            1,
            crate::duplicates::Root::new(album.path()).unwrap(),
            |_| {},
        );
        let item = &scan.items[0];
        let cancelled = AtomicBool::new(true);
        assert!(
            decode(&scan, item, &cancelled)
                .unwrap_err()
                .contains("已取消")
        );

        let image = decode(&scan, item, &AtomicBool::new(false)).unwrap();
        assert_eq!(image.dimensions(), (6048, 8064));
        // After clockwise rotation, the original top-left red tile is top-right.
        let [r, _, b] = image.get_pixel(6048 - 512, 512).0;
        assert!(r > b + 50);
        let [r, _, b] = image.get_pixel(6048 - 512, 1536).0;
        assert!(b > r + 50);
        drop(image);

        let preview = scan.preview(item.file.id, true).unwrap();
        let (_, encoded) = preview.split_once(',').unwrap();
        let preview = image::load_from_memory(&STANDARD.decode(encoded).unwrap()).unwrap();
        assert_eq!((preview.width(), preview.height()), (1200, 1600));
    }

    fn all() -> Filter {
        Filter {
            query: String::new(),
            kind: None,
            from: None,
            to: None,
            unknown_date: false,
            favorites_only: false,
            location_mode: Default::default(),
            location_selection: None,
        }
    }

    #[test]
    #[ignore = "requires PHOTOSYS_HEIC_ALBUM with failed HEIC records and PHOTOSYS_AI_MODEL_DIR"]
    fn real_failed_heic_retry_preserves_successful_index() {
        let source = std::path::PathBuf::from(
            std::env::var_os("PHOTOSYS_HEIC_ALBUM").expect("PHOTOSYS_HEIC_ALBUM"),
        );
        // Read the original album only; all indexing writes go to a temporary copy.
        let source_scan = Scan::read(0, crate::duplicates::Root::new(&source).unwrap(), |_| {});
        let records = Cache::at(&source.join(".photosys/ai.sqlite"), false)
            .unwrap()
            .unwrap()
            .records()
            .unwrap();
        let album = tempfile::tempdir().unwrap();
        let mut failures = HashMap::new();
        for item in &source_scan.items {
            if !crate::media_codec::is_heif(&item.file.relative) {
                continue;
            }
            let Some(record) = records
                .get(&favorites::path_key(&item.file.relative))
                .filter(|record| !record.indexed)
            else {
                continue;
            };
            let name = format!("failed-{}.HEIC", failures.len());
            fs::copy(source.join(&item.file.relative), album.path().join(&name)).unwrap();
            failures.insert(name, record.error.clone().unwrap());
        }
        assert!(!failures.is_empty(), "album has no failed HEIC records");

        let runtime_name = if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        ai::inference::initialize_from(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("binaries/onnxruntime")
                .join(std::env::consts::ARCH)
                .join(runtime_name),
        )
        .unwrap();
        let model = std::path::PathBuf::from(
            std::env::var_os("PHOTOSYS_AI_MODEL_DIR").expect("PHOTOSYS_AI_MODEL_DIR"),
        );
        let runtime = Arc::new(ai::inference::Runtime::load(&model).unwrap());
        let successful_image = image::RgbImage::from_pixel(8, 8, image::Rgb([220, 40, 20]));
        successful_image
            .save(album.path().join("indexed.png"))
            .unwrap();
        let successful_vector = runtime.encode_image(successful_image, 0).unwrap();

        let root = crate::duplicates::Root::new(album.path()).unwrap();
        favorites::initialize(&root).unwrap();
        let opened = super::super::open_directory(
            album.path().to_str().unwrap(),
            super::super::OpenMode::Refresh,
            |_| {},
        )
        .unwrap();
        let id = opened.scan_id;
        let scan = super::super::scan(id).unwrap();
        let mut cache = Cache::open(&scan, true).unwrap().unwrap();
        let entries = scan
            .items
            .iter()
            .map(|item| {
                let name = item.file.relative.to_str().unwrap();
                let result = if name == "indexed.png" {
                    Ok(successful_vector.clone())
                } else {
                    Err(failures[name].clone())
                };
                (
                    favorites::path_key(&item.file.relative),
                    item.file.stamp.portable_key(),
                    0,
                    result,
                )
            })
            .collect::<Vec<_>>();
        cache.save(&entries).unwrap();
        cache
            .connection
            .execute("INSERT INTO settings VALUES ('enabled',1)", [])
            .unwrap();
        ai::index_policy::approve(&cache.connection, runtime.spec).unwrap();
        drop(cache);

        let progress = Channel::new(|_| Ok(()));
        let automatic = update(id, true, &progress, || Ok(runtime.clone())).unwrap();
        assert_eq!(
            (automatic.indexed, automatic.reused, automatic.failed),
            (1, 1, failures.len())
        );
        let retried = update(id, false, &progress, || Ok(runtime.clone())).unwrap();
        assert_eq!(
            (
                retried.indexed,
                retried.reused,
                retried.failed,
                retried.pending
            ),
            (failures.len() + 1, 1, 0, 0)
        );
        let cache = Cache::open(&scan, false).unwrap().unwrap();
        assert!(
            cache
                .records()
                .unwrap()
                .values()
                .all(|r| r.indexed && r.error.is_none())
        );
        let saved: Vec<u8> = cache
            .connection
            .query_row(
                "SELECT vector FROM embeddings WHERE path=?1",
                [b"indexed.png".as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            saved,
            successful_vector
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>()
        );
        let repeated = update(id, false, &progress, || Ok(runtime.clone())).unwrap();
        assert_eq!(repeated.reused, failures.len() + 1);
        println!(
            "retried {} failed HEIC photos; existing vector preserved; repeated update reused {}",
            failures.len(),
            repeated.reused
        );
        *session().unwrap() = None;
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    #[test]
    #[ignore = "requires real models, an OpenCV test portrait, and a Windows hardware GPU"]
    fn real_directml_search_reuses_cpu_index() {
        let _session = super::super::TEST_SESSION.lock().unwrap();
        use ai::device_tests::{assert_directml_profiles, benchmark_indexes, directml_test_device};
        use ai::inference::Runtime;
        let models =
            std::path::PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_DIR").unwrap());
        let portrait =
            std::path::PathBuf::from(std::env::var_os("PHOTOSYS_MODEL_OPTIONS_PORTRAIT").unwrap());
        let spec = ai::catalog::default_model(ai::ModelKind::Search);
        let model = models.join(spec.id);
        let profiles = tempfile::tempdir().unwrap();
        let device = directml_test_device(profiles.path());
        let cpu = Arc::new(Runtime::load_spec(&model, spec).unwrap());
        let album = tempfile::tempdir().unwrap();
        let directory = album.path().to_str().unwrap();
        let open = || {
            super::super::open_directory(directory, super::super::OpenMode::Refresh, |_| {})
                .unwrap()
                .scan_id
        };
        fs::copy(&portrait, album.path().join("portrait.jpg")).unwrap();
        favorites::initialize(&crate::duplicates::Root::new(album.path()).unwrap()).unwrap();
        let id = open();
        let progress = Channel::new(|_| Ok(()));
        assert_eq!(
            update(id, false, &progress, || Ok(cpu.clone()))
                .unwrap()
                .indexed,
            1
        );

        let gpu = Arc::new(Runtime::load_with_device(&model, spec, &device).unwrap());
        let query = gpu.encode_text("人像照片").unwrap();
        let scan = super::super::scan(id).unwrap();
        let page = rank(&scan, &query, &all(), &HashSet::new(), &HashMap::new(), 0).unwrap();
        assert_eq!(page.items[0].name, "portrait.jpg");
        image::RgbImage::from_pixel(256, 256, image::Rgb([0, 0, 255]))
            .save(album.path().join("blue.png"))
            .unwrap();
        let id = open();
        let added = update(id, true, &progress, || Ok(gpu.clone())).unwrap();
        assert_eq!((added.indexed, added.reused, added.failed), (2, 1, 0));
        assert!(!added.needs_rebuild);

        let same = update(id, true, &progress, || Ok(cpu.clone())).unwrap();
        assert_eq!(same.reused, 2);
        let query = cpu.encode_text("人像照片").unwrap();
        let scan = super::super::scan(id).unwrap();
        let page = rank(&scan, &query, &all(), &HashSet::new(), &HashMap::new(), 0).unwrap();
        assert_eq!(page.items[0].name, "portrait.jpg");
        benchmark_indexes(&portrait, |path, use_gpu| {
            favorites::initialize(&crate::duplicates::Root::new(path).unwrap()).unwrap();
            let opened = super::super::open_directory(
                path.to_str().unwrap(),
                super::super::OpenMode::Refresh,
                |_| {},
            )
            .unwrap();
            let runtime = if use_gpu { &gpu } else { &cpu };
            let result = update(opened.scan_id, false, &progress, || Ok(runtime.clone())).unwrap();
            (result.indexed, result.failed)
        });
        assert_directml_profiles(gpu.end_profiles());
        *session().unwrap() = None;
    }

    #[test]
    #[ignore = "requires PHOTOSYS_AI_REFERENCE and downloaded real models"]
    fn real_album_incremental_cancel_and_search() {
        let reference_dir = std::path::PathBuf::from(
            std::env::var_os("PHOTOSYS_AI_REFERENCE").expect("PHOTOSYS_AI_REFERENCE"),
        );
        let reference: serde_json::Value =
            serde_json::from_slice(&fs::read(reference_dir.join("reference.json")).unwrap())
                .unwrap();
        let model_directory = Path::new(reference["model_directory"].as_str().unwrap());
        let name = if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        ai::inference::initialize_from(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("binaries/onnxruntime")
                .join(std::env::consts::ARCH)
                .join(name),
        )
        .unwrap();
        let runtime = Arc::new(ai::inference::Runtime::load(model_directory).unwrap());
        let album = tempfile::tempdir().unwrap();
        for (i, entry) in reference["images"].as_array().unwrap().iter().enumerate() {
            fs::copy(
                entry["path"].as_str().unwrap(),
                album.path().join(format!("photo-{i}.png")),
            )
            .unwrap();
        }
        let root = crate::duplicates::Root::new(album.path()).unwrap();
        favorites::initialize(&root).unwrap();
        let directory = album.path().to_str().unwrap();
        let opened =
            super::super::open_directory(directory, super::super::OpenMode::Refresh, |_| {})
                .unwrap();
        let id = opened.scan_id;
        let progress = Channel::new(|_| Ok(()));
        let started = std::time::Instant::now();
        let result = update(id, false, &progress, || Ok(runtime.clone())).unwrap();
        println!(
            "first index: {} photos, {:?}",
            result.indexed,
            started.elapsed()
        );
        assert_eq!(result.indexed, 3);
        assert_eq!(result.pending, 0);
        let started = std::time::Instant::now();
        let reused = update(id, true, &progress, || Ok(runtime.clone())).unwrap();
        println!(
            "unchanged incremental: {} reused, {:?}",
            reused.reused,
            started.elapsed()
        );
        assert_eq!(reused.reused, 3);
        let scan = super::super::scan(id).unwrap();
        for (i, entry) in reference["images"].as_array().unwrap().iter().enumerate() {
            let query = runtime
                .encode_text(entry["query"].as_str().unwrap())
                .unwrap();
            let page = rank(&scan, &query, &all(), &HashSet::new(), &HashMap::new(), 0).unwrap();
            assert_eq!(page.items[0].name, format!("photo-{i}.png"));
        }
        session()
            .unwrap()
            .as_mut()
            .unwrap()
            .set_rotation(id, 0, 90)
            .unwrap();
        assert_eq!(status(id).unwrap().pending, 1);
        assert_eq!(
            update(id, true, &progress, || Ok(runtime.clone()))
                .unwrap()
                .reused,
            2
        );
        fs::copy(
            reference["images"][0]["path"].as_str().unwrap(),
            album.path().join("new.png"),
        )
        .unwrap();
        let refreshed =
            super::super::open_directory(directory, super::super::OpenMode::Refresh, |_| {})
                .unwrap();
        let result = update(refreshed.scan_id, true, &progress, || Ok(runtime.clone())).unwrap();
        assert_eq!(result.indexed, 4);
        assert_eq!(result.reused, 3);
        fs::remove_file(album.path().join("new.png")).unwrap();
        let refreshed =
            super::super::open_directory(directory, super::super::OpenMode::Refresh, |_| {})
                .unwrap();
        assert!(status(id).is_err()); // stale library sessions cannot be used
        update(refreshed.scan_id, true, &progress, || Ok(runtime.clone())).unwrap();
        let scan = super::super::scan(refreshed.scan_id).unwrap();
        assert_eq!(
            Cache::open(&scan, false)
                .unwrap()
                .unwrap()
                .records()
                .unwrap()
                .len(),
            3
        );
        fs::remove_file(album.path().join(".photosys/ai.sqlite")).unwrap();
        let id = refreshed.scan_id;
        let cancel_progress = Channel::new(move |message| {
            if let tauri::ipc::InvokeResponseBody::Json(json) = message {
                let value: serde_json::Value = serde_json::from_str(&json).unwrap();
                if value["indexed"].as_u64().unwrap_or(0) >= 1 {
                    ai_cancel_index(id);
                }
            }
            Ok(())
        });
        let partial = update(id, false, &cancel_progress, || Ok(runtime.clone())).unwrap();
        assert_eq!(partial.indexed, 1);
        assert_eq!(partial.pending, 2);
        let recovered = update(id, true, &progress, || Ok(runtime.clone())).unwrap();
        assert_eq!(recovered.indexed, 3);
        assert_eq!(recovered.reused, 1);
        *session().unwrap() = None;
    }

    #[test]
    fn ranks_before_pagination_and_combines_dates_favorites_and_validity() {
        let temp = tempfile::tempdir().unwrap();
        for id in 0..210 {
            image::RgbImage::new(2, 2)
                .save(temp.path().join(format!("20240101_000000-{id:03}.png")))
                .unwrap();
        }
        let root = crate::duplicates::Root::new(temp.path()).unwrap();
        favorites::initialize(&root).unwrap();
        let mut scan = Scan::read(1, root, |_| {});
        scan.items[209].location = Some(super::super::locations::Location {
            coordinates: crate::media_date::Coordinates::new(20., 100.).unwrap(),
            address: None,
        });
        assert_eq!(scan.items.len(), 210);
        assert!(scan.items.iter().all(|i| i.captured_on.is_some()));
        let mut cache = Cache::open(&scan, true).unwrap().unwrap();
        let entries: Vec<_> = scan
            .items
            .iter()
            .map(|item| {
                let mut embedding = vec![0.0; DIM];
                embedding[0] = 1.0;
                embedding[1] = item.file.id as f32 / 100.0;
                (
                    favorites::path_key(&item.file.relative),
                    item.file.stamp.portable_key(),
                    0,
                    Ok(embedding),
                )
            })
            .collect();
        cache.save(&entries).unwrap();
        let mut query = vec![0.0; DIM];
        query[0] = 1.0;
        let page = rank(&scan, &query, &all(), &HashSet::new(), &HashMap::new(), 1).unwrap();
        assert_eq!(page.total, 200);
        assert_eq!(page.items.len(), 24);
        assert_eq!(page.items[0].id, 24);
        let mut geographic = all();
        geographic.location_mode = super::super::locations::LocationMode::Known;
        let page = rank(
            &scan,
            &query,
            &geographic,
            &HashSet::new(),
            &HashMap::new(),
            0,
        )
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].id, 209); // Must filter before truncating the top 200.
        let mapped = rank_for_limit(
            &scan,
            &query,
            &all(),
            &HashSet::new(),
            &HashMap::new(),
            0,
            ai::catalog::active(ai::ModelKind::Search),
            200,
        )
        .unwrap();
        assert_eq!(mapped.items.len(), 200); // Map includes every page, not only the first 24.
        let mut filter = all();
        filter.favorites_only = true;
        let favorite = HashSet::from([209]);
        let page = rank(&scan, &query, &filter, &favorite, &HashMap::new(), 100).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.page, 0);
        assert_eq!(page.items[0].id, 209);
        filter.from = Some("2025-01-01".into());
        assert_eq!(
            rank(&scan, &query, &filter, &favorite, &HashMap::new(), 0)
                .unwrap()
                .total,
            0
        );
        filter.from = None;
        filter.unknown_date = true;
        assert_eq!(
            rank(&scan, &query, &filter, &favorite, &HashMap::new(), 0)
                .unwrap()
                .total,
            0
        );
        filter.unknown_date = false;
        assert_eq!(
            rank(
                &scan,
                &query,
                &filter,
                &favorite,
                &HashMap::from([(209, 90)]),
                0
            )
            .unwrap()
            .total,
            0
        );
        cache
            .connection
            .execute(
                "UPDATE embeddings SET version='old' WHERE path=?1",
                [favorites::path_key(&scan.items[209].file.relative)],
            )
            .unwrap();
        assert_eq!(
            rank(&scan, &query, &filter, &favorite, &HashMap::new(), 0)
                .unwrap()
                .total,
            0
        );
    }

    #[test]
    fn portable_cache_reuses_moves_and_rejects_changed_photos_and_rotations() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("album");
        fs::create_dir(&original).unwrap();
        image::RgbImage::new(2, 2)
            .save(original.join("photo.png"))
            .unwrap();
        let root = crate::duplicates::Root::new(&original).unwrap();
        favorites::initialize(&root).unwrap();
        let scan = Scan::read(1, root, |_| {});
        let mut cache = Cache::open(&scan, true).unwrap().unwrap();
        cache
            .save(&[(
                b"photo.png".to_vec(),
                scan.items[0].file.stamp.portable_key(),
                90,
                Ok(vec![1.0; DIM]),
            )])
            .unwrap();
        drop(cache);
        let moved = temp.path().join("moved");
        fs::rename(&original, &moved).unwrap();
        let scan = Scan::read(2, crate::duplicates::Root::new(&moved).unwrap(), |_| {});
        let cache = Cache::open(&scan, false).unwrap().unwrap();
        let records = cache.records().unwrap();
        let record = &records[b"photo.png".as_slice()];
        assert!(record.matches(&scan.items[0], 90));
        assert!(!record.matches(&scan.items[0], 0));
        let copied = temp.path().join("copied");
        fs::create_dir(&copied).unwrap();
        fs::copy(moved.join("photo.png"), copied.join("photo.png")).unwrap();
        fs::File::options()
            .write(true)
            .open(copied.join("photo.png"))
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(
                    fs::metadata(moved.join("photo.png"))
                        .unwrap()
                        .modified()
                        .unwrap(),
                ),
            )
            .unwrap();
        let copied_scan = Scan::read(4, crate::duplicates::Root::new(&copied).unwrap(), |_| {});
        assert!(record.matches(&copied_scan.items[0], 90));
        image::RgbImage::new(40, 80)
            .save(moved.join("photo.png"))
            .unwrap();
        let refreshed = Scan::read(3, crate::duplicates::Root::new(&moved).unwrap(), |_| {});
        assert!(!record.matches(&refreshed.items[0], 90));
    }

    #[cfg(unix)]
    #[test]
    fn read_only_index_still_supports_search() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        image::RgbImage::new(2, 2)
            .save(temp.path().join("photo.png"))
            .unwrap();
        let root = crate::duplicates::Root::new(temp.path()).unwrap();
        favorites::initialize(&root).unwrap();
        let scan = Scan::read(1, root, |_| {});
        let mut cache = Cache::open(&scan, true).unwrap().unwrap();
        cache
            .save(&[(
                b"photo.png".to_vec(),
                scan.items[0].file.stamp.portable_key(),
                0,
                Ok(vec![1.0; DIM]),
            )])
            .unwrap();
        drop(cache);
        let directory = temp.path().join(".photosys");
        fs::set_permissions(
            directory.join("ai.sqlite"),
            fs::Permissions::from_mode(0o444),
        )
        .unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
        let writable = is_writable(&scan);
        let result = rank(
            &scan,
            &vec![1.0; DIM],
            &all(),
            &HashSet::new(),
            &HashMap::new(),
            0,
        );
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(
            directory.join("ai.sqlite"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(!writable);
        assert_eq!(result.unwrap().total, 1);
    }
    #[test]
    fn cache_persists_progress_prunes_and_versions_entries() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("ai.sqlite");
        let mut cache = Cache::at(&db, true).unwrap().unwrap();
        cache
            .save(&[
                (b"one.jpg".to_vec(), vec![1], 90, Ok(vec![1.0; DIM])),
                (b"bad.jpg".to_vec(), vec![2], 0, Err("decode failed".into())),
            ])
            .unwrap();
        drop(cache);
        let mut cache = Cache::at(&db, true).unwrap().unwrap();
        let records = cache.records().unwrap();
        assert!(records[b"one.jpg".as_slice()].indexed);
        assert_eq!(
            records[b"bad.jpg".as_slice()].error.as_deref(),
            Some("decode failed")
        );
        cache.prune(&HashSet::from([b"one.jpg".to_vec()])).unwrap();
        assert_eq!(cache.records().unwrap().len(), 1);
        let read = Cache::at(&db, false).unwrap().unwrap();
        assert_eq!(read.records().unwrap()[b"one.jpg".as_slice()].rotation, 90);
        assert!(vector(&[0; 16]).is_err());
    }
}

#[cfg(test)]
mod model_selection_tests {
    use super::*;
    #[test]
    fn search_never_mixes_same_dimension_models_or_partially_rebuilt_vectors() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["one.png", "two.png"] {
            image::RgbImage::new(2, 2)
                .save(temp.path().join(name))
                .unwrap();
        }
        let root = crate::duplicates::Root::new(temp.path()).unwrap();
        favorites::initialize(&root).unwrap();
        let scan = Scan::read(1, root, |_| {});
        let low = ai::catalog::find(ai::ModelKind::Search, "chinese-clip-vit-l14-fp32-v1").unwrap();
        let high =
            ai::catalog::find(ai::ModelKind::Search, "chinese-clip-vit-l14-336px-fp32-v1").unwrap();
        let mut cache = Cache::open(&scan, true).unwrap().unwrap();
        for (i, spec) in [low, high].iter().enumerate() {
            cache.spec = spec;
            let item = &scan.items[i];
            cache
                .save(&[(
                    favorites::path_key(&item.file.relative),
                    item.file.stamp.portable_key(),
                    0,
                    Ok(vec![1.; 768]),
                )])
                .unwrap();
        }
        let filter = Filter {
            query: String::new(),
            kind: None,
            from: None,
            to: None,
            unknown_date: false,
            favorites_only: false,
            location_mode: Default::default(),
            location_selection: None,
        };
        for spec in [low, high] {
            let page = rank_for(
                &scan,
                &vec![1.; 768],
                &filter,
                &HashSet::new(),
                &HashMap::new(),
                0,
                spec,
            )
            .unwrap();
            assert_eq!(page.total, 1);
        }
        assert_eq!(
            rank_for(
                &scan,
                &vec![1.; 512],
                &filter,
                &HashSet::new(),
                &HashMap::new(),
                0,
                ai::catalog::all().first().unwrap()
            )
            .unwrap()
            .total,
            0
        );
        assert!(
            rank_for(
                &scan,
                &vec![1.; 512],
                &filter,
                &HashSet::new(),
                &HashMap::new(),
                0,
                low
            )
            .is_err()
        );
        cache.spec = high;
        let item = &scan.items[0];
        cache
            .save(&[(
                favorites::path_key(&item.file.relative),
                item.file.stamp.portable_key(),
                0,
                Ok(vec![1.; 768]),
            )])
            .unwrap();
        assert_eq!(
            rank_for(
                &scan,
                &vec![1.; 768],
                &filter,
                &HashSet::new(),
                &HashMap::new(),
                0,
                high
            )
            .unwrap()
            .total,
            2
        );
        assert_eq!(
            rank_for(
                &scan,
                &vec![1.; 768],
                &filter,
                &HashSet::new(),
                &HashMap::new(),
                0,
                low
            )
            .unwrap()
            .total,
            0
        );
    }
}

#[cfg(test)]
mod real_selection_tests {
    use super::*;
    struct Choices;
    impl Drop for Choices {
        fn drop(&mut self) {
            ai::catalog::select(ai::catalog::default_model(ai::ModelKind::Search));
        }
    }
    #[test]
    #[ignore = "requires PHOTOSYS_MODEL_OPTIONS_DIR; mutates process model choice, run this test alone"]
    fn real_search_model_selection_manual_gate_cancel_resume_and_switch_back() {
        let _session = super::super::TEST_SESSION.lock().unwrap();
        let _restore = Choices;
        let models = std::path::PathBuf::from(
            std::env::var_os("PHOTOSYS_MODEL_OPTIONS_DIR").expect("models"),
        );
        let name = if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        ai::inference::initialize_from(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("binaries/onnxruntime")
                .join(std::env::consts::ARCH)
                .join(name),
        )
        .unwrap();
        let album = tempfile::tempdir().unwrap();
        for (i, c) in [[255, 40, 40], [40, 255, 40], [40, 40, 255]]
            .iter()
            .enumerate()
        {
            image::RgbImage::from_pixel(64, 48, image::Rgb(*c))
                .save(album.path().join(format!("{i}.png")))
                .unwrap();
        }
        let opened = super::super::open_directory(
            album.path().to_str().unwrap(),
            super::super::OpenMode::Select,
            |_| {},
        )
        .unwrap();
        let id = opened.scan_id;
        let people_before = ai::catalog::active(ai::ModelKind::People).id;
        for (n, spec) in ai::catalog::all()
            .iter()
            .filter(|m| m.kind == ai::ModelKind::Search)
            .enumerate()
        {
            ai::catalog::select(spec);
            let state = status(id).unwrap();
            assert_eq!(state.needs_rebuild, n > 0);
            assert_eq!(state.indexed, 0);
            let blocked = update(id, true, &Channel::new(|_| Ok(())), || {
                panic!("automatic model rebuild must not load inference")
            })
            .unwrap();
            assert_eq!(blocked.indexed, 0);
            let runtime =
                Arc::new(ai::inference::Runtime::load_spec(&models.join(spec.id), spec).unwrap());
            let progress = Channel::new(move |message| {
                if let tauri::ipc::InvokeResponseBody::Json(body) = message {
                    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
                    if data["indexed"].as_u64().unwrap_or(0) >= 1 {
                        ai_cancel_index(id);
                    }
                }
                Ok(())
            });
            let partial = update(id, false, &progress, || Ok(runtime.clone())).unwrap();
            assert_eq!(partial.indexed, 1);
            assert_eq!(partial.pending, 2);
            let result =
                update(id, true, &Channel::new(|_| Ok(())), || Ok(runtime.clone())).unwrap();
            assert_eq!(result.indexed, 3);
            assert_eq!(result.reused, 1);
            assert!(!result.needs_rebuild);
            let scan = scan(id).unwrap();
            let query = runtime.encode_text("照片").unwrap();
            let filter = Filter {
                query: String::new(),
                kind: None,
                from: None,
                to: None,
                unknown_date: false,
                favorites_only: false,
                location_mode: Default::default(),
                location_selection: None,
            };
            assert_eq!(
                rank(&scan, &query, &filter, &HashSet::new(), &HashMap::new(), 0)
                    .unwrap()
                    .total,
                3
            );
            assert_eq!(ai::catalog::active(ai::ModelKind::People).id, people_before);
            // Same contract at another directory reuses all saved embeddings.
            let result =
                update(id, true, &Channel::new(|_| Ok(())), || Ok(runtime.clone())).unwrap();
            assert_eq!(result.reused, 3);
        }
        ai::catalog::select(ai::catalog::default_model(ai::ModelKind::Search));
        let state = status(id).unwrap();
        assert!(state.needs_rebuild);
        assert_eq!(state.indexed, 0);
        assert_eq!(
            model_impact(id, ai::catalog::default_model(ai::ModelKind::Search)).unwrap(),
            3
        );
        *session().unwrap() = None;
    }
}

pub(super) fn map_items(
    app: &AppHandle,
    scan_id: u64,
    query: &str,
    filter: &Filter,
) -> Result<Vec<ItemView>, String> {
    Ok(SEARCH_READER
        .lock()
        .map_err(error)?
        .search(scan_id, query, filter.clone(), 0, RESULT_LIMIT, |query| {
            ai::runtime(app)?.encode_text(query)
        })?
        .items)
}
