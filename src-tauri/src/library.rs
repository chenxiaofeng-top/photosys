pub(crate) mod ai_search;
pub(crate) mod albums;
pub(crate) mod details;
pub(crate) mod export;
mod favorites;
#[cfg(test)]
mod index_benchmark;
mod live;
pub(crate) mod locations;
pub(crate) mod people;
pub(crate) mod platform;
mod previews;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use nom_exif::MediaParser;
use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, Manager};
use tempfile::NamedTempFile;
use tokio::sync::Semaphore;
use walkdir::{DirEntry, WalkDir};

use crate::duplicates::{Candidate, FileStamp, Root, display_path, is_link};
use crate::media_date::{self, Media};
pub(crate) use previews::Cache as PreviewCache;

const PAGE_SIZE: usize = 24;
const MAX_THUMBNAIL_CONCURRENCY: usize = 3;
const STATE_ERROR: &str = "无法读取媒体库状态，请重启应用后重试。";
const STALE_SCAN: &str = "媒体库已变化，请刷新。";
static SCAN_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
static TEST_SESSION: Mutex<()> = Mutex::new(());
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static THUMBNAIL_CONCURRENCY: LazyLock<usize> =
    LazyLock::new(|| thumbnail_concurrency(thread::available_parallelism().map_or(1, usize::from)));
static THUMBNAIL_SLOTS: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(*THUMBNAIL_CONCURRENCY));
static DETAIL_SLOTS: Semaphore = Semaphore::const_new(1);
static LIVE_VIDEO_SLOTS: Semaphore = Semaphore::const_new(1);

fn thumbnail_concurrency(threads: usize) -> usize {
    (threads / 2).clamp(1, MAX_THUMBNAIL_CONCURRENCY)
}

