use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use tauri::{AppHandle, ipc::Channel};

use super::{
    Filter, Issue, Item, ItemView, Kind, Page, STALE_SCAN, Scan, current, favorites, parse_date,
    scan, session,
};
#[cfg(test)]
use crate::ai::DIM;
use crate::ai::{self, FEATURE_VERSION, MODEL_GENERATION, error};

static SERIAL: Mutex<()> = Mutex::new(());
pub(crate) fn stop_and_wait() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    cancel_all();
    SERIAL.lock().map_err(error)
}

static ACTIVE: Mutex<Option<(u64, Arc<AtomicBool>)>> = Mutex::new(None);

pub(super) fn cancel_all() {
    if let Ok(active) = ACTIVE.lock()
        && let Some((_, cancel)) = active.as_ref()
    {
        cancel.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
pub(crate) fn ai_cancel_index(scan_id: u64) {
    if let Ok(active) = ACTIVE.lock()
        && let Some((id, cancel)) = active.as_ref()
        && *id == scan_id
    {
        cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IndexStatus {
    scan_id: u64,
    enabled: bool,
    running: bool,
    total: usize,
    indexed: usize,
    failed: usize,
    pending: usize,
    reused: usize,
    writable: bool,
    message: Option<String>,
    model_id: &'static str,
    needs_rebuild: bool,
    rebuild_reason: Option<String>,
}

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

    fn save(
        &mut self,
        entries: &[(Vec<u8>, Vec<u8>, u16, Result<Vec<f32>, String>)],
    ) -> Result<(), String> {
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
    if tempfile::NamedTempFile::new_in(dir).is_err() {
        return false;
    }
    let path = scan.root.path.join(".photosys/ai.sqlite");
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
    status.running = ACTIVE
        .lock()
        .map_err(error)?
        .as_ref()
        .is_some_and(|(id, _)| *id == scan_id);
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
    tauri::async_runtime::spawn_blocking(move || status(scan_id))
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

struct ActiveGuard;
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE.lock() {
            *active = None;
        }
    }
}

fn update(
    scan_id: u64,
    automatic: bool,
    progress: &Channel<IndexStatus>,
    load_model: impl FnOnce() -> Result<Arc<ai::inference::Runtime>, String>,
) -> Result<IndexStatus, String> {
    let _serial = SERIAL.lock().map_err(error)?;
    let scan = scan(scan_id)?;
    let initial = status(scan_id)?;
    if automatic && (!initial.enabled || !initial.writable || initial.needs_rebuild) {
        return Ok(initial);
    }
    if !initial.writable {
        return Err("此相册的 AI 索引不可写，已有索引仍可用于搜索。".into());
    }
    let generation = MODEL_GENERATION.load(Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    *ACTIVE.lock().map_err(error)? = Some((scan_id, cancel.clone()));
    let _active = ActiveGuard;
    let mut state = initial;
    state.running = true;
    let _ = progress.send(state.clone());
    let Some(_analysis) = ai::analysis_slot(&cancel)? else {
        return status(scan_id);
    };
    let runtime = load_model()?;
    if runtime.spec.id != state.model_id {
        return Err("模型已变化，请重试索引。".into());
    }
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
    let mut batch = Vec::new();
    state.indexed = 0;
    state.failed = 0;
    for item in scan.items.iter().filter(|i| eligible(i)) {
        let path = favorites::path_key(&item.file.relative);
        if cancel.load(Ordering::Relaxed)
            || generation != MODEL_GENERATION.load(Ordering::Relaxed)
            || super::scan(scan_id).is_err()
        {
            break;
        }
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
            let result = decode(&scan, item, &cancel)
                .and_then(|image| runtime.encode_image(image, rotation));
            if cancel.load(Ordering::Relaxed)
                || generation != MODEL_GENERATION.load(Ordering::Relaxed)
            {
                break;
            }
            // Never save a vector with an old rotation after an interactive edit.
            if rotations(scan_id)?.get(&item.file.id).copied().unwrap_or(0) != rotation {
                continue;
            }
            if result.is_ok() {
                state.indexed += 1;
            } else {
                state.failed += 1;
            }
            batch.push((path, item.file.stamp.portable_key(), rotation, result));
        }
        // Keep completed vectors available to concurrent searches and resilient to interruption.
        if batch.len() >= 8 {
            cache.save(&batch)?;
            batch.clear();
        }
        state.pending = state.total.saturating_sub(state.indexed + state.failed);
        let _ = progress.send(state.clone());
    }
    cache.save(&batch)?;
    if !cancel.load(Ordering::Relaxed)
        && generation == MODEL_GENERATION.load(Ordering::Relaxed)
        && super::scan(scan_id).is_ok()
    {
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
    drop(_active);
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
        update(scan_id, automatic, &progress, || ai::runtime(&app))
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub(crate) async fn ai_index_issues(scan_id: u64, page: usize) -> Result<Page<Issue>, String> {
    tauri::async_runtime::spawn_blocking(move || {
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

fn rank_for(
    scan: &Scan,
    embedding: &[f32],
    filter: &Filter,
    favorites: &HashSet<usize>,
    rotations: &HashMap<usize, u16>,
    page: usize,
    spec: &'static ai::catalog::ModelSpec,
) -> Result<Page<ItemView>, String> {
    let from = parse_date(filter.from.as_deref())?;
    let to = parse_date(filter.to.as_deref())?;
    if matches!((from, to), (Some(a), Some(b)) if a > b) {
        return Err("开始日期应早于或等于结束日期。".into());
    }
    let cache = Cache::open(scan, false)?;
    let mut ranked = Vec::new();
    if let Some(mut cache) = cache {
        cache.spec = spec;
        let candidates: HashMap<_, _> = scan
            .items
            .iter()
            .filter(|i| eligible(i))
            .filter(|i| !filter.favorites_only || favorites.contains(&i.file.id))
            .filter(|i| {
                if filter.unknown_date {
                    i.captured_on.is_none()
                } else {
                    from.is_none_or(|d| i.captured_on >= Some(d))
                        && to.is_none_or(|d| i.captured_on.is_some_and(|v| v <= d))
                }
            })
            .map(|i| (favorites::path_key(&i.file.relative), i))
            .collect();
        let mut stmt = cache.connection.prepare("SELECT path, stamp, rotation, vector FROM embeddings WHERE version=?1 AND vector IS NOT NULL").map_err(error)?;
        let mut rows = stmt.query([cache.spec.feature_version]).map_err(error)?;
        while let Some(row) = rows.next().map_err(error)? {
            let path: Vec<u8> = row.get(0).map_err(error)?;
            let Some(item) = candidates.get(&path) else {
                continue;
            };
            let stamp: Vec<u8> = row.get(1).map_err(error)?;
            let rotation: u16 = row.get(2).map_err(error)?;
            if stamp != item.file.stamp.portable_key()
                || rotation != rotations.get(&item.file.id).copied().unwrap_or(0)
            {
                continue;
            }
            let stored: Vec<u8> = row.get(3).map_err(error)?;
            let v = vector_dim(&stored, cache.spec.dimension)?;
            if embedding.len() != v.len() {
                return Err("查询与索引模型不兼容。".into());
            }
            let score: f32 = embedding.iter().zip(v).map(|(a, b)| a * b).sum();
            ranked.push((score, item.file.id));
        }
    }
    ranked.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    ranked.truncate(200);
    let total = ranked.len();
    let (page, offset) = Page::<ItemView>::bounds(page, total);
    let items = ranked
        .into_iter()
        .skip(offset)
        .take(super::PAGE_SIZE)
        .map(|(_, id)| {
            scan.items[id].view(
                favorites.contains(&id),
                rotations.get(&id).copied().unwrap_or(0),
            )
        })
        .collect();
    Ok(Page { page, total, items })
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
        let scan = scan(scan_id)?;
        let generation = MODEL_GENERATION.load(Ordering::Relaxed);
        let (favorites, rotations) = {
            let lock = session()?;
            let session = current(&lock, scan_id)?;
            (session.favorites.clone(), session.rotations.clone())
        };
        let embedding = ai::runtime(&app)?.encode_text(query.trim())?;
        let state = status(scan_id)?;
        let Page { page, total, items } =
            rank(&scan, &embedding, &filter, &favorites, &rotations, page)?;
        super::scan(scan_id)?;
        if generation != MODEL_GENERATION.load(Ordering::Relaxed) {
            return Err("模型目录已变化，请重新搜索。".into());
        }
        Ok(SearchPage {
            page,
            total,
            items,
            indexed: state.indexed,
            pending: state.pending,
            failed: state.failed,
        })
    })
    .await
    .map_err(error)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Filter {
        Filter {
            query: String::new(),
            kind: None,
            from: None,
            to: None,
            unknown_date: false,
            favorites_only: false,
        }
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
        let scan = Scan::read(1, root, |_| {});
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
