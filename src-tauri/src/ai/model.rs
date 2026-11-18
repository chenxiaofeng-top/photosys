use std::fs;
#[cfg(test)] use std::fs::File;
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
    MODEL_ID, download, error,
    inference::{self, Runtime},
};

pub(crate) static MODEL_GENERATION: AtomicU64 = AtomicU64::new(0);
static TASK: Mutex<()> = Mutex::new(());
static CANCEL: AtomicBool = AtomicBool::new(false);
static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);

struct Loaded {
    directory: PathBuf,
    stamps: Vec<(u64, std::time::SystemTime)>,
    runtime: Arc<Runtime>,
}

#[derive(Clone, Copy)]
pub(crate) struct Asset {
    pub name: &'static str,
    pub size: u64,
    pub sha: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
}

pub(crate) const ASSETS: &[Asset] = &[
    Asset {
        name: "cn_clip_text.onnx",
        size: 409585120,
        sha: "5ddc2d8971b09acda8063048003d18cca9b587f089a1c0a4df7d35624bd0fcea",
        repository: "felixdu/chinese-clip-vit-base-patch16-onnx",
        revision: "47080d16c631d8416d2e6b155c59f8fd2c322e98",
    },
    Asset {
        name: "cn_clip_vision.onnx",
        size: 345839119,
        sha: "980020bf226e528a202de26c6a6186df125b74c612b09cfcd797a082e046b628",
        repository: "felixdu/chinese-clip-vit-base-patch16-onnx",
        revision: "47080d16c631d8416d2e6b155c59f8fd2c322e98",
    },
    Asset {
        name: "vocab.txt",
        size: 109540,
        sha: "45bbac6b341c319adc98a532532882e91a9cefc0329aa57bac9ae761c27b291c",
        repository: "OFA-Sys/chinese-clip-vit-base-patch16",
        revision: "36e679e65c2a2fead755ae21162091293ad37834",
    },
];

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ModelKind { #[default] Search, People }

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

fn config(app: &AppHandle) -> Result<PathBuf, String> {
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

pub(crate) fn installed_directory(base: &Path) -> PathBuf {
    base.join(MODEL_ID)
}

fn staging_directory(base: &Path) -> PathBuf {
    base.join(format!(".{MODEL_ID}.partial"))
}


pub(crate) fn verify_assets(directory: &Path, cancel: &AtomicBool) -> Result<(), String> {
    regular_directory(directory)?;
    for asset in ASSETS {
        let path = directory.join(asset.name);
        if regular_file(&path)?.len() != asset.size || download::digest(&path, cancel)? != asset.sha
        {
            return Err(format!("{} 校验失败，请重新下载或导入。", asset.name));
        }
    }
    Ok(())
}

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
    let base = selected_directory(app)?.ok_or("请先选择模型目录并安装模型。")?;
    regular_directory(&base)?;
    let directory = installed_directory(&base);
    regular_directory(&directory)?;
    let current = stamps(&directory).map_err(|e| format!("模型不完整，请下载安装。{e}"))?;
    let mut loaded = LOADED.lock().map_err(|_| "模型状态异常，请重启软件。")?;
    if let Some(model) = loaded.as_ref()
        && model.directory == directory
        && model.stamps == current
    {
        return Ok(model.runtime.clone());
    }
    verify_assets(&directory, &AtomicBool::new(false))?;
    inference::initialize(app)?;
    let runtime = Arc::new(Runtime::load(&directory)?);
    *loaded = Some(Loaded {
        directory,
        stamps: current,
        runtime: runtime.clone(),
    });
    Ok(runtime)
}

fn status(app: &AppHandle, verify: bool) -> Result<ModelStatus, String> {
    let base = selected_directory(app)?;
    let total_bytes = ASSETS.iter().map(|a| a.size).sum();
    let mut status = ModelStatus {
        directory: base.as_ref().map(|p| p.to_string_lossy().into_owned()),
        ready: false,
        total_bytes,
        resumable_bytes: 0,
        message: None,
        model_id: MODEL_ID,
    };
    if let Some(base) = base {
        let staging = staging_directory(&base);
        status.resumable_bytes = ASSETS
            .iter()
            .map(|a| regular_file(&staging.join(a.name)).map_or(0, |m| m.len().min(a.size)))
            .sum();
        if !verify {
            status.message = Some("正在安装模型…".into());
        } else if installed_directory(&base).is_dir() {
            match runtime(app) {
                Ok(_) => status.ready = true,
                Err(e) => status.message = Some(e),
            }
        } else if let Err(e) = regular_directory(&base) {
            status.message = Some(e);
        }
    }
    Ok(status)
}

#[tauri::command]
pub(crate) async fn ai_model_status(app: AppHandle, kind: Option<ModelKind>) -> Result<ModelStatus, String> {
    if matches!(kind, Some(ModelKind::People)) {
        return tauri::async_runtime::spawn_blocking(move || super::people_model::status(&app)).await.map_err(error)?;
    }
    tauri::async_runtime::spawn_blocking(move || match TASK.try_lock() {
        Ok(_task) => status(&app, true),
        Err(_) => status(&app, false),
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
        return tauri::async_runtime::spawn_blocking(move || super::people_model::set_directory(&app, &directory)).await.map_err(error)?;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _task = TASK
            .try_lock()
            .map_err(|_| "请先取消正在进行的模型安装。")?;
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
    if matches!(kind, Some(ModelKind::People)) { super::people_model::cancel_install(); return; }
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


fn install(
    app: &AppHandle,
    source: Source,
    package: Option<&Path>,
    channel: &Channel<InstallProgress>,
) -> Result<ModelStatus, String> {
    let _task = TASK.try_lock().map_err(|_| "模型安装任务正在运行。")?;
    CANCEL.store(false, Ordering::Relaxed);
    MODEL_GENERATION.fetch_add(1, Ordering::Relaxed);
    let base = selected_directory(app)?.ok_or("请先选择模型安装目录。")?;
    install_into(&base, source, package, channel, || {
        inference::initialize(app)
    })?;
    status(app, true)
}

fn install_into(
    base: &Path,
    source: Source,
    package: Option<&Path>,
    channel: &Channel<InstallProgress>,
    initialize: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let (checked_stamps, checked_runtime) = super::packages::install(
        base, MODEL_ID, ASSETS, &CANCEL, source, package, channel,
        include_str!("../../../scripts/AI-MODEL-NOTICES.md"),
        || { initialize()?; Runtime::load(&staging_directory(base)).map(Arc::new) },
        || { *LOADED.lock().map_err(|_| "模型状态异常。")? = None; Ok(()) },
    )?;
    MODEL_GENERATION.fetch_add(1, Ordering::Relaxed);
    *LOADED.lock().map_err(|_| "模型状态异常。")? = Some(Loaded {
        directory: installed_directory(base), stamps: checked_stamps, runtime: checked_runtime,
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
) -> Result<ModelStatus, String> {
    if matches!(kind, Some(ModelKind::People)) {
        return tauri::async_runtime::spawn_blocking(move || super::people_model::install(&app, source, package.as_deref().map(Path::new), &progress)).await.map_err(error)?;
    }
    tauri::async_runtime::spawn_blocking(move || {
        install(&app, source, package.as_deref().map(Path::new), &progress)
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
        for a in ASSETS {
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