pub(crate) fn include_in_scan(entry: &DirEntry) -> bool {
    entry.depth() == 0 || entry.file_name() != favorites::DATA_DIR
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Kind {
    Photo,
    Video,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Filter {
    #[serde(default)]
    query: String,
    kind: Option<Kind>,
    from: Option<String>,
    to: Option<String>,
    unknown_date: bool,
    favorites_only: bool,
    #[serde(default)]
    location_mode: locations::LocationMode,
    #[serde(default)]
    location_selection: Option<locations::Selection>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Sort {
    #[default]
    DateDesc,
    DateAsc,
    NameAsc,
    NameDesc,
    SizeDesc,
    SizeAsc,
}

impl Sort {
    fn compare(self, a: &Item, b: &Item) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let order = match self {
            Self::DateDesc | Self::DateAsc => match (a.captured_on, b.captured_on) {
                (Some(a), Some(b)) => {
                    if matches!(self, Self::DateAsc) {
                        a.cmp(&b)
                    } else {
                        b.cmp(&a)
                    }
                }
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (None, None) => Ordering::Equal,
            },
            Self::NameAsc => a.name_key.cmp(&b.name_key),
            Self::NameDesc => b.name_key.cmp(&a.name_key),
            Self::SizeDesc => b.file.stamp.size.cmp(&a.file.stamp.size),
            Self::SizeAsc => a.file.stamp.size.cmp(&b.file.stamp.size),
        };
        order.then_with(|| a.file.relative.cmp(&b.file.relative))
    }
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
    thumbnail_concurrency: usize,
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
    location: Option<locations::Location>,
    preview_available: bool,
    is_live_photo: bool,
    favorite: bool,
    rotation: u16,
}

struct Item {
    file: Candidate,
    search_path: String,
    name_key: String,
    kind: Kind,
    captured_on: Option<NaiveDate>,
    location: Option<locations::Location>,
    preview_available: bool,
    is_live_photo: bool,
}

impl Item {
    fn read(
        root: &Root,
        relative: PathBuf,
        media: Media,
        parser: &mut MediaParser,
        cache: &mut Option<locations::Cache>,
    ) -> std::io::Result<Self> {
        let file = root.open_file(&relative)?;
        let stamp = FileStamp::read(&file)?;
        let (metadata, location) = cache
            .as_ref()
            .and_then(|c| c.read(&relative, &stamp))
            .unwrap_or_else(|| {
                let metadata = media_date::read_metadata(parser, file, media);
                let location = metadata.coordinates.map(locations::Location::resolve);
                if let Some(cache) = cache.as_mut() {
                    cache.write(&relative, &stamp, &metadata, &location);
                }
                (metadata, location)
            });
        let captured_on = metadata
            .date
            .or_else(|| media_date::from_filename(&relative));
        let kind = if media == Media::Video {
            Kind::Video
        } else {
            Kind::Photo
        };
        let is_live_photo = media == Media::LivePhoto;
        let preview_available =
            kind == Kind::Video || is_live_photo || crate::similar::supports_preview(&relative);
        Ok(Self {
            search_path: display_path(&relative).to_lowercase(),
            name_key: display_path(Path::new(relative.file_name().expect("media file name")))
                .to_lowercase(),
            file: Candidate {
                id: 0,
                relative,
                stamp,
            },
            kind,
            captured_on,
            location,
            preview_available,
            is_live_photo,
        })
    }

    fn view(&self, favorite: bool, rotation: u16) -> ItemView {
        ItemView {
            id: self.file.id,
            name: display_path(Path::new(
                self.file.relative.file_name().expect("media file name"),
            )),
            relative_path: self.file.path_label(),
            size: self.file.stamp.size,
            kind: self.kind,
            captured_on: self.captured_on.map(|date| date.to_string()),
            location: self.location.clone(),
            preview_available: self.preview_available,
            is_live_photo: self.is_live_photo,
            favorite,
            rotation,
        }
    }
}

pub(crate) struct Scan {
    id: u64,
    pub(crate) root: Root,
    items: Vec<Item>,
    issues: Vec<Issue>,
    previews: Mutex<previews::Cache>,
    ai_decodes: crate::ai::indexing::DecodePool,
}

impl Scan {
    fn item(&self, id: usize) -> Result<&Item, String> {
        self.items.get(id).ok_or_else(|| STALE_SCAN.into())
    }

    fn item_path(&self, id: usize) -> Result<PathBuf, String> {
        let item = self.item(id)?;
        self.root
            .open_matching(&item.file)
            .map_err(|_| STALE_SCAN)?;
        Ok(self.root.path.join(&item.file.relative))
    }

    fn read(id: u64, root: Root, mut progress: impl FnMut(Progress)) -> Self {
        let mut items = Vec::new();
        let mut issues = Vec::new();
        let mut parser = MediaParser::new();
        let mut location_cache = locations::Cache::open(&root);
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
            match Item::read(
                &root,
                relative.to_path_buf(),
                media,
                &mut parser,
                &mut location_cache,
            ) {
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
            previews: Mutex::default(),
            ai_decodes: Default::default(),
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
    rotations: HashMap<usize, u16>,
    search_revision: u64,
}

impl Session {
    fn from_scan(scan: Scan, keys: HashSet<Vec<u8>>, rotations: HashMap<Vec<u8>, u16>) -> Self {
        let favorites = scan
            .items
            .iter()
            .filter(|item| keys.contains(&favorites::path_key(&item.file.relative)))
            .map(|item| item.file.id)
            .collect();
        let rotations = scan
            .items
            .iter()
            .filter(|item| item.kind == Kind::Photo && item.preview_available)
            .filter_map(|item| {
                rotations
                    .get(&favorites::path_key(&item.file.relative))
                    .filter(|rotation| matches!(rotation, 90 | 180 | 270))
                    .map(|rotation| (item.file.id, *rotation))
            })
            .collect();
        Self {
            scan: Arc::new(scan),
            favorites,
            rotations,
            search_revision: 0,
        }
    }

    fn set_rotation(&mut self, scan_id: u64, file_id: usize, rotation: u16) -> Result<u16, String> {
        if self.scan.id != scan_id {
            return Err(STALE_SCAN.into());
        }
        if !matches!(rotation, 0 | 90 | 180 | 270) {
            return Err("旋转角度必须为 0、90、180 或 270 度。".into());
        }
        let item = self.scan.item(file_id)?;
        if item.kind != Kind::Photo || !item.preview_available {
            return Err("仅支持旋转可预览的照片。".into());
        }
        self.scan
            .root
            .open_matching(&item.file)
            .map_err(|_| STALE_SCAN)?;
        favorites::write_rotation(&self.scan.root, &item.file.relative, rotation)
            .map_err(|error| format!("无法保存旋转方向。{error}"))?;
        if rotation == 0 {
            self.rotations.remove(&file_id);
        } else {
            self.rotations.insert(file_id, rotation);
        }
        self.search_revision += 1;
        Ok(rotation)
    }

    fn matching_items(&self, filter: &Filter) -> Result<Vec<&Item>, String> {
        filter.validate_location()?;
        let from = parse_date(filter.from.as_deref())?;
        let to = parse_date(filter.to.as_deref())?;
        if matches!((from, to), (Some(from), Some(to)) if from > to) {
            return Err("开始日期应早于或等于结束日期。".into());
        }
        let query = filter.query.trim().to_lowercase();
        let matching: Vec<_> = self
            .scan
            .items
            .iter()
            .filter(|item| {
                filter.matches_location(item)
                    && item.search_path.contains(&query)
                    && filter.kind.is_none_or(|kind| item.kind == kind)
                    && (!filter.favorites_only || self.favorites.contains(&item.file.id))
                    && if filter.unknown_date {
                        item.captured_on.is_none()
                    } else {
                        from.is_none_or(|from| item.captured_on >= Some(from))
                            && to.is_none_or(|to| item.captured_on.is_some_and(|date| date <= to))
                    }
            })
            .collect();
        Ok(matching)
    }

    fn items(
        &self,
        filter: Filter,
        requested: usize,
        sort: Sort,
    ) -> Result<Page<ItemView>, String> {
        let mut matching = self.matching_items(&filter)?;
        matching.sort_unstable_by(|a, b| sort.compare(a, b));
        let total = matching.len();
        let (page, offset) = Page::<ItemView>::bounds(requested, total);
        Ok(Page {
            page,
            total,
            items: matching
                .into_iter()
                .skip(offset)
                .take(PAGE_SIZE)
                .map(|item| {
                    item.view(
                        self.favorites.contains(&item.file.id),
                        self.rotations.get(&item.file.id).copied().unwrap_or(0),
                    )
                })
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

pub(crate) fn scan(scan_id: u64) -> Result<Arc<Scan>, String> {
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

pub(crate) fn directory_preference(app: &AppHandle) -> Result<PathBuf, String> {
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

pub(crate) fn save_preference(path: &Path, directory: &str) -> io::Result<()> {
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
    crate::database_backup::recover(&root)?;
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
    ai_search::cancel_all();
    people::cancel_all();
    let rotations =
        favorites::read_rotations(&root).map_err(|error| format!("无法读取旋转方向。{error}"))?;
    let scanned = Session::from_scan(
        Scan::read(SCAN_ID.fetch_add(1, Ordering::Relaxed), root, progress),
        keys,
        rotations,
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
        thumbnail_concurrency: *THUMBNAIL_CONCURRENCY,
        preference_warning: None,
    };
    *session()? = Some(scanned);
    Ok(summary)
}

pub(crate) fn open_directory(
    directory: &str,
    mode: OpenMode,
    progress: impl FnMut(Progress),
) -> Result<Summary, String> {
    ai_search::close_reader()?;
    let root =
        Root::new(Path::new(directory)).map_err(|error| format!("无法打开媒体库目录。{error}"))?;
    crate::database_backup::recover(&root)?;
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
        let _database = crate::database_access::maintenance()?;
        people::close_reader()?;
        ai_search::close_reader()?;
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
        let _database = crate::database_access::maintenance()?;
        people::close_reader()?;
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
    sort: Option<Sort>,
) -> Result<Page<ItemView>, String> {
    current(&*session()?, scan_id)?.items(filter, page, sort.unwrap_or_default())
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
    let slots = if detail {
        &DETAIL_SLOTS
    } else {
        &*THUMBNAIL_SLOTS
    };
    let permit = slots.acquire().await.map_err(|_| STATE_ERROR)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        scan(scan_id)?.preview(file_id, detail)
    })
    .await
    .map_err(|error| format!("无法加载媒体预览。{error}"))?
}

#[tauri::command]
pub(crate) async fn library_live_video(scan_id: u64, file_id: usize) -> Result<Response, String> {
    let permit = LIVE_VIDEO_SLOTS.acquire().await.map_err(|_| STATE_ERROR)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        let bytes = scan(scan_id)?.live_video(file_id)?;
        scan(scan_id)?;
        Ok(Response::new(bytes))
    })
    .await
    .map_err(|error| format!("无法加载实况视频。{error}"))?
}

#[tauri::command]
pub(crate) async fn set_library_rotation(
    scan_id: u64,
    file_id: usize,
    rotation: u16,
) -> Result<u16, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let _database = crate::database_access::read()?;
        let _edits = people::lock_edits()?;
        session()?
            .as_mut()
            .ok_or(STALE_SCAN)?
            .set_rotation(scan_id, file_id, rotation)
    })
    .await
    .map_err(|error| format!("旋转任务异常结束。{error}"))?
}

#[tauri::command]
pub(crate) async fn set_library_favorite(
    scan_id: u64,
    file_id: usize,
    favorite: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let _database = crate::database_access::read()?;
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
        session.search_revision += 1;
        Ok(())
    })
    .await
    .map_err(|error| format!("收藏任务异常结束。{error}"))?
}

#[tauri::command]
pub(crate) async fn open_library_item(scan_id: u64, file_id: usize) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let scan = scan(scan_id)?;
        platform::open_default(&scan.item_path(file_id)?)
            .map_err(|error| format!("无法用系统默认应用打开。{error}"))
    })
    .await
    .map_err(|error| format!("无法打开文件。{error}"))?
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RevealResult {
    Revealed,
    OpenedParent,
}

