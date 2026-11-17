mod favorites;
mod platform;

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use nom_exif::MediaParser;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager};
use tempfile::NamedTempFile;
use walkdir::{DirEntry, WalkDir};

use crate::duplicates::{Candidate, FileStamp, Root, display_path, is_link};
use crate::media_date::{self, Media};

const PAGE_SIZE: usize = 24;
const STATE_ERROR: &str = "无法读取媒体库状态，请重启应用后重试。";
const STALE_SCAN: &str = "媒体库已变化，请刷新。";
static SCAN_ID: AtomicU64 = AtomicU64::new(1);
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

pub(crate) fn include_in_scan(entry: &DirEntry) -> bool {
    entry.depth() == 0 || entry.file_name() != favorites::DATA_DIR
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Kind {
    Photo,
    Video,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Filter {
    kind: Option<Kind>,
    from: Option<String>,
    to: Option<String>,
    unknown_date: bool,
    favorites_only: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct Issue {
    path: String,
    reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Summary {
    scan_id: u64,
    total: usize,
    issues: usize,
    preference_warning: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum OpenMode {
    Select,
    Refresh,
}

#[derive(Serialize)]
pub(crate) struct OpenedLibrary {
    directory: String,
    summary: Summary,
}

#[derive(Serialize)]
pub(crate) struct Progress {
    found: usize,
}

#[derive(Serialize)]
pub(crate) struct Page<T> {
    page: usize,
    total: usize,
    items: Vec<T>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemView {
    id: usize,
    name: String,
    relative_path: String,
    size: u64,
    kind: Kind,
    captured_on: Option<String>,
    preview_available: bool,
    favorite: bool,
}

struct Item {
    file: Candidate,
    kind: Kind,
    captured_on: Option<NaiveDate>,
    preview_available: bool,
}

impl Item {
    fn read(
        root: &Root,
        relative: PathBuf,
        media: Media,
        parser: &mut MediaParser,
    ) -> std::io::Result<Self> {
        let file = root.open_file(&relative)?;
        let stamp = FileStamp::read(&file)?;
        let captured_on = media_date::from_open_file(parser, file, media)
            .or_else(|| media_date::from_filename(&relative));
        let kind = if media == Media::Video {
            Kind::Video
        } else {
            Kind::Photo
        };
        let preview_available = kind == Kind::Photo && crate::similar::supports_preview(&relative);
        Ok(Self {
            file: Candidate {
                id: 0,
                relative,
                stamp,
            },
            kind,
            captured_on,
            preview_available,
        })
    }

    fn view(&self, favorite: bool) -> ItemView {
        ItemView {
            id: self.file.id,
            name: display_path(Path::new(
                self.file.relative.file_name().expect("media file name"),
            )),
            relative_path: self.file.path_label(),
            size: self.file.stamp.size,
            kind: self.kind,
            captured_on: self.captured_on.map(|date| date.to_string()),
            preview_available: self.preview_available,
            favorite,
        }
    }
}

struct Scan {
    id: u64,
    root: Root,
    items: Vec<Item>,
    issues: Vec<Issue>,
}

impl Scan {
    fn item(&self, id: usize) -> Result<&Item, String> {
        self.items.get(id).ok_or_else(|| STALE_SCAN.into())
    }

    fn read(id: u64, root: Root, mut progress: impl FnMut(Progress)) -> Self {
        let mut items = Vec::new();
        let mut issues = Vec::new();
        let mut parser = MediaParser::new();
        let mut found = 0;
        let mut last_update = Instant::now();
        progress(Progress { found });

        let mut entries = WalkDir::new(&root.path)
            .follow_links(false)
            .min_depth(1)
            .into_iter();
        while let Some(entry) = entries.next() {
            let result = entry.and_then(|entry| entry.metadata().map(|metadata| (entry, metadata)));
            let (entry, metadata) = match result {
                Ok(pair) => pair,
                Err(error) => {
                    issues.push(Issue {
                        path: display_path(
                            error
                                .path()
                                .and_then(|path| path.strip_prefix(&root.path).ok())
                                .unwrap_or(Path::new(".")),
                        ),
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            if !include_in_scan(&entry) || is_link(&metadata) {
                if entry.file_type().is_dir() {
                    entries.skip_current_dir();
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&root.path)
                .expect("entry under scan root");
            let Some(media) = Media::from_path(relative) else {
                continue;
            };
            found += 1;
            match Item::read(&root, relative.to_path_buf(), media, &mut parser) {
                Ok(item) => items.push(item),
                Err(error) => issues.push(Issue {
                    path: display_path(relative),
                    reason: error.to_string(),
                }),
            }
            if last_update.elapsed() >= Duration::from_millis(150) {
                progress(Progress { found });
                last_update = Instant::now();
            }
        }

        items.sort_unstable_by(|a, b| {
            b.captured_on
                .cmp(&a.captured_on)
                .then_with(|| a.file.relative.cmp(&b.file.relative))
        });
        for (id, item) in items.iter_mut().enumerate() {
            item.file.id = id;
        }
        Self {
            id,
            root,
            items,
            issues,
        }
    }
}

fn parse_date(date: Option<&str>) -> Result<Option<NaiveDate>, String> {
    date.map(|date| {
        NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| "拍摄日期格式无效。".into())
    })
    .transpose()
}

impl<T> Page<T> {
    fn bounds(requested: usize, total: usize) -> (usize, usize) {
        let page = requested.min(total.saturating_sub(1) / PAGE_SIZE);
        (page, page * PAGE_SIZE)
    }
}

struct Session {
    scan: Arc<Scan>,
    favorites: HashSet<usize>,
}

impl Session {
    fn from_scan(scan: Scan, keys: HashSet<Vec<u8>>) -> Self {
        let favorites = scan
            .items
            .iter()
            .filter(|item| keys.contains(&favorites::path_key(&item.file.relative)))
            .map(|item| item.file.id)
            .collect();
        Self {
            scan: Arc::new(scan),
            favorites,
        }
    }

    fn items(&self, filter: Filter, requested: usize) -> Result<Page<ItemView>, String> {
        let from = parse_date(filter.from.as_deref())?;
        let to = parse_date(filter.to.as_deref())?;
        if matches!((from, to), (Some(from), Some(to)) if from > to) {
            return Err("开始日期应早于或等于结束日期。".into());
        }
        let matching = self.scan.items.iter().filter(|item| {
            filter.kind.is_none_or(|kind| item.kind == kind)
                && (!filter.favorites_only || self.favorites.contains(&item.file.id))
                && if filter.unknown_date {
                    item.captured_on.is_none()
                } else {
                    from.is_none_or(|from| item.captured_on >= Some(from))
                        && to.is_none_or(|to| item.captured_on.is_some_and(|date| date <= to))
                }
        });
        let total = matching.clone().count();
        let (page, offset) = Page::<ItemView>::bounds(requested, total);
        Ok(Page {
            page,
            total,
            items: matching
                .skip(offset)
                .take(PAGE_SIZE)
                .map(|item| item.view(self.favorites.contains(&item.file.id)))
                .collect(),
        })
    }
}

fn session() -> Result<MutexGuard<'static, Option<Session>>, String> {
    SESSION.lock().map_err(|_| STATE_ERROR.into())
}

fn current(session: &Option<Session>, scan_id: u64) -> Result<&Session, String> {
    session
        .as_ref()
        .filter(|session| session.scan.id == scan_id)
        .ok_or_else(|| STALE_SCAN.into())
}

fn scan(scan_id: u64) -> Result<Arc<Scan>, String> {
    Ok(current(&*session()?, scan_id)?.scan.clone())
}

#[cfg(target_os = "linux")]
fn appimage_parent(path: &Path) -> Option<&Path> {
    if !path.is_absolute() || !path.is_file() {
        return None;
    }
    path.parent()
}

fn portable_directory() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("APPIMAGE")
            .as_deref()
            .and_then(|path| appimage_parent(Path::new(path)))
            .map(Path::to_path_buf)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn directory_preference(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|path| path.join("library-directory"))
        .map_err(|error| error.to_string())
}

fn read_preference(path: &Path) -> io::Result<Option<PathBuf>> {
    match fs::read_to_string(path) {
        Ok(directory) => {
            let directory = PathBuf::from(directory);
            Ok(directory.is_absolute().then_some(directory))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn save_preference(path: &Path, directory: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("缺少应用配置目录。"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(directory.as_bytes())?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

struct StartupLibrary {
    directory: String,
    root: Root,
    keys: HashSet<Vec<u8>>,
}

fn startup_candidate(directory: &Path) -> Result<Option<StartupLibrary>, String> {
    let root = match Root::new(directory) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "无法打开媒体库目录 {}。{error}",
                directory.display()
            ));
        }
    };
    let Some(keys) = favorites::read(&root)
        .map_err(|error| format!("无法读取 {} 的收藏数据库。{error}", directory.display()))?
    else {
        return Ok(None);
    };
    let directory = directory
        .to_str()
        .ok_or("媒体库目录的路径无法显示，请选择其他目录。")?
        .to_owned();
    Ok(Some(StartupLibrary {
        directory,
        root,
        keys,
    }))
}

fn find_startup_library(
    portable: Option<&Path>,
    preference: Result<&Path, String>,
) -> Result<Option<StartupLibrary>, String> {
    let portable_error = match portable
        .map(startup_candidate)
        .transpose()
        .map(Option::flatten)
    {
        Ok(Some(library)) => return Ok(Some(library)),
        Ok(None) => None,
        Err(error) => Some(error),
    };
    let remembered = preference
        .and_then(|path| read_preference(path).map_err(|error| error.to_string()))
        .map_err(|error| format!("无法读取上次选择的媒体库目录。{error}"))?;
    if let Some(directory) = remembered
        && let Some(library) = startup_candidate(&directory)?
    {
        return Ok(Some(library));
    }
    portable_error.map_or(Ok(None), Err)
}

fn open_library(
    root: Root,
    keys: HashSet<Vec<u8>>,
    progress: impl FnMut(Progress),
) -> Result<Summary, String> {
    let scanned = Session::from_scan(
        Scan::read(SCAN_ID.fetch_add(1, Ordering::Relaxed), root, progress),
        keys,
    );
    scanned
        .scan
        .root
        .verify_directory()
        .map_err(|error| error.to_string())?;
    let summary = Summary {
        scan_id: scanned.scan.id,
        total: scanned.scan.items.len(),
        issues: scanned.scan.issues.len(),
        preference_warning: None,
    };
    *session()? = Some(scanned);
    Ok(summary)
}

fn open_directory(
    directory: &str,
    mode: OpenMode,
    progress: impl FnMut(Progress),
) -> Result<Summary, String> {
    let root =
        Root::new(Path::new(directory)).map_err(|error| format!("无法打开媒体库目录。{error}"))?;
    let keys =
        match favorites::read(&root).map_err(|error| format!("无法读取收藏数据库。{error}"))?
        {
            Some(keys) => keys,
            None if matches!(mode, OpenMode::Select) => favorites::initialize(&root)
                .map_err(|error| format!("无法初始化媒体库。{error}"))?,
            None => return Err("找不到收藏数据库。请重新选择媒体库目录。".into()),
        };
    open_library(root, keys, progress)
}

#[tauri::command]
pub(crate) async fn scan_startup_library(
    app: AppHandle,
    progress: Channel<Progress>,
) -> Result<Option<OpenedLibrary>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let portable = portable_directory();
        let preference = directory_preference(&app);
        let Some(StartupLibrary {
            directory,
            root,
            keys,
        }) = find_startup_library(
            portable.as_deref(),
            preference.as_deref().map_err(String::clone),
        )?
        else {
            return Ok(None);
        };
        let summary = open_library(root, keys, |update| {
            let _ = progress.send(update);
        })?;
        Ok(Some(OpenedLibrary { directory, summary }))
    })
    .await
    .map_err(|error| format!("媒体库启动扫描异常结束。{error}"))?
}

#[tauri::command]
pub(crate) async fn scan_library(
    app: AppHandle,
    directory: String,
    mode: OpenMode,
    progress: Channel<Progress>,
) -> Result<Summary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let mut summary = open_directory(&directory, mode, |update| {
            let _ = progress.send(update);
        })?;
        if matches!(mode, OpenMode::Select)
            && let Err(error) = directory_preference(&app).and_then(|path| {
                save_preference(&path, &directory).map_err(|error| error.to_string())
            })
        {
            summary.preference_warning = Some(format!("无法记住本次选择的媒体库目录。{error}"));
        }
        Ok(summary)
    })
    .await
    .map_err(|error| format!("媒体库扫描异常结束。{error}"))?
}

#[tauri::command]
pub(crate) fn library_items(
    scan_id: u64,
    filter: Filter,
    page: usize,
) -> Result<Page<ItemView>, String> {
    current(&*session()?, scan_id)?.items(filter, page)
}

#[tauri::command]
pub(crate) fn library_issues(scan_id: u64, page: usize) -> Result<Page<Issue>, String> {
    let scan = scan(scan_id)?;
    let total = scan.issues.len();
    let (page, offset) = Page::<Issue>::bounds(page, total);
    Ok(Page {
        page,
        total,
        items: scan
            .issues
            .iter()
            .skip(offset)
            .take(PAGE_SIZE)
            .cloned()
            .collect(),
    })
}

#[tauri::command]
pub(crate) async fn library_preview(
    scan_id: u64,
    file_id: usize,
    detail: bool,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let scan = scan(scan_id)?;
        let item = scan.item(file_id)?;
        if !item.preview_available {
            return Err("此文件可用系统默认应用打开。".into());
        }
        crate::similar::preview(&scan.root, &item.file, detail)
    })
    .await
    .map_err(|error| format!("无法加载媒体预览。{error}"))?
}

#[tauri::command]
pub(crate) async fn set_library_favorite(
    scan_id: u64,
    file_id: usize,
    favorite: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let scan = scan(scan_id)?;
        let item = scan.item(file_id)?;
        scan.root
            .open_matching(&item.file)
            .map_err(|_| STALE_SCAN)?;
        favorites::write(&scan.root, &item.file.relative, favorite)
            .map_err(|error| format!("无法保存收藏。{error}"))?;
        let mut session = session()?;
        let session = session
            .as_mut()
            .filter(|session| session.scan.id == scan_id)
            .ok_or(STALE_SCAN)?;
        if favorite {
            session.favorites.insert(file_id);
        } else {
            session.favorites.remove(&file_id);
        }
        Ok(())
    })
    .await
    .map_err(|error| format!("收藏任务异常结束。{error}"))?
}

#[tauri::command]
pub(crate) async fn open_library_item(scan_id: u64, file_id: usize) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let scan = scan(scan_id)?;
        let item = scan.item(file_id)?;
        scan.root
            .open_matching(&item.file)
            .map_err(|_| STALE_SCAN)?;
        platform::open_default(&scan.root.path.join(&item.file.relative))
            .map_err(|error| format!("无法用系统默认应用打开。{error}"))
    })
    .await
    .map_err(|error| format!("无法打开文件。{error}"))?
}

#[cfg(test)]
mod tests;
