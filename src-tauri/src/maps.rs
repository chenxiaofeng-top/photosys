//! Managed offline PMTiles archives. Only explicit downloads access the network.
use crate::duplicates::is_link;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{
    AppHandle, Manager,
    ipc::{Channel, Response},
};
use tauri_plugin_dialog::DialogExt;
use tempfile::NamedTempFile;

const VERSION: &str = "1.31.2";
const CANCELLED: &str = "地图包任务已取消，可以重试。";
static TASK: Mutex<()> = Mutex::new(());
static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Package {
    id: String,
    name: String,
    bounds: [f64; 4],
    min_zoom: u8,
    max_zoom: u8,
    size: u64,
    installed_at: u64,
    source: String,
    version: String,
    #[serde(default)]
    warning: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct State {
    directory: String,
    packages: Vec<Package>,
    interrupted: bool,
    interrupted_download: Option<DownloadJob>,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Progress {
    phase: String,
    bytes: u64,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct DownloadJob {
    name: String,
    bounds: [f64; 4],
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(error)?
        .join("maps-directory"))
}
fn directory(app: &AppHandle) -> Result<PathBuf, String> {
    let config = config_path(app)?;
    let path = match fs::read_to_string(config) {
        Ok(p) => PathBuf::from(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            app.path().app_data_dir().map_err(error)?.join("maps")
        }
        Err(e) => return Err(error(e)),
    };
    if !path.is_absolute() {
        return Err("地图目录必须是绝对路径。".into());
    }
    fs::create_dir_all(&path).map_err(|e| format!("无法打开地图目录。{e}"))?;
    let m = fs::symlink_metadata(&path).map_err(error)?;
    if !m.is_dir() || is_link(&m) {
        return Err("地图目录必须是普通目录。".into());
    }
    Ok(path)
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut temporary = NamedTempFile::new_in(path.parent().ok_or("缺少目录。")?).map_err(error)?;
    serde_json::to_writer(temporary.as_file_mut(), value).map_err(error)?;
    temporary.as_file_mut().sync_all().map_err(error)?;
    temporary.persist(path).map_err(error)?;
    Ok(())
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit())
}
fn file(path: &Path) -> Result<File, String> {
    let m = fs::symlink_metadata(path).map_err(error)?;
    if !m.is_file() || is_link(&m) {
        return Err("地图包必须是普通文件。".into());
    }
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(0x00200000);
    }
    let f = o.open(path).map_err(error)?;
    let m = f.metadata().map_err(error)?;
    if !m.is_file() || is_link(&m) {
        return Err("地图包必须是普通文件。".into());
    }
    Ok(f)
}
fn list(dir: &Path) -> Result<Vec<Package>, String> {
    let mut packages = Vec::new();
    for e in fs::read_dir(dir).map_err(error)? {
        let path = e.map_err(error)?.path();
        let Some(id) = path.file_stem().and_then(|v| v.to_str()) else {
            continue;
        };
        if path.extension().is_none_or(|e| e != "json") || !valid_id(id) {
            continue;
        }
        let parsed =
            file(&path).and_then(|f| serde_json::from_reader::<_, Package>(f).map_err(error));
        let mut p = match parsed {
            Ok(p) if p.id == id => p,
            _ => Package {
                id: id.into(),
                name: format!("损坏的地图包 {id}"),
                bounds: [0.; 4],
                min_zoom: 0,
                max_zoom: 0,
                size: 0,
                installed_at: 0,
                source: String::new(),
                version: String::new(),
                warning: Some("地图包记录损坏，请删除后重新导入。".into()),
            },
        };
        if p.warning.is_none()
            && file(&dir.join(format!("{id}.pmtiles")))
                .and_then(|f| f.metadata().map_err(error))
                .is_ok_and(|m| m.len() == p.size)
                == false
        {
            p.warning = Some("地图包已丢失或变化，请删除后重新导入。".into());
        }
        packages.push(p);
    }
    packages.sort_by(|a, b| b.installed_at.cmp(&a.installed_at));
    Ok(packages)
}
fn state(app: &AppHandle) -> Result<State, String> {
    let dir = directory(app)?;
    Ok(State {
        directory: dir.to_string_lossy().into_owned(),
        packages: list(&dir)?,
        interrupted: dir.join("download-job.json").exists(),
        interrupted_download: file(&dir.join("download-job.json"))
            .ok()
            .and_then(|f| serde_json::from_reader(f.take(65536)).ok()),
    })
}
fn executable() -> Result<PathBuf, String> {
    let name = if cfg!(windows) {
        "photosys-pmtiles.exe"
    } else {
        "photosys-pmtiles"
    };
    let exe = std::env::current_exe().map_err(error)?;
    #[cfg(test)]
    if let Some(profile) = exe
        .ancestors()
        .find(|p| p.ends_with("deps") || p.ends_with("build"))
        .and_then(Path::parent)
    {
        return Ok(profile.join(name));
    }
    Ok(exe.with_file_name(name))
}
fn command() -> Result<Command, String> {
    let mut command = Command::new(executable()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.stdin(Stdio::null());
    Ok(command)
}
fn check_cancel() -> Result<(), String> {
    if CANCEL.load(Ordering::Relaxed) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
fn run(mut command: Command, timeout: Duration, mut tick: impl FnMut()) -> Result<(), String> {
    let log = tempfile::tempfile().map_err(error)?;
    command
        .stdout(Stdio::from(log.try_clone().map_err(error)?))
        .stderr(Stdio::from(log.try_clone().map_err(error)?));
    let mut child = command
        .spawn()
        .map_err(|e| format!("无法启动离线地图工具 {VERSION}。{e}"))?;
    let start = Instant::now();
    loop {
        if let Err(e) = check_cancel() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        match child.try_wait() {
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error(e));
            }
            Ok(result) => match result {
                Some(status) if status.success() => return Ok(()),
                Some(_) => {
                    let mut log = log;
                    let size = log.metadata().map_or(0, |m| m.len());
                    let _ = log.seek(SeekFrom::Start(size.saturating_sub(4096)));
                    let mut tail = Vec::new();
                    let _ = log.read_to_end(&mut tail);
                    let text = String::from_utf8_lossy(&tail);
                    return Err(format!(
                        "地图包处理失败，请检查网络、源数据和磁盘空间。{}",
                        text.trim()
                    ));
                }
                None => {}
            },
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err("地图包处理超时，可以重试。".into());
        }
        tick();
        thread::sleep(Duration::from_millis(200));
    }
}
fn read_u64(h: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(h[offset..offset + 8].try_into().unwrap())
}
fn read_coord(h: &[u8], offset: usize) -> f64 {
    i32::from_le_bytes(h[offset..offset + 4].try_into().unwrap()) as f64 / 1e7
}
fn validate(path: &Path) -> Result<Package, String> {
    let mut input = file(path)?;
    let size = input.metadata().map_err(error)?.len();
    let mut h = [0u8; 127];
    input.read_exact(&mut h).map_err(|_| "地图包头不完整。")?;
    if &h[..7] != b"PMTiles"
        || h[7] != 3
        || h[99] != 1
        || ![1, 2].contains(&h[97])
        || ![1, 2].contains(&h[98])
    {
        return Err("仅支持 PMTiles v3、无压缩或 gzip 的 Protomaps 矢量地图包。".into());
    }
    for offset in [8, 24, 40, 56] {
        let start = read_u64(&h, offset);
        let len = read_u64(&h, offset + 8);
        if (len > 0 && start < 127) || start.checked_add(len).is_none_or(|end| end > size) {
            return Err("地图包数据范围损坏。".into());
        }
    }
    let metadata_size = read_u64(&h, 32);
    if metadata_size > 8 * 1024 * 1024 {
        return Err("地图包元数据过大。".into());
    }
    input
        .seek(SeekFrom::Start(read_u64(&h, 24)))
        .map_err(error)?;
    let mut bytes = vec![0; metadata_size as usize];
    input.read_exact(&mut bytes).map_err(error)?;
    if h[97] == 2 {
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(&bytes[..])
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut decoded)
            .map_err(error)?;
        if decoded.len() > 8 * 1024 * 1024 {
            return Err("地图包元数据解压后过大。".into());
        }
        bytes = decoded;
    }
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "地图包元数据损坏。")?;
    let layers = json["vector_layers"]
        .as_array()
        .ok_or("地图包缺少 Protomaps 图层说明。")?;
    for expected in ["earth", "water", "roads", "places"] {
        if !layers.iter().any(|l| l["id"].as_str() == Some(expected)) {
            return Err(format!(
                "地图包缺少 {expected} 图层，不兼容 Protomaps 底图。"
            ));
        }
    }
    let version = json.get("version").and_then(|v| v.as_str()).unwrap_or("");
    // Basemap style v5 supports tileset schema v4 only.
    if !version.starts_with("4.") {
        return Err("地图包需要 Protomaps v4 图层结构。".into());
    }
    let bounds = [
        read_coord(&h, 102),
        read_coord(&h, 106),
        read_coord(&h, 110),
        read_coord(&h, 114),
    ];
    validate_bounds(bounds)?;
    if h[100] > h[101] || h[101] > 22 || read_u64(&h, 72) == 0 {
        return Err("地图包缩放级别或瓦片数量无效。".into());
    }
    let mut verify = command()?;
    verify.arg("verify").arg(path);
    run(verify, Duration::from_secs(300), || {})?;
    Ok(Package {
        id: String::new(),
        name: json["name"].as_str().unwrap_or("离线地图").to_owned(),
        bounds,
        min_zoom: h[100],
        max_zoom: h[101],
        size,
        installed_at: 0,
        source: String::new(),
        version: version.to_owned(),
        warning: None,
    })
}
fn validate_bounds([w, s, e, n]: [f64; 4]) -> Result<(), String> {
    if [w, s, e, n].iter().any(|v| !v.is_finite())
        || w >= e
        || s >= n
        || w < -180.
        || e > 180.
        || s < -85.051129
        || n > 85.051129
    {
        return Err("下载区域无效，请在地图上选择不跨越日期变更线的矩形区域。".into());
    }
    Ok(())
}
fn publish(
    dir: &Path,
    temporary: NamedTempFile,
    mut package: Package,
    name: String,
    source: String,
) -> Result<Package, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(error)?
        .as_nanos();
    package.id = format!("{nanos:032x}");
    package.installed_at = (nanos / 1_000_000) as u64;
    package.name = name.trim().chars().take(120).collect();
    if package.name.is_empty() {
        package.name = "离线地图".into();
    }
    package.source = source;
    let path = dir.join(format!("{}.pmtiles", package.id));
    temporary.as_file().sync_all().map_err(error)?;
    temporary.persist_noclobber(&path).map_err(error)?;
    if let Err(e) = atomic_json(&dir.join(format!("{}.json", package.id)), &package) {
        let _ = fs::remove_file(path);
        return Err(e);
    }
    Ok(package)
}
#[tauri::command]
pub(crate) async fn offline_map_state(app: AppHandle) -> Result<State, String> {
    tauri::async_runtime::spawn_blocking(move || state(&app))
        .await
        .map_err(error)?
}
#[tauri::command]
pub(crate) async fn offline_map_choose_directory(
    app: AppHandle,
    window: tauri::WebviewWindow,
) -> Result<Option<State>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = TASK.try_lock().map_err(|_| "地图包任务正在运行。")?;
        let Some(chosen) = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("选择离线地图保存目录")
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let chosen = chosen.into_path().map_err(error)?;
        let m = fs::symlink_metadata(&chosen).map_err(error)?;
        if !m.is_dir() || is_link(&m) {
            return Err("请选择普通目录。".into());
        }
        let path = config_path(&app)?;
        fs::create_dir_all(path.parent().unwrap()).map_err(error)?;
        let mut temporary = NamedTempFile::new_in(path.parent().unwrap()).map_err(error)?;
        temporary
            .write_all(chosen.to_str().ok_or("目录路径无法显示。")?.as_bytes())
            .map_err(error)?;
        temporary.persist(path).map_err(error)?;
        state(&app).map(Some)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn offline_map_import(
    app: AppHandle,
    window: tauri::WebviewWindow,
    progress: Channel<Progress>,
) -> Result<Option<Package>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = TASK.try_lock().map_err(|_| "地图包任务正在运行。")?;
        CANCEL.store(false, Ordering::Relaxed);
        let Some(chosen) = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("导入 Protomaps v4 PMTiles 地图包")
            .add_filter("PMTiles", &["pmtiles"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = chosen.into_path().map_err(error)?;
        let dir = directory(&app)?;
        let mut source = file(&path)?;
        let mut temporary = NamedTempFile::new_in(&dir).map_err(error)?;
        let mut bytes = vec![0; 1024 * 1024];
        let mut copied = 0;
        loop {
            check_cancel()?;
            let n = source.read(&mut bytes).map_err(error)?;
            if n == 0 {
                break;
            }
            temporary
                .write_all(&bytes[..n])
                .map_err(|e| format!("无法写入地图包，请检查磁盘空间。{e}"))?;
            copied += n as u64;
            let _ = progress.send(Progress {
                phase: "正在复制地图包".into(),
                bytes: copied,
            });
        }
        let _ = progress.send(Progress {
            phase: "正在验证地图包".into(),
            bytes: copied,
        });
        let p = validate(temporary.path())?;
        check_cancel()?;
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        publish(&dir, temporary, p, name, "本地导入".into()).map(Some)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn offline_map_download(
    app: AppHandle,
    name: String,
    bounds: [f64; 4],
    progress: Channel<Progress>,
) -> Result<Package, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = TASK.try_lock().map_err(|_| "地图包任务正在运行。")?;
        CANCEL.store(false, Ordering::Relaxed);
        validate_bounds(bounds)?;
        let dir = directory(&app)?;
        atomic_json(
            &dir.join("download-job.json"),
            &DownloadJob {
                name: name.clone(),
                bounds,
            },
        )?;
        let _ = progress.send(Progress {
            phase: "正在查找兼容地图版本".into(),
            bytes: 0,
        });
        let client = reqwest::Client::builder()
            .https_only(true)
            .user_agent("PhotoSys/0.1 offline-maps")
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(error)?;
        let bytes = tauri::async_runtime::block_on(async {
            let response = client
                .get("https://build-metadata.protomaps.dev/builds.json")
                .send()
                .await
                .map_err(error)?
                .error_for_status()
                .map_err(error)?;
            if response
                .content_length()
                .is_some_and(|len| len > 4 * 1024 * 1024)
            {
                return Err("地图版本目录过大。".to_owned());
            }
            let data = response.bytes().await.map_err(error)?;
            if data.len() > 4 * 1024 * 1024 {
                return Err("地图版本目录过大。".into());
            }
            Ok(data)
        })?;
        let builds: Vec<serde_json::Value> = serde_json::from_slice(&bytes).map_err(error)?;
        let key = builds
            .iter()
            .filter(|b| b["version"].as_str().is_some_and(|v| v.starts_with("4.")))
            .filter_map(|b| b["key"].as_str())
            .filter(|k| {
                k.ends_with(".pmtiles")
                    && k.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-' || c == b'_')
            })
            .max()
            .ok_or("暂时找不到兼容的地图版本，请稍后重试或导入本地地图包。")?;
        check_cancel()?;
        let url = format!("https://build.protomaps.com/{key}");
        let temporary = NamedTempFile::new_in(&dir).map_err(error)?;
        let mut command = command()?;
        command
            .arg("extract")
            .arg(&url)
            .arg(temporary.path())
            .arg(format!(
                "--bbox={},{},{},{}",
                bounds[0], bounds[1], bounds[2], bounds[3]
            ))
            .args(["--maxzoom=15", "--download-threads=2"]);
        run(command, Duration::from_secs(4 * 60 * 60), || {
            let bytes = fs::metadata(temporary.path()).map_or(0, |m| m.len());
            let _ = progress.send(Progress {
                phase: "正在下载区域地图".into(),
                bytes,
            });
        })?;
        let _ = progress.send(Progress {
            phase: "正在验证地图包".into(),
            bytes: 0,
        });
        let mut p = validate(temporary.path())?;
        p.bounds = bounds;
        check_cancel()?;
        let p = publish(&dir, temporary, p, name, url)?;
        let _ = fs::remove_file(dir.join("download-job.json"));
        Ok(p)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) fn offline_map_cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}