pub(crate) fn reveal_path(
    path: &Path,
    reveal: impl FnOnce(&Path) -> Result<(), String>,
    open: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<RevealResult, String> {
    match reveal(path) {
        Ok(()) => Ok(RevealResult::Revealed),
        Err(reveal_error) => {
            let parent = path.parent().ok_or("无法确定文件所在目录。")?;
            open(parent).map_err(|error| {
                format!("无法定位文件。{reveal_error}；无法打开所在目录。{error}")
            })?;
            Ok(RevealResult::OpenedParent)
        }
    }
}

#[tauri::command]
pub(crate) async fn reveal_library_item(
    scan_id: u64,
    file_id: usize,
) -> Result<RevealResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = scan(scan_id)?.item_path(file_id)?;
        reveal_path(&path, platform::reveal, |parent| {
            tauri_plugin_opener::open_path(parent, None::<&str>).map_err(|error| error.to_string())
        })
    })
    .await
    .map_err(|error| format!("无法定位文件。{error}"))?
}

pub(crate) fn clipboard_path(path: &Path) -> Result<String, String> {
    #[cfg(windows)]
    let path = crate::duplicates::shell_path(path)?;
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "此文件路径无法准确转换为文本，未复制。".into())
}

#[tauri::command]
pub(crate) async fn copy_library_item_path(
    app: AppHandle,
    scan_id: u64,
    file_id: usize,
) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    tauri::async_runtime::spawn_blocking(move || {
        let path = scan(scan_id)?.item_path(file_id)?;
        app.clipboard()
            .write_text(clipboard_path(&path)?)
            .map_err(|error| format!("无法复制完整路径。{error}"))
    })
    .await
    .map_err(|error| format!("无法复制完整路径。{error}"))?
}

#[cfg(test)]
mod tests;

#[tauri::command]
pub(crate) async fn library_item(scan_id: u64, file_id: usize) -> Result<ItemView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let s = session()?;
        let s = current(&s, scan_id)?;
        let item = s.scan.item(file_id)?;
        s.scan
            .root
            .open_matching(&item.file)
            .map_err(|_| STALE_SCAN)?;
        Ok(item.view(
            s.favorites.contains(&file_id),
            s.rotations.get(&file_id).copied().unwrap_or(0),
        ))
    })
    .await
    .map_err(crate::ai::error)?
}

/// Called under exclusive database access before replacing any database files.
pub(crate) fn invalidate() -> Result<(), String> {
    *session()? = None;
    ai_search::close_reader()?;
    people::close_reader()
}

// Shared renderer accepts a validated candidate from any session, never a library ID.
pub(crate) fn preview_candidate(
    root: &Root,
    candidate: &Candidate,
    detail: bool,
) -> Result<String, String> {
    previews::render_candidate(root, candidate, detail)
}
