//! Bounded IPC views and session-scoped selection; originals always remain in Scan.
use super::*;
use crate::media_date::Media;
use serde::Deserialize;
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::{AppHandle, ipc::Channel};

const PAGE_SIZE: usize = 25;
const STALE: &str = "扫描结果已过期，请重新扫描。";
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static ACTIVE: Mutex<Option<(u64, Arc<AtomicBool>)>> = Mutex::new(None);
// Tauri may dispatch a synchronous cancel before polling the async scan future.
// Keep just one pending token; old tokens cannot cancel a different scan.
static PENDING_CANCEL: Mutex<Option<u64>> = Mutex::new(None);

struct Session {
    scan: Scan,
    selected: HashSet<usize>,
    revision: u64,
    outcomes: Vec<TrashOutcome>,
    previews: crate::library::PreviewCache,
}

fn current(guard: &mut Option<Session>, id: u64) -> Result<&mut Session, String> {
    guard
        .as_mut()
        .filter(|s| s.scan.id == id)
        .ok_or_else(|| STALE.into())
}
fn session() -> Result<MutexGuard<'static, Option<Session>>, String> {
    SESSION
        .lock()
        .map_err(|_| "查重任务异常结束，请重启应用。".into())
}

#[derive(Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Filter {
    #[serde(default)]
    query: String,
    kind: Option<Kind>,
}
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Kind {
    Photo,
    Video,
    Other,
}
fn kind(file: &Candidate) -> Kind {
    match Media::from_path(&file.relative) {
        Some(Media::Photo | Media::LivePhoto) => Kind::Photo,
        Some(Media::Video) => Kind::Video,
        None => Kind::Other,
    }
}
fn matches(group: &Group, filter: &Filter) -> bool {
    let query = filter.query.to_lowercase();
    group.files.iter().any(|f| {
        filter.kind.is_none_or(|k| kind(f) == k)
            && (query.is_empty() || f.path_label().to_lowercase().contains(&query))
    })
}
fn capacity(group: &Group) -> usize {
    if group.needs_rescan {
        return 0;
    }
    let regular = group.files.iter().filter(|f| !f.stamp.hard_link()).count();
    regular.min(group.files.len().saturating_sub(1))
}
fn offset(page: usize) -> usize {
    page.saturating_mul(PAGE_SIZE)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Summary {
    scan_id: u64,
    scanned: usize,
    groups: usize,
    files: usize,
    extras: usize,
    extra_bytes: u64,
    issues: usize,
    bytes_read: u64,
    elapsed_ms: u64,
    selected: usize,
    selected_bytes: u64,
    hidden_selected: usize,
    revision: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileRow {
    #[serde(flatten)]
    file: FileView,
    kind: Kind,
    size: u64,
    preview_available: bool,
    selected: bool,
    disabled: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupRow {
    id: usize,
    size: u64,
    count: usize,
    selected: usize,
    extra_bytes: u64,
    needs_rescan: bool,
    representative: FileRow,
}
#[derive(Serialize)]
pub(crate) struct Page<T> {
    total: usize,
    items: Vec<T>,
}
#[derive(Serialize)]
pub(crate) struct GroupPage {
    total: usize,
    items: Vec<GroupRow>,
    summary: Summary,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Sort {
    ExtraBytes,
    Size,
    Count,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum KeepRule {
    Shallow,
    Oldest,
    Newest,
}
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum SelectionAction {
    Toggle {
        file_id: usize,
    },
    Clear,
    Apply {
        filter: Filter,
        rule: KeepRule,
        preferred_directory: String,
    },
}
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub(crate) enum Outcome {
    Finished { summary: Summary },
    Cancelled,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfirmationRow {
    group_id: usize,
    size: u64,
    moving: bool,
    file: FileRow,
}
#[derive(Serialize)]
pub(crate) struct Confirmation {
    total: usize,
    items: Vec<ConfirmationRow>,
    summary: Summary,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchSummary {
    moved: usize,
    skipped: usize,
    failed: usize,
}

impl Session {
    fn new(scan: Scan) -> Self {
        Self {
            scan,
            selected: HashSet::new(),
            revision: 0,
            outcomes: Vec::new(),
            previews: Default::default(),
        }
    }
    fn summary(&self, filter: &Filter) -> Summary {
        let mut summary = Summary {
            scan_id: self.scan.id,
            scanned: self.scan.scanned,
            groups: self.scan.groups.len(),
            files: 0,
            extras: 0,
            extra_bytes: 0,
            issues: self.scan.issues.len(),
            bytes_read: self.scan.bytes_read,
            elapsed_ms: self.scan.elapsed_ms,
            selected: self.selected.len(),
            selected_bytes: 0,
            hidden_selected: 0,
            revision: self.revision,
        };
        for g in &self.scan.groups {
            summary.files += g.files.len();
            summary.extras += capacity(g);
            summary.extra_bytes += g.files[0].stamp.size.saturating_mul(capacity(g) as u64);
            let count = g
                .files
                .iter()
                .filter(|f| self.selected.contains(&f.id))
                .count();
            summary.selected_bytes += g.files[0].stamp.size.saturating_mul(count as u64);
            if count > 0 && !matches(g, filter) {
                summary.hidden_selected += count;
            }
        }
        summary
    }
    fn row(&self, g: &Group, f: &Candidate, selected_count: usize) -> FileRow {
        FileRow {
            file: f.view(),
            kind: kind(f),
            size: f.stamp.size,
            preview_available: matches!(
                Media::from_path(&f.relative),
                Some(Media::Video | Media::LivePhoto)
            ) || crate::similar::supports_preview(&f.relative),
            selected: self.selected.contains(&f.id),
            disabled: g.needs_rescan
                || f.stamp.hard_link()
                || (!self.selected.contains(&f.id) && selected_count + 1 >= g.files.len()),
        }
    }
    fn groups(&self, filter: &Filter, sort: Sort, page: usize) -> GroupPage {
        let mut groups: Vec<_> = self
            .scan
            .groups
            .iter()
            .filter(|g| matches(g, filter))
            .collect();
        groups.sort_by(|a, b| {
            let value = |g: &Group| match sort {
                Sort::ExtraBytes => g.files[0].stamp.size.saturating_mul(capacity(g) as u64),
                Sort::Size => g.files[0].stamp.size,
                Sort::Count => g.files.len() as u64,
            };
            value(b)
                .cmp(&value(a))
                .then_with(|| a.files[0].relative.cmp(&b.files[0].relative))
                .then_with(|| a.id.cmp(&b.id))
        });
        GroupPage {
            total: groups.len(),
            summary: self.summary(filter),
            items: groups
                .into_iter()
                .skip(offset(page))
                .take(PAGE_SIZE)
                .map(|g| {
                    let selected = g
                        .files
                        .iter()
                        .filter(|f| self.selected.contains(&f.id))
                        .count();
                    GroupRow {
                        id: g.id,
                        size: g.files[0].stamp.size,
                        count: g.files.len(),
                        selected,
                        extra_bytes: g.files[0].stamp.size.saturating_mul(capacity(g) as u64),
                        needs_rescan: g.needs_rescan,
                        representative: self.row(g, &g.files[0], selected),
                    }
                })
                .collect(),
        }
    }
    fn members(&self, id: usize, page: usize) -> Result<Page<FileRow>, String> {
        let g = self.scan.groups.iter().find(|g| g.id == id).ok_or(STALE)?;
        let selected = g
            .files
            .iter()
            .filter(|f| self.selected.contains(&f.id))
            .count();
        Ok(Page {
            total: g.files.len(),
            items: g
                .files
                .iter()
                .skip(offset(page))
                .take(PAGE_SIZE)
                .map(|f| self.row(g, f, selected))
                .collect(),
        })
    }
    fn selection(&mut self, action: SelectionAction) -> Result<(), String> {
        match action {
            SelectionAction::Clear => self.selected.clear(),
            SelectionAction::Toggle { file_id } => {
                let g = self
                    .scan
                    .groups
                    .iter()
                    .find(|g| g.files.iter().any(|f| f.id == file_id))
                    .ok_or(STALE)?;
                let f = g.files.iter().find(|f| f.id == file_id).unwrap();
                if self.selected.contains(&file_id) {
                    self.selected.remove(&file_id);
                } else {
                    let count = g
                        .files
                        .iter()
                        .filter(|f| self.selected.contains(&f.id))
                        .count();
                    if self.row(g, f, count).disabled {
                        return Err("该文件需要保留，或本组已变化。".into());
                    }
                    self.selected.insert(file_id);
                }
            }
            SelectionAction::Apply {
                filter,
                rule,
                preferred_directory,
            } => {
                let preferred = validate_preferred(&self.scan.root, &preferred_directory)?;
                let mut selected = HashSet::new();
                for g in self
                    .scan
                    .groups
                    .iter()
                    .filter(|g| !g.needs_rescan && matches(g, &filter))
                {
                    let keep = g
                        .files
                        .iter()
                        .min_by(|a, b| {
                            let priority = |f: &Candidate| {
                                !preferred
                                    .as_ref()
                                    .is_some_and(|p| f.relative.starts_with(p))
                            };
                            priority(a)
                                .cmp(&priority(b))
                                .then_with(|| match rule {
                                    KeepRule::Shallow => a
                                        .relative
                                        .components()
                                        .count()
                                        .cmp(&b.relative.components().count()),
                                    KeepRule::Oldest => a.stamp.modified.cmp(&b.stamp.modified),
                                    KeepRule::Newest => b.stamp.modified.cmp(&a.stamp.modified),
                                })
                                .then_with(|| a.relative.cmp(&b.relative))
                        })
                        .unwrap();
                    let has_hard_link = g.files.iter().any(|f| f.stamp.hard_link());
                    selected.extend(
                        g.files
                            .iter()
                            .filter(|f| !f.stamp.hard_link() && (has_hard_link || f.id != keep.id))
                            .map(|f| f.id),
                    );
                }
                self.selected = selected;
            }
        }
        self.revision += 1;
        Ok(())
    }
    fn confirmation(&self, revision: u64, page: usize) -> Result<Confirmation, String> {
        self.check_revision(revision)?;
        let affected = || {
            self.scan
                .groups
                .iter()
                .filter(|g| g.files.iter().any(|f| self.selected.contains(&f.id)))
        };
        let total = affected().map(|g| g.files.len()).sum();
        let items = affected()
            .flat_map(|g| {
                let count = g
                    .files
                    .iter()
                    .filter(|f| self.selected.contains(&f.id))
                    .count();
                g.files.iter().map(move |f| (g, f, count))
            })
            .skip(offset(page))
            .take(PAGE_SIZE)
            .map(|(g, f, count)| ConfirmationRow {
                group_id: g.id,
                size: f.stamp.size,
                moving: self.selected.contains(&f.id),
                file: self.row(g, f, count),
            })
            .collect();
        Ok(Confirmation {
            total,
            items,
            summary: self.summary(&Filter::default()),
        })
    }
    fn check_revision(&self, revision: u64) -> Result<(), String> {
        if revision != self.revision {
            return Err("勾选已变化，请重新打开确认窗口。".into());
        }
        Ok(())
    }
    fn recycle(
        &mut self,
        revision: u64,
        trash: impl FnMut(&Path) -> Result<(), String>,
    ) -> Result<BatchSummary, String> {
        self.check_revision(revision)?;
        let result =
            self.scan
                .recycle(self.scan.id, self.selected.iter().copied().collect(), trash)?;
        self.outcomes = result.outcomes;
        // Only still-valid failures remain selected for retry.
        let valid: HashSet<_> = self
            .scan
            .groups
            .iter()
            .filter(|g| !g.needs_rescan)
            .flat_map(|g| g.files.iter().map(|f| f.id))
            .collect();
        self.selected.retain(|id| valid.contains(id));
        self.revision += 1;
        let mut counts = BatchSummary {
            moved: 0,
            skipped: 0,
            failed: 0,
        };
        for outcome in &self.outcomes {
            match outcome.status {
                TrashStatus::Moved => counts.moved += 1,
                TrashStatus::Skipped => counts.skipped += 1,
                TrashStatus::Failed => counts.failed += 1,
            }
        }
        Ok(counts)
    }
    fn file(&self, id: usize) -> Result<&Candidate, String> {
        self.scan
            .groups
            .iter()
            .flat_map(|g| &g.files)
            .find(|f| f.id == id)
            .ok_or_else(|| STALE.into())
    }
    fn path(&self, id: usize) -> Result<PathBuf, String> {
        let f = self.file(id)?;
        self.scan.root.open_matching(f).map_err(|e| e.to_string())?;
        Ok(self.scan.root.path.join(&f.relative))
    }
}

fn validate_preferred(root: &Root, text: &str) -> Result<Option<PathBuf>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let relative = Path::new(text);
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("请输入扫描目录内的相对子目录。".into());
    }
    root.verify_directory().map_err(|e| e.to_string())?;
    let mut path = root.path.clone();
    for c in relative.components() {
        path.push(c);
        let m = fs::symlink_metadata(&path).map_err(|e| format!("无法访问优先保留目录。{e}"))?;
        if is_link(&m) || !m.is_dir() {
            return Err("优先保留目录必须是扫描目录内的普通子目录。".into());
        }
    }
    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
    let relative = canonical
        .strip_prefix(&root.path)
        .map_err(|_| "优先保留目录已不在扫描目录内。")?;
    Ok(Some(relative.to_owned()))
}

struct ActiveGuard(Arc<AtomicBool>);
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE.lock() {
            if active
                .as_ref()
                .is_some_and(|(_, flag)| Arc::ptr_eq(flag, &self.0))
            {
                *active = None;
            }
        }
    }
}

// Register before spawning, so an immediate cancel cannot race task startup.
#[tauri::command]
pub(crate) async fn scan_duplicates(
    directory: String,
    scan_id: u64,
    progress: Channel<ScanProgress>,
) -> Result<Outcome, String> {
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let _task = crate::file_task()?;
        let mut active = ACTIVE.lock().map_err(|_| "无法读取扫描状态。")?;
        if active.is_some() {
            return Err("已有查重任务正在运行。".into());
        }
        if PENDING_CANCEL
            .lock()
            .map_err(|_| "无法读取取消状态。")?
            .take()
            == Some(scan_id)
        {
            cancelled.store(true, Ordering::Relaxed);
        }
        *active = Some((scan_id, cancelled.clone()));
    }
    let guard = ActiveGuard(cancelled.clone());
    *session()? = None;
    // The worker reacquires FILE_TASK; ACTIVE rejects another duplicate scan
    // during startup, and a competing file task makes this scan fail safely.
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let result = (|| {
            let _task = crate::file_task()?;
            let scan = run_scan(
                scan_id,
                Path::new(&directory),
                &cancelled,
                |p| {
                    let _ = progress.send(p);
                },
                true,
            )?;
            Ok::<_, String>(Session::new(scan))
        })();
        let mut active = ACTIVE.lock().map_err(|_| "无法读取扫描状态。")?;
        let outcome = if cancelled.load(Ordering::Relaxed) {
            Ok(Outcome::Cancelled)
        } else {
            result.and_then(|new| {
                let summary = new.summary(&Filter::default());
                *session()? = Some(new);
                Ok(Outcome::Finished { summary })
            })
        };
        *active = None;
        drop(active);
        outcome
    })
    .await
    .map_err(|e| format!("查重任务异常结束。{e}"))?
}
#[tauri::command]
pub(crate) fn cancel_duplicates(scan_id: u64) -> Result<(), String> {
    let active = ACTIVE.lock().map_err(|_| "无法读取扫描状态。")?;
    if let Some((_, flag)) = active.as_ref().filter(|(id, _)| *id == scan_id) {
        flag.store(true, Ordering::Relaxed);
    } else if active.is_none() {
        *PENDING_CANCEL.lock().map_err(|_| "无法读取取消状态。")? = Some(scan_id);
    }
    Ok(())
}
#[tauri::command]
pub(crate) fn duplicate_groups(
    scan_id: u64,
    filter: Filter,
    sort: Sort,
    page: usize,
) -> Result<GroupPage, String> {
    Ok(current(&mut *session()?, scan_id)?.groups(&filter, sort, page))
}
#[tauri::command]
pub(crate) fn duplicate_members(
    scan_id: u64,
    group_id: usize,
    page: usize,
) -> Result<Page<FileRow>, String> {
    current(&mut *session()?, scan_id)?.members(group_id, page)
}
#[tauri::command]
pub(crate) fn duplicate_selection(
    scan_id: u64,
    action: SelectionAction,
    filter: Filter,
) -> Result<Summary, String> {
    let mut guard = session()?;
    let s = current(&mut guard, scan_id)?;
    s.selection(action)?;
    Ok(s.summary(&filter))
}
#[tauri::command]
pub(crate) fn duplicate_confirmation(
    scan_id: u64,
    revision: u64,
    page: usize,
) -> Result<Confirmation, String> {
    current(&mut *session()?, scan_id)?.confirmation(revision, page)
}
#[tauri::command]
pub(crate) fn duplicate_issues(scan_id: u64, page: usize) -> Result<Page<ScanIssue>, String> {
    let mut guard = session()?;
    let s = current(&mut guard, scan_id)?;
    Ok(Page {
        total: s.scan.issues.len(),
        items: s
            .scan
            .issues
            .iter()
            .skip(offset(page))
            .take(PAGE_SIZE)
            .cloned()
            .collect(),
    })
}
#[tauri::command]
pub(crate) fn duplicate_outcomes(scan_id: u64, page: usize) -> Result<Page<TrashOutcome>, String> {
    let mut guard = session()?;
    let s = current(&mut guard, scan_id)?;
    Ok(Page {
        total: s.outcomes.len(),
        items: s
            .outcomes
            .iter()
            .skip(offset(page))
            .take(PAGE_SIZE)
            .cloned()
            .collect(),
    })
}
#[tauri::command]
pub(crate) async fn trash_duplicates(scan_id: u64, revision: u64) -> Result<BatchSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        current(&mut *session()?, scan_id)?.recycle(revision, trash)
    })
    .await
    .map_err(|e| format!("清理任务异常结束。{e}"))?
}
static THUMBNAILS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(3);
static DETAILS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
#[tauri::command]
pub(crate) async fn duplicate_preview(
    scan_id: u64,
    file_id: usize,
    detail: bool,
) -> Result<String, String> {
    let slot = if detail { &DETAILS } else { &THUMBNAILS }
        .acquire()
        .await
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _slot = slot;
        let (root, file) = {
            let mut guard = session()?;
            let s = current(&mut guard, scan_id)?;
            let f = s.file(file_id)?;
            s.scan.root.open_matching(f).map_err(|e| e.to_string())?;
            let file = f.clone();
            if let Some(image) = s.previews.get((file_id, detail)) {
                return Ok(image);
            }
            (s.scan.root.clone(), file)
        };
        let image = crate::library::preview_candidate(&root, &file, detail)?;
        let mut guard = session()?;
        let s = current(&mut guard, scan_id)?;
        s.scan
            .root
            .open_matching(s.file(file_id)?)
            .map_err(|e| e.to_string())?;
        s.previews.insert((file_id, detail), &image);
        Ok(image)
    })
    .await
    .map_err(|e| format!("无法预览文件。{e}"))?
}
#[tauri::command]
pub(crate) async fn reveal_duplicate(
    scan_id: u64,
    file_id: usize,
) -> Result<crate::library::RevealResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = current(&mut *session()?, scan_id)?.path(file_id)?;
        crate::library::reveal_path(&path, crate::library::platform::reveal, |p| {
            tauri_plugin_opener::open_path(p, None::<&str>).map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub(crate) async fn copy_duplicate_path(
    app: AppHandle,
    scan_id: u64,
    file_id: usize,
) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    tauri::async_runtime::spawn_blocking(move || {
        let path = current(&mut *session()?, scan_id)?.path(file_id)?;
        app.clipboard()
            .write_text(crate::library::clipboard_path(&path)?)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests;
