mod ai;
mod archive;
mod database_access;
mod database_backup;
mod duplicates;
mod library;
mod maps;
mod media_codec;
mod media_date;
mod similar;

use std::path::Path;
use std::sync::{Mutex, MutexGuard, TryLockError};

use archive::{ArchiveProgress, ArchiveSummary};

use tauri::WebviewWindow;
use tauri::ipc::Channel;
use tauri_plugin_dialog::DialogExt;

static FILE_TASK: Mutex<()> = Mutex::new(());
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            ai::initialize_choices(app.handle()).map_err(std::io::Error::other)?;
            ai::device::initialize_settings(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            choose_folder,
            database_backup::database_backup_file,
            database_backup::database_export,
            database_backup::database_inspect,
            database_backup::database_restore,
            library::locations::library_map_points,
            library::locations::location_regions,
            maps::offline_map_state,
            maps::offline_map_choose_directory,
            maps::offline_map_import,
            maps::offline_map_download,
            maps::offline_map_cancel,
            maps::offline_map_delete,
            maps::offline_map_read,
            ai::catalog::ai_model_catalog,
            ai::device::ai_inference_status,
            ai::device::ai_set_inference_preference,
            ai::ai_activate_model,
            ai::ai_model_change_impact,
            ai::ai_open_model_source,
            ai::ai_model_status,
            ai::ai_set_model_directory,
            ai::ai_install_model,
            ai::ai_cancel_install,
            ai::ai_choose_model_package,
            library::ai_search::ai_index_status,
            library::ai_search::ai_update_index,
            library::ai_search::ai_cancel_index,
            library::ai_search::ai_index_issues,
            library::ai_search::ai_search,
            library::people::people_index_status,
            library::people::people_update_index,
            library::people::people_save_grouping,
            library::people::people_regroup_pending,
            library::people::people_cancel_index,
            library::people::people_index_issues,
            library::people::people_list,
            library::people::people_faces,
            library::people::people_face_suggestions,
            library::people::people_photos,
            library::people::people_photo_people,
            library::people::people_months,
            library::people::people_edit,
            library::people::people_undo,
            library::library_item,
            library::scan_startup_library,
            library::scan_library,
            library::library_items,
            library::albums::albums_list,
            library::albums::album_items,
            library::albums::albums_edit,
            library::library_issues,
            library::details::library_media_details,
            library::library_preview,
            library::library_live_video,
            library::set_library_favorite,
            library::set_library_rotation,
            library::open_library_item,
            library::reveal_library_item,
            library::copy_library_item_path,
            archive_media,
            duplicates::session::scan_duplicates,
            duplicates::session::cancel_duplicates,
            duplicates::session::duplicate_groups,
            duplicates::session::duplicate_members,
            duplicates::session::duplicate_selection,
            duplicates::session::duplicate_confirmation,
            duplicates::session::duplicate_issues,
            duplicates::session::duplicate_outcomes,
            duplicates::session::duplicate_preview,
            duplicates::session::reveal_duplicate,
            duplicates::session::copy_duplicate_path,
            duplicates::session::trash_duplicates,
            similar::scan_similar,
            similar::cancel_similar,
            similar::trash_similar,
            similar::similar_groups,
            similar::similar_members,
            similar::similar_issues,
            similar::similar_media_details,
            similar::similar_preview
        ])
        .run(tauri::generate_context!())
        .expect("could not start PhotoSys");
}
