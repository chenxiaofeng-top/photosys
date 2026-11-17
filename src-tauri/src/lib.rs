mod archive;
mod duplicates;
mod library;
mod media_codec;
mod media_date;
mod similar;

use std::path::Path;
use std::sync::{Mutex, MutexGuard, TryLockError};

use archive::{ArchiveProgress, ArchiveSummary};
use duplicates::{Scan, ScanProgress, ScanResult, TrashResult};
use tauri::WebviewWindow;
use tauri::ipc::Channel;
use tauri_plugin_dialog::DialogExt;

static FILE_TASK: Mutex<()> = Mutex::new(());
static DUPLICATE_SESSION: Mutex<DuplicateSession> = Mutex::new(DuplicateSession {
    scan_id: 0,
    current: None,
});

struct DuplicateSession {
    scan_id: u64,
    current: Option<Scan>,
}

fn file_task() -> Result<MutexGuard<'static, ()>, String> {
    FILE_TASK.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => "已有文件任务正在运行。".into(),
        TryLockError::Poisoned(_) => "文件任务异常结束，请重启应用后重试。".into(),
    })
}

#[tauri::command]
async fn choose_folder(window: WebviewWindow, title: String) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title(title)
            .blocking_pick_folder()
            .map(|folder| {
                folder
                    .into_path()
                    .map_err(|error| error.to_string())?
                    .into_os_string()
                    .into_string()
                    .map_err(|_| "所选目录的路径无法显示，请选择其他目录。".to_string())
            })
            .transpose()
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn archive_media(
    source: String,
    destination: String,
    progress: Channel<ArchiveProgress>,
) -> Result<ArchiveSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = file_task()?;

        archive::run(Path::new(&source), Path::new(&destination), |update| {
            let _ = progress.send(update);
        })
    })
    .await
    .map_err(|error| format!("归档任务异常结束。{error}"))?
}

#[tauri::command]
async fn scan_duplicates(
    directory: String,
    progress: Channel<ScanProgress>,
) -> Result<ScanResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = file_task()?;
        let mut session = DUPLICATE_SESSION
            .lock()
            .map_err(|_| "查重任务异常结束，请重启应用后重试。")?;
        session.scan_id += 1;
        session.current = None;
        let (scan, result) = duplicates::scan(session.scan_id, Path::new(&directory), |update| {
            let _ = progress.send(update);
        })?;
        session.current = Some(scan);
        Ok(result)
    })
    .await
    .map_err(|error| format!("查重任务异常结束。{error}"))?
}

#[tauri::command]
async fn trash_duplicates(scan_id: u64, file_ids: Vec<usize>) -> Result<TrashResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = file_task()?;
        let mut session = DUPLICATE_SESSION
            .lock()
            .map_err(|_| "查重任务异常结束，请重启应用后重试。")?;
        let scan = session.current.as_mut().ok_or("请先扫描重复文件。")?;
        scan.recycle(scan_id, file_ids, duplicates::trash)
    })
    .await
    .map_err(|error| format!("清理任务异常结束。{error}"))?
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            choose_folder,
            library::scan_startup_library,
            library::scan_library,
            library::library_items,
            library::library_issues,
            library::library_preview,
            library::library_live_video,
            library::set_library_favorite,
            library::set_library_rotation,
            library::open_library_item,
            library::reveal_library_item,
            library::copy_library_item_path,
            archive_media,
            scan_duplicates,
            trash_duplicates,
            similar::scan_similar,
            similar::cancel_similar,
            similar::similar_groups,
            similar::similar_members,
            similar::similar_issues,
            similar::similar_preview
        ])
        .run(tauri::generate_context!())
        .expect("could not start PhotoSys");
}
