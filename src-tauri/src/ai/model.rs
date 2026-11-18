use std::fs;
#[cfg(test)]
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, ipc::Channel};
use tauri_plugin_dialog::DialogExt;

use super::{
    download, error,
    inference::{self, Runtime},
};

pub(crate) static PEOPLE_GENERATION: AtomicU64 = AtomicU64::new(0);
pub(crate) static MODEL_GENERATION: AtomicU64 = AtomicU64::new(0);
static TASK: Mutex<()> = Mutex::new(());
static CANCEL: AtomicBool = AtomicBool::new(false);
static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);

struct Loaded {
    directory: PathBuf,
    stamps: Vec<(u64, std::time::SystemTime)>,
    runtime: Arc<Runtime>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(crate) struct Asset {
    pub name: &'static str,
    pub path: &'static str,
    pub size: u64,
    pub sha: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
}

#[cfg(test)]
static ASSETS: std::sync::LazyLock<Vec<Asset>> = std::sync::LazyLock::new(|| {
    super::catalog::default_model(ModelKind::Search)
        .assets
        .clone()
});

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ModelKind {
    #[default]
    Search,
    People,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Source {
    Mirror,
    Official,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelStatus {
    pub(crate) directory: Option<String>,
    pub(crate) ready: bool,
    pub(crate) total_bytes: u64,
    pub(crate) resumable_bytes: u64,
    pub(crate) message: Option<String>,
    pub(crate) model_id: &'static str,
    pub(crate) active_model_id: &'static str,
    pub(crate) active_ready: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallProgress {
    pub(super) phase: &'static str,
    pub(super) file: String,
    pub(super) downloaded: u64,
    pub(super) total: u64,
    pub(super) source: Source,
}

pub(super) fn config(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(error)?
        .join("ai-model-directory"))
}

pub(crate) fn selected_directory(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    match fs::read_to_string(config(app)?) {
        Ok(path) => Ok(Some(PathBuf::from(path))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("无法读取模型目录设置。{e}")),
    }
}

pub(super) fn regular_directory(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("模型目录不可用，请重新选择。{e}"))?;
    if !metadata.is_dir() || crate::duplicates::is_link(&metadata) {
        return Err("模型目录必须是普通目录。".into());
    }
    Ok(())
}

pub(super) fn regular_file(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(error)?;
    if !metadata.is_file() || crate::duplicates::is_link(&metadata) {
        return Err("模型文件必须是普通文件。".into());
    }
    Ok(metadata)
}

#[cfg(test)]
pub(crate) fn installed_directory(base: &Path) -> PathBuf {
    base.join(super::MODEL_ID)
}

#[cfg(test)]
fn staging_directory(base: &Path) -> PathBuf {
    base.join(format!(".{}.partial", super::MODEL_ID))
}

#[cfg(test)]
pub(crate) fn verify_assets(directory: &Path, cancel: &AtomicBool) -> Result<(), String> {
    regular_directory(directory)?;
    for asset in &*ASSETS {
        let path = directory.join(asset.name);
        if regular_file(&path)?.len() != asset.size || download::digest(&path, cancel)? != asset.sha
        {
            return Err(format!("{} 校验失败，请重新下载或导入。", asset.name));
        }
    }
    Ok(())
}

#[cfg(test)]
fn stamps(directory: &Path) -> Result<Vec<(u64, std::time::SystemTime)>, String> {
    ASSETS
        .iter()
        .map(|a| {
            let metadata = regular_file(&directory.join(a.name))?;
            Ok((metadata.len(), metadata.modified().map_err(error)?))
        })
        .collect()
}

pub(crate) fn runtime(app: &AppHandle) -> Result<Arc<Runtime>, String> {
    runtime_for(app, super::catalog::active(ModelKind::Search))
}
fn runtime_for(
    app: &AppHandle,
    spec: &'static super::catalog::ModelSpec,
) -> Result<Arc<Runtime>, String> {
    let base = selected_directory(app)?.ok_or("请先选择模型目录并安装模型。")?;
    regular_directory(&base)?;
    let directory = base.join(spec.id);
    regular_directory(&directory)?;
    let current = super::packages::stamps(&directory, &spec.assets)?;
    let mut loaded = LOADED.lock().map_err(error)?;
    if let Some(model) = loaded.as_ref()
        && model.directory == directory
        && model.stamps == current
    {
        return Ok(model.runtime.clone());
    }
    verify_spec(&directory, spec)?;
    inference::initialize(app)?;
    let runtime = Arc::new(Runtime::load_spec(&directory, spec)?);
    // Inspecting an inactive model must not evict the active model or keep both resident.
    if spec.id == super::catalog::active(ModelKind::Search).id {
        *loaded = Some(Loaded {
            directory,
            stamps: current,
            runtime: runtime.clone(),
        });
    }
    Ok(runtime)
}
pub(super) fn verify_spec(
    directory: &Path,
    spec: &super::catalog::ModelSpec,
) -> Result<(), String> {
    regular_directory(directory)?;
    for a in &spec.assets {
        let path = directory.join(a.name);
        if regular_file(&path)?.len() != a.size
            || download::digest(&path, &AtomicBool::new(false))? != a.sha
        {
            return Err(format!("{} 校验失败，请重新安装。", a.name));
        }
    }
    Ok(())
}
fn status_for(
    app: &AppHandle,
    verify: bool,
    spec: &'static super::catalog::ModelSpec,
) -> Result<ModelStatus, String> {
    let base = selected_directory(app)?;
    let active = super::catalog::active(ModelKind::Search);
    let mut value = ModelStatus {
        directory: base.as_ref().map(|p| p.to_string_lossy().into_owned()),
        ready: false,
        total_bytes: spec.assets.iter().map(|a| a.size).sum(),
        resumable_bytes: 0,
        message: None,
        model_id: spec.id,
        active_model_id: active.id,
        active_ready: false,
    };
    if let Some(base) = base {
        let staging = base.join(format!(".{}.partial", spec.id));
        value.resumable_bytes = spec
            .assets
            .iter()
            .map(|a| regular_file(&staging.join(a.name)).map_or(0, |m| m.len().min(a.size)))
            .sum();
        if !verify {
            value.message = Some("正在安装模型…".into());
        } else {
            if base.join(spec.id).exists() {
                match runtime_for(app, spec) {
                    Ok(_) => value.ready = true,
                    Err(e) => value.message = Some(e),
                }
            } else if let Err(e) = regular_directory(&base) {
                value.message = Some(e);
            }
            value.active_ready = if spec.id == active.id {
                value.ready
            } else {
                runtime(app).is_ok()
            };
        }
    }
    Ok(value)
}
fn status(app: &AppHandle, verify: bool) -> Result<ModelStatus, String> {
    status_for(app, verify, super::catalog::active(ModelKind::Search))
}
#[tauri::command]
pub(crate) async fn ai_model_status(
    app: AppHandle,
    kind: Option<ModelKind>,
    model_id: Option<String>,
) -> Result<ModelStatus, String> {
    let kind = kind.unwrap_or_default();
    let spec = match model_id {
        Some(id) => super::catalog::find(kind, &id)?,
        None => super::catalog::active(kind),
    };
    tauri::async_runtime::spawn_blocking(move || {
        if kind == ModelKind::People {
            return super::people_model::status_for(&app, spec);
        }
        match TASK.try_lock() {
            Ok(_task) => status_for(&app, true, spec),
            Err(_) => status_for(&app, false, spec),
        }
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub(crate) async fn ai_set_model_directory(
    app: AppHandle,
    directory: String,
    kind: Option<ModelKind>,
) -> Result<ModelStatus, String> {
    if matches!(kind, Some(ModelKind::People)) {
        return tauri::async_runtime::spawn_blocking(move || {
            super::people_model::set_directory(&app, &directory)
        })
        .await
        .map_err(error)?;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _task = TASK
            .try_lock()
            .map_err(|_| "请先取消正在进行的模型安装。")?;
        let _index = crate::library::ai_search::stop_and_wait()?;
        let path = Path::new(&directory);
        if !path.is_absolute() {
            return Err("请选择完整的模型目录路径。".into());
        }
        regular_directory(path)?;
        let config = config(&app)?;
        fs::create_dir_all(config.parent().ok_or("找不到配置目录。")?).map_err(error)?;
        let mut temp = tempfile::NamedTempFile::new_in(config.parent().unwrap()).map_err(error)?;
        temp.write_all(directory.as_bytes()).map_err(error)?;
        temp.persist(&config).map_err(error)?;
        MODEL_GENERATION.fetch_add(1, Ordering::Relaxed);
        *LOADED.lock().map_err(|_| "模型状态异常。")? = None;
        status(&app, true)
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub(crate) fn ai_cancel_install(kind: Option<ModelKind>) {
    if matches!(kind, Some(ModelKind::People)) {
        super::people_model::cancel_install();
        return;
    }
    CANCEL.store(true, Ordering::Relaxed);
}

#[tauri::command]
pub(crate) async fn ai_choose_model_package(
    window: tauri::WebviewWindow,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("导入 PhotoSys 离线模型包")
            .add_filter("PhotoSys 模型包", &["zip"])
            .blocking_pick_file()
            .map(|file| {
                file.into_path().map_err(error).and_then(|p| {
                    p.into_os_string()
                        .into_string()
                        .map_err(|_| "模型包路径无法显示。".into())
                })
            })
            .transpose()
    })
    .await
    .map_err(error)?
}

fn install_spec(
    app: &AppHandle,
    spec: &'static super::catalog::ModelSpec,
    source: Source,
    package: Option<&Path>,
    channel: &Channel<InstallProgress>,
) -> Result<ModelStatus, String> {
    let _task = TASK.try_lock().map_err(|_| "模型安装任务正在运行。")?;
    CANCEL.store(false, Ordering::Relaxed);
    let base = selected_directory(app)?.ok_or("请先选择模型安装目录。")?;
    // Installation does not select the target. Stop only when repairing active files.
    let _index = if spec.id == super::catalog::active(ModelKind::Search).id {
        Some(crate::library::ai_search::stop_and_wait()?)
    } else {
        None
    };
    super::packages::install(
        &base,
        spec.id,
        &spec.assets,
        &CANCEL,
        source,
        package,
        channel,
        include_str!("../../../scripts/AI-MODEL-NOTICES.md"),
        || {
            inference::initialize(app)?;
            Runtime::load_spec(&base.join(format!(".{}.partial", spec.id)), spec)
        },
        || {
            if spec.id == super::catalog::active(ModelKind::Search).id {
                *LOADED.lock().map_err(error)? = None;
            }
            Ok(())
        },
    )?;
    drop(_index);
    status_for(app, true, spec)
}
#[cfg(test)]
fn install_into(
    base: &Path,
    source: Source,
    package: Option<&Path>,
    channel: &Channel<InstallProgress>,
    initialize: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let spec = super::catalog::default_model(ModelKind::Search);
    let (stamps, runtime) = super::packages::install(
        base,
        spec.id,
        &spec.assets,
        &CANCEL,
        source,
        package,
        channel,
        include_str!("../../../scripts/AI-MODEL-NOTICES.md"),
        || {
            initialize()?;
            Runtime::load_spec(&staging_directory(base), spec).map(Arc::new)
        },
        || Ok(()),
    )?;
    *LOADED.lock().map_err(error)? = Some(Loaded {
        directory: installed_directory(base),
        stamps,
        runtime,
    });
    Ok(())
}
#[tauri::command]
pub(crate) async fn ai_install_model(
    app: AppHandle,
    source: Source,
    package: Option<String>,
    progress: Channel<InstallProgress>,
    kind: Option<ModelKind>,
    model_id: Option<String>,
) -> Result<ModelStatus, String> {
    let kind = kind.unwrap_or_default();
    let spec = match model_id {
        Some(id) => super::catalog::find(kind, &id)?,
        None => super::catalog::active(kind),
    };
    tauri::async_runtime::spawn_blocking(move || {
        if kind == ModelKind::People {
            super::people_model::install_spec(
                &app,
                spec,
                source,
                package.as_deref().map(Path::new),
                &progress,
            )
        } else {
            install_spec(
                &app,
                spec,
                source,
                package.as_deref().map(Path::new),
                &progress,
            )
        }
    })
    .await
    .map_err(error)?
}
pub(super) fn persist_choice(
    app: &AppHandle,
    spec: &super::catalog::ModelSpec,
) -> Result<(), String> {
    let config = super::catalog::selection_path(app, spec.kind)?;
    let parent = config.parent().ok_or("缺少配置目录。")?;
    fs::create_dir_all(parent).map_err(error)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
    temp.write_all(spec.id.as_bytes()).map_err(error)?;
    temp.as_file().sync_all().map_err(error)?;
    temp.persist(config).map_err(error)?;
    super::catalog::select(spec);
    Ok(())
}
#[tauri::command]
pub(crate) async fn ai_activate_model(
    app: AppHandle,
    kind: Option<ModelKind>,
    model_id: String,
) -> Result<ModelStatus, String> {
    let kind = kind.unwrap_or_default();
    let spec = super::catalog::find(kind, &model_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        if kind == ModelKind::People {
            return super::people_model::activate(&app, spec);
        }
        let _task = TASK.try_lock().map_err(|_| "请等待模型安装结束。")?;
        let _index = crate::library::ai_search::stop_and_wait()?;
        // Validate before committing. On failure the selection and old cache survive.
        let runtime = runtime_for(&app, spec)?;
        let base = selected_directory(&app)?.ok_or("缺少模型目录。")?;
        let directory = base.join(spec.id);
        let stamps = super::packages::stamps(&directory, &spec.assets)?;
        persist_choice(&app, spec)?;
        MODEL_GENERATION.fetch_add(1, Ordering::Relaxed);
        *LOADED.lock().map_err(error)? = Some(Loaded {
            stamps,
            directory,
            runtime,
        });
        status_for(&app, true, spec)
    })
    .await
    .map_err(error)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_is_pinned_and_corrupt_models_fail() {
        assert_eq!(ASSETS.iter().map(|a| a.size).sum::<u64>(), 755533779);
        assert!(
            ASSETS
                .iter()
                .all(|a| a.revision.len() == 40 && a.sha.len() == 64)
        );
        let temp = tempfile::tempdir().unwrap();
        for a in &*ASSETS {
            fs::write(temp.path().join(a.name), b"broken").unwrap();
        }
        assert!(verify_assets(temp.path(), &AtomicBool::new(false)).is_err());
    }

    #[test]
    #[ignore = "requires PHOTOSYS_AI_PACKAGE pointing to a validated offline ZIP"]
    fn real_offline_install_and_reuse() {
        let _task = TASK.lock().unwrap();
        let package =
            PathBuf::from(std::env::var_os("PHOTOSYS_AI_PACKAGE").expect("PHOTOSYS_AI_PACKAGE"));
        let temp = tempfile::tempdir().unwrap();
        let name = if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        let runtime_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/onnxruntime")
            .join(std::env::consts::ARCH)
            .join(name);
        let channel = Channel::new(|_| Ok(()));
        CANCEL.store(false, Ordering::Relaxed);
        install_into(
            temp.path(),
            Source::Mirror,
            Some(&package),
            &channel,
            || inference::initialize_from(&runtime_path),
        )
        .unwrap();
        let directory = installed_directory(temp.path());
        verify_assets(&directory, &AtomicBool::new(false)).unwrap();
        assert!(directory.join("README.txt").is_file());
        // Re-selecting an installed directory can use the already self-tested runtime.
        let loaded = LOADED.lock().unwrap();
        let loaded = loaded.as_ref().unwrap();
        assert_eq!(loaded.directory, directory);
        assert_eq!(loaded.stamps, stamps(&directory).unwrap());
        loaded.runtime.encode_text("一只小狗").unwrap();
    }

    #[test]
    fn incomplete_offline_package_never_publishes_installation() {
        let _task = TASK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("incomplete.zip");
        let mut zip = zip::ZipWriter::new(File::create(&package).unwrap());
        zip.start_file("../../outside", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"not a model").unwrap();
        zip.finish().unwrap();
        CANCEL.store(false, Ordering::Relaxed);
        assert!(
            install_into(
                temp.path(),
                Source::Mirror,
                Some(&package),
                &Channel::new(|_| Ok(())),
                || Ok(())
            )
            .is_err()
        );
        assert!(!installed_directory(temp.path()).exists());
        assert!(!temp.path().parent().unwrap().join("outside").exists());
    }

    #[test]
    #[ignore = "requires network and PHOTOSYS_AI_MODEL_DIR with the verified encoders"]
    fn real_download_resumes_pinned_vocabulary() {
        let _task = TASK.lock().unwrap();
        let source = PathBuf::from(
            std::env::var_os("PHOTOSYS_AI_MODEL_DIR").expect("PHOTOSYS_AI_MODEL_DIR"),
        );
        let temp = tempfile::tempdir().unwrap();
        let staging = staging_directory(temp.path());
        fs::create_dir(&staging).unwrap();
        for asset in &ASSETS[..2] {
            fs::hard_link(source.join(asset.name), staging.join(asset.name))
                .or_else(|_| {
                    fs::copy(source.join(asset.name), staging.join(asset.name)).map(|_| ())
                })
                .unwrap();
        }
        let vocabulary = fs::read(source.join("vocab.txt")).unwrap();
        fs::write(staging.join("vocab.txt"), &vocabulary[..5000]).unwrap();
        let name = if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        let runtime_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/onnxruntime")
            .join(std::env::consts::ARCH)
            .join(name);
        CANCEL.store(false, Ordering::Relaxed);
        install_into(
            temp.path(),
            Source::Mirror,
            None,
            &Channel::new(|_| Ok(())),
            || inference::initialize_from(&runtime_path),
        )
        .unwrap();
        assert_eq!(
            fs::read(installed_directory(temp.path()).join("vocab.txt")).unwrap(),
            vocabulary
        );
    }
}

#[tauri::command]
pub(crate) async fn ai_model_change_impact(
    scan_id: u64,
    kind: Option<ModelKind>,
    model_id: String,
) -> Result<usize, String> {
    let kind = kind.unwrap_or_default();
    let spec = super::catalog::find(kind, &model_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        if kind == ModelKind::People {
            crate::library::people::model_impact(scan_id, spec)
        } else {
            crate::library::ai_search::model_impact(scan_id, spec)
        }
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub(crate) fn ai_open_model_source(
    kind: Option<ModelKind>,
    model_id: String,
) -> Result<(), String> {
    let spec = super::catalog::find(kind.unwrap_or_default(), &model_id)?;
    tauri_plugin_opener::open_url(spec.source, None::<&str>).map_err(error)
}
