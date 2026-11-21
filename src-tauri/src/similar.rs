mod cache;
mod imaging;
mod matching;

pub(crate) use imaging::scaled as decode_rgb;
pub(crate) use imaging::{preview as encode_preview, supported as supports_preview};

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use nom_exif::MediaParser;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager};
use walkdir::WalkDir;

use crate::duplicates::{Candidate, FileStamp, Root, display_path, is_link};
use crate::library::details::{DETAILS_SLOTS, MediaDetails, read_media_details};
use crate::media_date::Media;

use self::cache::Cache;
use self::imaging::PreviewSize;
use self::matching::{Features, Group, MatchKind};

const PAGE_SIZE: usize = 25;
const CACHE_BATCH: usize = 256;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
static RESULT: Mutex<Option<Arc<RwLock<Scan>>>> = Mutex::new(None);
static PREVIEW: Mutex<()> = Mutex::new(());
const STATE_ERROR: &str = "无法读取相似照片任务状态，请重启应用后重试。";

struct Active {
    id: u64,
    cancelled: Arc<AtomicBool>,
}

#[derive(Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub(crate) enum Progress {
    Scanning { found: usize },
    Extracting { processed: usize, total: usize },
    Matching { processed: usize, total: usize },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Summary {
    scan_id: u64,
    found: usize,
    checked: usize,
    reused: usize,
    unsupported: usize,
    skipped: usize,
    groups: usize,
    cache_warning: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub(crate) enum Outcome {
    Finished { summary: Summary },
    Cancelled,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page<T> {
    total: usize,
    items: Vec<T>,
}

#[derive(Clone, Serialize)]
pub(crate) struct Issue {
    path: String,
    reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PhotoView {
    id: usize,
    name: String,
    relative_path: String,
    size: u64,
    width: u32,
    height: u32,
    captured_at: Option<String>,
    hard_link: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupView {
    id: usize,
    representative: PhotoView,
    count: usize,
    has_variant: bool,
    has_burst: bool,
}

#[derive(Serialize)]
pub(crate) struct MemberView {
    photo: PhotoView,
    kind: Option<MatchKind>,
}

struct Photo {
    file: Candidate,
    features: Features,
    moved: bool,
}

impl Photo {
    fn view(&self) -> PhotoView {
        PhotoView {
            id: self.file.id,
            name: self
                .file
                .relative
                .file_name()
                .map_or_else(String::new, |name| display_path(Path::new(name))),
            relative_path: self.file.path_label(),
            size: self.file.stamp.size,
            width: self.features.width,
            height: self.features.height,
            captured_at: self
                .features
                .captured
                .as_ref()
                .map(|time| time.label.clone()),
            hard_link: self.file.stamp.hard_link(),
        }
    }
}

struct Scan {
    id: u64,
    root: Root,
    photos: Vec<Photo>,
    // Empty slots preserve unrelated group IDs after deletion or regrouping.
    groups: Vec<Option<Group>>,
    issues: Vec<Issue>,
}

impl Scan {
    fn active_photo(&self, file_id: usize) -> Result<&Photo, String> {
        self.photos
            .get(file_id)
            .filter(|photo| !photo.moved)
            .ok_or_else(|| "照片记录已失效，请重新扫描。".into())
    }

    fn media_details(&self, file_id: usize) -> Result<MediaDetails, String> {
        read_media_details(&self.root, &self.active_photo(file_id)?.file)
    }

    fn groups(&self, kind: Option<MatchKind>, page: usize) -> Page<GroupView> {
        let groups = self
            .groups
            .iter()
            .enumerate()
            .filter_map(|(id, group)| group.as_ref().map(|group| (id, group)))
            .filter(|(_, group)| group.matches(kind));
        Page {
            total: groups.clone().count(),
            items: groups
                .skip(page_offset(page))
                .take(PAGE_SIZE)
                .map(|(id, group)| self.group_view(id, group))
                .collect(),
        }
    }

    fn members(&self, id: usize, page: usize) -> Result<Page<MemberView>, String> {
        let group = self
            .groups
            .get(id)
            .and_then(Option::as_ref)
            .ok_or("相似组已失效，请重新扫描。")?;
        Ok(Page {
            total: group.members.len() + 1,
            items: std::iter::once((group.anchor, None))
                .chain(
                    group
                        .members
                        .iter()
                        .map(|(photo, kind)| (*photo, Some(*kind))),
                )
                .skip(page_offset(page))
                .take(PAGE_SIZE)
                .map(|(photo, kind)| MemberView {
                    photo: self.photos[photo].view(),
                    kind,
                })
                .collect(),
        })
    }

    fn group_view(&self, id: usize, group: &Group) -> GroupView {
        GroupView {
            id,
            representative: self.photos[group.anchor].view(),
            count: group.members.len() + 1,
            has_variant: group.has_variant,
            has_burst: group.has_burst,
        }
    }

    fn recycle(
        &mut self,
        scan_id: u64,
        group_id: usize,
        file_ids: Vec<usize>,
        mut trash: impl FnMut(&Path) -> Result<(), String>,
    ) -> Result<TrashResult, String> {
        if scan_id != self.id {
            return Err("扫描结果已失效，请重新扫描。".into());
        }
        let group = self
            .groups
            .get(group_id)
            .and_then(Option::as_ref)
            .ok_or("相似组已失效，请重新扫描。")?;
        let ids: Vec<_> = std::iter::once(group.anchor)
            .chain(group.members.iter().map(|(id, _)| *id))
            .collect();
        let selected: HashSet<_> = file_ids.into_iter().collect();
        if selected.is_empty() {
            return Err("请先勾选要移到回收站的照片。".into());
        }
        let group_ids: HashSet<_> = ids.iter().copied().collect();
        if !selected.is_subset(&group_ids) {
            return Err("部分照片不在当前相似组中，请重新选择。".into());
        }
        if selected.len() == ids.len() {
            return Err("每组相似照片至少需要保留一张。".into());
        }
        if selected
            .iter()
            .any(|id| self.photos[*id].file.stamp.hard_link())
        {
            return Err("硬链接照片需要保留，请重新选择。".into());
        }

        let mut outcomes = Vec::new();
        let mut moved = HashSet::new();
        let keepers: Vec<_> = ids
            .iter()
            .filter(|id| !selected.contains(id))
            .map(|&id| &self.photos[id].file)
            .collect();
        for &id in ids.iter().filter(|id| selected.contains(id)) {
            let target = &self.photos[id].file;
            // Recheck a surviving, explicitly unselected photo before every move.
            let keeping = keepers
                .iter()
                .any(|file| self.root.open_matching(file).is_ok());
            let (status, reason) = if !keeping {
                (
                    TrashStatus::Skipped,
                    Some("无法确认保留照片，请重新扫描。".into()),
                )
            } else {
                match self
                    .root
                    .open_matching(target)
                    .and_then(|file| self.root.verify_read(&file, target))
                {
                    Err(error) => (
                        TrashStatus::Skipped,
                        Some(format!("照片已变化或已移除，请重新扫描。{error}")),
                    ),
                    Ok(()) => match trash(&self.root.path.join(&target.relative)) {
                        Ok(()) => {
                            moved.insert(id);
                            (TrashStatus::Moved, None)
                        }
                        Err(error) => (TrashStatus::Failed, Some(error)),
                    },
                }
            };
            outcomes.push(TrashOutcome {
                id,
                relative_path: target.path_label(),
                status,
                reason,
            });
        }

        let mut affected_ids = vec![group_id];
        if !moved.is_empty() {
            for &id in &moved {
                self.photos[id].moved = true;
            }
            let mut group = self.groups[group_id].take().expect("validated group");
            if moved.contains(&group.anchor) {
                // Restore original path order for deterministic tie breaking.
                let mut remaining: Vec<_> =
                    ids.into_iter().filter(|id| !moved.contains(id)).collect();
                remaining.sort_unstable();
                let features: Vec<_> = remaining
                    .iter()
                    .map(|&id| &self.photos[id].features)
                    .collect();
                let regrouped = matching::group(&features, |_| ControlFlow::Continue(()))
                    .continue_value()
                    .expect("regrouping cannot be cancelled");
                for (index, mut group) in regrouped.into_iter().enumerate() {
                    group.anchor = remaining[group.anchor];
                    for (id, _) in &mut group.members {
                        *id = remaining[*id];
                    }
                    if index == 0 {
                        self.groups[group_id] = Some(group);
                    } else {
                        affected_ids.push(self.groups.len());
                        self.groups.push(Some(group));
                    }
                }
            } else {
                group.members.retain(|(id, _)| !moved.contains(id));
                group.has_variant = group
                    .members
                    .iter()
                    .any(|(_, kind)| *kind == MatchKind::Variant);
                group.has_burst = group
                    .members
                    .iter()
                    .any(|(_, kind)| *kind == MatchKind::Burst);
                if !group.members.is_empty() {
                    self.groups[group_id] = Some(group);
                }
            }
        }
        Ok(TrashResult {
            outcomes,
            total_groups: self.groups.iter().flatten().count(),
            affected_groups: affected_ids
                .into_iter()
                .filter_map(|id| {
                    self.groups[id]
                        .as_ref()
                        .map(|group| self.group_view(id, group))
                })
                .collect(),
        })
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum TrashStatus {
    Moved,
    Skipped,
    Failed,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrashOutcome {
    id: usize,
    relative_path: String,
    status: TrashStatus,
    reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrashResult {
    outcomes: Vec<TrashOutcome>,
    total_groups: usize,
    affected_groups: Vec<GroupView>,
}

fn page_offset(page: usize) -> usize {
    page.saturating_mul(PAGE_SIZE)
}

fn checkpoint(cancelled: &AtomicBool) -> ControlFlow<()> {
    if cancelled.load(Ordering::Relaxed) {
        ControlFlow::Break(())
    } else {
        ControlFlow::Continue(())
    }
}

fn cache_failure(
    cache: &mut Option<Cache>,
    warning: &mut Option<String>,
    error: impl std::fmt::Display,
) {
    *cache = None;
    *warning = Some(cache_message(error));
}

fn cache_message(error: impl std::fmt::Display) -> String {
    format!("照片索引不可用，其余照片将直接读取。{error}")
}

fn discover(
    root: &Root,
    cancelled: &AtomicBool,
    progress: &mut impl FnMut(Progress),
) -> ControlFlow<(), (Vec<PathBuf>, usize, Vec<Issue>)> {
    let mut paths = Vec::new();
    let mut unsupported = 0;
    let mut issues = Vec::new();
    let mut last = Instant::now();
    progress(Progress::Scanning { found: 0 });
    let mut entries = WalkDir::new(&root.path)
        .follow_links(false)
        .min_depth(1)
        .into_iter();
    while let Some(entry) = entries.next() {
        checkpoint(cancelled)?;
        if let Ok(entry) = entry {
            if !crate::library::include_in_scan(&entry) {
                if entry.file_type().is_dir() {
                    entries.skip_current_dir();
                }
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&root.path)
                .expect("directory entry below root");
            match entry.metadata() {
                Ok(metadata) if is_link(&metadata) => {
                    if entry.file_type().is_dir() {
                        entries.skip_current_dir();
                    }
                }
                Ok(metadata) if metadata.is_file() && imaging::supported(relative) => {
                    paths.push(relative.to_path_buf());
                }
                Ok(metadata)
                    if metadata.is_file()
                        && (imaging::recognizes_format(relative)
                            || Media::from_path(relative) == Some(Media::Photo)) =>
                {
                    unsupported += 1;
                    issues.push(Issue {
                        path: display_path(relative),
                        reason: "暂不支持此照片格式。".into(),
                    });
                }
                Ok(_) => {}
                Err(error) => {
                    if entry.file_type().is_dir() {
                        entries.skip_current_dir();
                    }
                    issues.push(Issue {
                        path: display_path(relative),
                        reason: error.to_string(),
                    });
                }
            }
        } else if let Err(error) = entry {
            let path = error
                .path()
                .and_then(|path| path.strip_prefix(&root.path).ok());
            issues.push(Issue {
                path: path.map_or_else(|| ".".into(), display_path),
                reason: error.to_string(),
            });
        }
        if last.elapsed() >= PROGRESS_INTERVAL {
            last = Instant::now();
            progress(Progress::Scanning {
                found: paths.len() + unsupported,
            });
        }
    }
    paths.sort_unstable();
    progress(Progress::Scanning {
        found: paths.len() + unsupported,
    });
    ControlFlow::Continue((paths, unsupported, issues))
}

fn open_photo(
    root: &Root,
    relative: &Path,
    id: usize,
) -> std::io::Result<(std::fs::File, Candidate)> {
    let file = root.open_file(relative)?;
    let stamp = FileStamp::read(&file)?;
    Ok((
        file,
        Candidate {
            id,
            relative: relative.to_path_buf(),
            stamp,
        },
    ))
}

fn run_scan(
    id: u64,
    root: Root,
    database: Result<&Path, &str>,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> ControlFlow<(), (Scan, Summary)> {
    let (paths, unsupported, mut issues) = discover(&root, cancelled, &mut progress)?;
    let fully_traversed = issues.len() == unsupported;
    let mut cache_warning = None;
    let mut cache = match database
        .map_err(str::to_owned)
        .and_then(|database| Cache::open(database, &root.path))
    {
        Ok(cache) => Some(cache),
        Err(error) => {
            cache_warning = Some(cache_message(error));
            None
        }
    };
    let mut items: Vec<(Candidate, Features)> = Vec::new();
    let mut pending = Vec::with_capacity(CACHE_BATCH);
    let mut parser = MediaParser::new();
    let hasher = imaging::hasher();
    let mut reused = 0;
    let mut last = Instant::now();
    progress(Progress::Extracting {
        processed: 0,
        total: paths.len(),
    });
    for (processed, relative) in paths.iter().enumerate() {
        checkpoint(cancelled)?;
        let photo = (|| {
            let (mut file, candidate) =
                open_photo(&root, relative, items.len()).map_err(|error| error.to_string())?;
            let cached = match cache
                .as_ref()
                .map(|cache| cache.get(&candidate))
                .transpose()
            {
                Ok(cached) => cached.flatten(),
                Err(error) => {
                    cache_failure(&mut cache, &mut cache_warning, error);
                    None
                }
            };
            let hit = cached.is_some();
            let features = match cached {
                Some(features) => Ok(features),
                None => imaging::features(&mut file, &candidate, &mut parser, &hasher, cancelled),
            };
            root.verify_read(&file, &candidate)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((candidate, features?, hit))
        })();
        checkpoint(cancelled)?;
        match photo {
            Ok((file, features, hit)) => {
                if hit {
                    reused += 1;
                } else if cache.is_some() {
                    pending.push(file.id);
                }
                items.push((file, features));
            }
            Err(reason) => issues.push(Issue {
                path: display_path(relative),
                reason,
            }),
        }
        if pending.len() == CACHE_BATCH {
            save_batch(&mut cache, &mut cache_warning, &mut pending, &items);
        }
        if last.elapsed() >= PROGRESS_INTERVAL {
            last = Instant::now();
            progress(Progress::Extracting {
                processed: processed + 1,
                total: paths.len(),
            });
        }
    }
    checkpoint(cancelled)?;
    save_batch(&mut cache, &mut cache_warning, &mut pending, &items);
    progress(Progress::Extracting {
        processed: paths.len(),
        total: paths.len(),
    });

    let total = items.len();
    let mut last = Instant::now();
    progress(Progress::Matching {
        processed: 0,
        total,
    });
    let features: Vec<_> = items.iter().map(|(_, feature)| feature).collect();
    let groups = matching::group(&features, |processed| {
        if processed == total || last.elapsed() >= PROGRESS_INTERVAL {
            last = Instant::now();
            progress(Progress::Matching { processed, total });
        }
        checkpoint(cancelled)
    })?;

    if fully_traversed && let Some(index) = cache.as_mut() {
        let remaining = paths.iter().map(|path| cache::key(path)).collect();
        if let Err(error) = index.prune(&remaining) {
            cache_failure(&mut cache, &mut cache_warning, error);
        }
    }
    checkpoint(cancelled)?;

    let summary = Summary {
        scan_id: id,
        found: paths.len() + unsupported,
        checked: items.len(),
        reused,
        unsupported,
        skipped: issues.len(),
        groups: groups.len(),
        cache_warning,
    };
    let photos = items
        .into_iter()
        .map(|(file, features)| Photo {
            file,
            features,
            moved: false,
        })
        .collect();
    ControlFlow::Continue((
        Scan {
            id,
            root,
            photos,
            groups: groups.into_iter().map(Some).collect(),
            issues,
        },
        summary,
    ))
}

fn save_batch(
    cache: &mut Option<Cache>,
    warning: &mut Option<String>,
    pending: &mut Vec<usize>,
    items: &[(Candidate, Features)],
) {
    if !pending.is_empty()
        && let Some(index) = cache.as_mut()
    {
        if let Err(error) = index.save(pending.iter().map(|&id| (&items[id].0, &items[id].1))) {
            cache_failure(cache, warning, error);
        }
    }
    pending.clear();
}

fn result(scan_id: u64) -> Result<Arc<RwLock<Scan>>, String> {
    let scan = RESULT
        .lock()
        .map_err(|_| STATE_ERROR)?
        .as_ref()
        .cloned()
        .ok_or("扫描结果已失效，请重新扫描。")?;
    if scan.read().map_err(|_| STATE_ERROR)?.id != scan_id {
        return Err("扫描结果已失效，请重新扫描。".into());
    }
    Ok(scan)
}

#[tauri::command]
pub(crate) async fn scan_similar(
    app: AppHandle,
    directory: String,
    scan_id: u64,
    progress: Channel<Progress>,
) -> Result<Outcome, String> {
    let database = app
        .path()
        .app_local_data_dir()
        .map(|path| path.join("similar-photos.sqlite3"))
        .map_err(|error| error.to_string());
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let root = Root::new(Path::new(&directory))
            .map_err(|error| format!("无法访问扫描目录。{error}"))?;
        *RESULT.lock().map_err(|_| STATE_ERROR)? = None;
        let cancelled = Arc::new(AtomicBool::new(false));
        *ACTIVE.lock().map_err(|_| STATE_ERROR)? = Some(Active {
            id: scan_id,
            cancelled: cancelled.clone(),
        });
        let scan = run_scan(
            scan_id,
            root,
            database.as_deref().map_err(String::as_str),
            &cancelled,
            |update| {
                let _ = progress.send(update);
            },
        );
        let mut active = ACTIVE.lock().map_err(|_| STATE_ERROR)?;
        *active = None;
        match scan {
            ControlFlow::Continue((scan, summary)) if !cancelled.load(Ordering::Relaxed) => {
                *RESULT.lock().map_err(|_| STATE_ERROR)? = Some(Arc::new(RwLock::new(scan)));
                Ok(Outcome::Finished { summary })
            }
            _ => Ok(Outcome::Cancelled),
        }
    })
    .await
    .map_err(|error| format!("相似照片扫描异常结束。{error}"))?
}

#[tauri::command]
pub(crate) fn cancel_similar(scan_id: u64) -> Result<(), String> {
    let active = ACTIVE.lock().map_err(|_| STATE_ERROR)?;
    if let Some(scan) = active.as_ref().filter(|scan| scan.id == scan_id) {
        scan.cancelled.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn similar_groups(
    scan_id: u64,
    kind: Option<MatchKind>,
    page: usize,
) -> Result<Page<GroupView>, String> {
    Ok(result(scan_id)?
        .read()
        .map_err(|_| STATE_ERROR)?
        .groups(kind, page))
}

#[tauri::command]
pub(crate) fn similar_members(
    scan_id: u64,
    group_id: usize,
    page: usize,
) -> Result<Page<MemberView>, String> {
    result(scan_id)?
        .read()
        .map_err(|_| STATE_ERROR)?
        .members(group_id, page)
}

#[tauri::command]
pub(crate) fn similar_issues(scan_id: u64, page: usize) -> Result<Page<Issue>, String> {
    let session = result(scan_id)?;
    let scan = session.read().map_err(|_| STATE_ERROR)?;
    Ok(Page {
        total: scan.issues.len(),
        items: scan
            .issues
            .iter()
            .skip(page_offset(page))
            .take(PAGE_SIZE)
            .cloned()
            .collect(),
    })
}

#[tauri::command]
pub(crate) async fn similar_preview(
    scan_id: u64,
    file_id: usize,
    detail: bool,
    large: Option<bool>,
) -> Result<String, String> {
    let size = match (large.unwrap_or(false), detail) {
        (true, _) => PreviewSize::Large,
        (false, true) => PreviewSize::Detail,
        (false, false) => PreviewSize::Thumbnail,
    };
    tauri::async_runtime::spawn_blocking(move || {
        let session = result(scan_id)?;
        let scan = session.read().map_err(|_| STATE_ERROR)?;
        preview(&scan.root, &scan.active_photo(file_id)?.file, size)
    })
    .await
    .map_err(|error| format!("无法加载照片预览。{error}"))?
}

#[tauri::command]
pub(crate) async fn similar_media_details(
    scan_id: u64,
    file_id: usize,
) -> Result<MediaDetails, String> {
    let permit = DETAILS_SLOTS.acquire().await.map_err(|_| STATE_ERROR)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        let session = result(scan_id)?;
        let details = session
            .read()
            .map_err(|_| STATE_ERROR)?
            .media_details(file_id)?;
        result(scan_id)?;
        Ok(details)
    })
    .await
    .map_err(|error| format!("无法读取照片详情。{error}"))?
}

#[tauri::command]
pub(crate) async fn trash_similar(
    scan_id: u64,
    group_id: usize,
    file_ids: Vec<usize>,
) -> Result<TrashResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let session = result(scan_id)?;
        let mut scan = session.write().map_err(|_| STATE_ERROR)?;
        scan.recycle(scan_id, group_id, file_ids, crate::duplicates::trash)
    })
    .await
    .map_err(|error| format!("清理相似照片异常结束。{error}"))?
}

fn preview(root: &Root, photo: &Candidate, size: PreviewSize) -> Result<String, String> {
    let _preview = PREVIEW.lock().map_err(|_| STATE_ERROR)?;
    let stale = |_| "照片已变化或已移除，请重新扫描。";
    let mut file = root.open_matching(photo).map_err(stale)?;
    let image = imaging::render_preview(&mut file, &photo.relative, photo.stamp.size, size);
    root.verify_read(&file, photo).map_err(stale)?;
    image
}

#[cfg(test)]
mod tests;