#[tauri::command]
pub(crate) async fn offline_map_delete(app: AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = TASK.try_lock().map_err(|_| "地图包任务正在运行。")?;
        if !valid_id(&id) {
            return Err("地图包标识无效。".into());
        }
        let dir = directory(&app)?;
        if !list(&dir)?.iter().any(|p| p.id == id) {
            return Err("地图包不存在。".into());
        }
        match fs::remove_file(dir.join(format!("{id}.pmtiles"))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(e)),
        }
        fs::remove_file(dir.join(format!("{id}.json"))).map_err(error)?;
        Ok(())
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn offline_map_read(
    app: AppHandle,
    id: String,
    offset: u64,
    length: u64,
) -> Result<Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !valid_id(&id) || length > 8 * 1024 * 1024 {
            return Err("地图读取请求无效。".into());
        }
        let dir = directory(&app)?;
        let record: Package =
            serde_json::from_reader(file(&dir.join(format!("{id}.json")))?).map_err(error)?;
        if record.id != id {
            return Err("地图包标识无效。".into());
        }
        let mut f = file(&dir.join(format!("{id}.pmtiles")))?;
        let size = f.metadata().map_err(error)?.len();
        if size != record.size || offset > size || offset.checked_add(length).is_none() {
            return Err("地图包已变化或请求越界，请重新导入。".into());
        }
        f.seek(SeekFrom::Start(offset)).map_err(error)?;
        let mut bytes = vec![0; length.min(size - offset) as usize];
        f.read_exact(&mut bytes).map_err(error)?;
        Ok(Response::new(bytes))
    })
    .await
    .map_err(error)?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn archive() -> Vec<u8> {
        let metadata=serde_json::to_vec(&serde_json::json!({"version":"4.15.2","name":"测试地图","vector_layers":[{"id":"earth"},{"id":"water"},{"id":"roads"},{"id":"places"}]})).unwrap();
        let mut tile = Vec::new();
        for name in ["earth", "water", "roads", "places"] {
            let mut layer = vec![10, name.len() as u8];
            layer.extend(name.as_bytes());
            layer.extend([40, 128, 32, 120, 2]);
            tile.extend([26, layer.len() as u8]);
            tile.extend(layer);
        }
        let root = vec![1, 0, 1, tile.len() as u8, 1];
        let mut h = vec![0; 127];
        h[..7].copy_from_slice(b"PMTiles");
        h[7] = 3;
        for (offset, value) in [
            (8, 127),
            (16, root.len() as u64),
            (24, 127 + root.len() as u64),
            (32, metadata.len() as u64),
            (40, 127 + root.len() as u64 + metadata.len() as u64),
            (56, 127 + root.len() as u64 + metadata.len() as u64),
            (64, tile.len() as u64),
            (72, 1),
            (80, 1),
            (88, 1),
        ] {
            h[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        h[96] = 1;
        h[97] = 1;
        h[98] = 1;
        h[99] = 1;
        for (offset, value) in [
            (102, -1800000000i32),
            (106, -850000000),
            (110, 1800000000),
            (114, 850000000),
        ] {
            h[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        h.extend(root);
        h.extend(metadata);
        h.extend(tile);
        h
    }
    #[test]
    fn validates_and_publishes_archives_without_exposing_partial_or_invalid_packages() {
        let _guard = TASK.lock().unwrap();
        CANCEL.store(false, Ordering::Relaxed);
        let temp = tempfile::tempdir().unwrap();
        let mut partial = NamedTempFile::new_in(temp.path()).unwrap();
        partial.write_all(&archive()).unwrap();
        assert!(list(temp.path()).unwrap().is_empty());
        let record = validate(partial.path()).unwrap();
        let installed = publish(
            temp.path(),
            partial,
            record,
            "北京".into(),
            "本地导入".into(),
        )
        .unwrap();
        assert!(valid_id(&installed.id));
        let published = temp.path().join(format!("{}.pmtiles", installed.id));
        let before = fs::read(&published).unwrap();
        let bad = temp.path().join("broken.pmtiles");
        fs::write(&bad, b"broken").unwrap();
        assert!(validate(&bad).is_err());
        assert_eq!(fs::read(&published).unwrap(), before);
        assert_eq!(list(temp.path()).unwrap().len(), 1);
        fs::remove_file(published).unwrap();
        let packages = list(temp.path()).unwrap();
        assert!(packages[0].warning.is_some());
        assert!(
            list(temp.path())
                .unwrap()
                .iter()
                .all(|p| p.id == installed.id)
        );
    }
    #[test]
    fn rejects_wrong_schemas_ranges_and_cancellation() {
        let _guard = TASK.lock().unwrap();
        CANCEL.store(false, Ordering::Relaxed);
        let temp = tempfile::tempdir().unwrap();
        let p = temp.path().join("map.pmtiles");
        for offset in [7, 97, 98, 99] {
            let mut bytes = archive();
            bytes[offset] = 0;
            fs::write(&p, bytes).unwrap();
            assert!(validate(&p).is_err());
        }
        let mut bytes = archive();
        bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&p, bytes).unwrap();
        assert!(validate(&p).is_err());
        assert!(validate_bounds([170., 0., -170., 10.]).is_err());
        assert!(validate_bounds([0., 0., 1., f64::NAN]).is_err());
        assert!(!valid_id("../not-a-map"));
        fs::write(&p, archive()).unwrap();
        CANCEL.store(true, Ordering::Relaxed);
        assert!(validate(&p).unwrap_err().contains("取消"));
        CANCEL.store(false, Ordering::Relaxed);
    }
    #[cfg(unix)]
    #[test]
    fn refuses_map_symlinks_and_preserves_the_target() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"preserved").unwrap();
        let link = temp.path().join("map.pmtiles");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(file(&link).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"preserved");
    }
}

