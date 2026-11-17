mod archive;
mod media_date;

use std::path::Path;
use std::sync::{Mutex, TryLockError};

use archive::{ArchiveProgress, ArchiveSummary};
use tauri::WebviewWindow;
use tauri::ipc::Channel;
use tauri_plugin_dialog::DialogExt;

static ARCHIVE_TASK: Mutex<()> = Mutex::new(());

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
        let _task = ARCHIVE_TASK.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => "已有归档任务正在运行。".to_string(),
            TryLockError::Poisoned(_) => "归档任务异常结束，请重启应用后重试。".to_string(),
        })?;

        archive::run(Path::new(&source), Path::new(&destination), |update| {
            let _ = progress.send(update);
        })
    })
    .await
    .map_err(|error| format!("归档任务异常结束。{error}"))?
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![choose_folder, archive_media])
        .run(tauri::generate_context!())
        .expect("could not start PhotoSys");
}