#[cfg(test)]
mod real_archive_tests {
    use super::*;
    #[test]
    #[ignore = "requires PHOTOSYS_MAP_ARCHIVE pointing to a real offline Protomaps v4 extract"]
    fn real_archive_validates_and_survives_reopening_offline() {
        let _guard = TASK.lock().unwrap();
        CANCEL.store(false, Ordering::Relaxed);
        let source = PathBuf::from(std::env::var_os("PHOTOSYS_MAP_ARCHIVE").unwrap());
        let record = validate(&source).unwrap();
        assert!(record.size > 127);
        assert!(record.max_zoom >= 15);
        let temp = tempfile::tempdir().unwrap();
        let mut partial = NamedTempFile::new_in(temp.path()).unwrap();
        std::io::copy(&mut file(&source).unwrap(), partial.as_file_mut()).unwrap();
        let installed = publish(
            temp.path(),
            partial,
            record,
            "北京离线测试".into(),
            "offline test".into(),
        )
        .unwrap();
        assert_eq!(list(temp.path()).unwrap()[0].id, installed.id);
        assert!(list(temp.path()).unwrap()[0].warning.is_none());
        assert_eq!(
            validate(&temp.path().join(format!("{}.pmtiles", installed.id)))
                .unwrap()
                .version,
            installed.version
        );
    }
}
