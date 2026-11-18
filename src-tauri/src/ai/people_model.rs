//! Independent face model package, sharing transport, verification and publication.
use super::{
    download, error,
    faces::FaceRuntime,
    inference,
    model::{Asset, InstallProgress, MODEL_GENERATION, ModelStatus, Source, regular_directory},
    packages,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{AppHandle, Manager, ipc::Channel};

pub(crate) const MODEL_ID: &str = "yunet-sface-fp32-v1";
pub(crate) const ASSETS: &[Asset] = &[
    Asset {
        name: "face_detection_yunet_2023mar.onnx",
        size: 232589,
        sha: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
        repository: "opencv/face_detection_yunet",
        revision: "3cc26e7f1014a5ee5d74a42acee58bafc9d0a310",
    },
    Asset {
        name: "face_recognition_sface_2021dec.onnx",
        size: 38696353,
        sha: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
        repository: "opencv/face_recognition_sface",
        revision: "3d7082438a6e4551e840c9b2bb60b71e8da4b524",
    },
];
static TASK: Mutex<()> = Mutex::new(());
static CANCEL: AtomicBool = AtomicBool::new(false);
static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);
struct Loaded {
    directory: PathBuf,
    stamps: packages::Stamps,
    runtime: Arc<FaceRuntime>,
}
fn config(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(error)?
        .join("ai-people-model-directory"))
}
fn selected(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    match fs::read_to_string(config(app)?) {
        Ok(s) => Ok(Some(PathBuf::from(s))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => super::selected_directory(app),
        Err(e) => Err(error(e)),
    }
}
pub(crate) fn runtime(app: &AppHandle) -> Result<Arc<FaceRuntime>, String> {
    let base = selected(app)?.ok_or("请先选择人物模型目录并安装模型。")?;
    regular_directory(&base)?;
    let directory = base.join(MODEL_ID);
    regular_directory(&directory)?;
    let stamps = packages::stamps(&directory, ASSETS)?;
    let mut loaded = LOADED.lock().map_err(error)?;
    if let Some(l) = loaded.as_ref()
        && l.directory == directory
        && l.stamps == stamps
    {
        return Ok(l.runtime.clone());
    }
    for a in ASSETS {
        if download::digest(&directory.join(a.name), &AtomicBool::new(false))? != a.sha {
            return Err("人物模型校验失败，请重新安装。".into());
        }
    }
    inference::initialize(app)?;
    let runtime = Arc::new(FaceRuntime::load(&directory)?);
    *loaded = Some(Loaded {
        directory,
        stamps,
        runtime: runtime.clone(),
    });
    Ok(runtime)
}
pub(crate) fn status(app: &AppHandle) -> Result<ModelStatus, String> {
    let base = selected(app)?;
    let mut status = ModelStatus {
        directory: base.as_ref().map(|p| p.to_string_lossy().into_owned()),
        ready: false,
        total_bytes: ASSETS.iter().map(|a| a.size).sum(),
        resumable_bytes: 0,
        message: None,
        model_id: MODEL_ID,
    };
    if let Some(base) = base {
        status.resumable_bytes = ASSETS
            .iter()
            .map(|a| {
                super::model::regular_file(&base.join(format!(".{MODEL_ID}.partial")).join(a.name))
                    .map_or(0, |m| m.len().min(a.size))
            })
            .sum();
        if let Ok(_task) = TASK.try_lock() {
            if base.join(MODEL_ID).exists() {
                match runtime(app) {
                    Ok(_) => status.ready = true,
                    Err(e) => status.message = Some(e),
                }
            } else if let Err(e) = regular_directory(&base) {
                status.message = Some(e);
            }
        } else {
            status.message = Some("正在安装人物模型…".into());
        }
    }
    Ok(status)
}
pub(crate) fn set_directory(app: &AppHandle, directory: &str) -> Result<ModelStatus, String> {
    let task = TASK.try_lock().map_err(|_| "请先取消人物模型安装。")?;
    let path = Path::new(directory);
    if !path.is_absolute() {
        return Err("请选择完整的模型目录路径。".into());
    }
    regular_directory(path)?;
    let config = config(app)?;
    let parent = config.parent().ok_or("缺少配置目录。")?;
    fs::create_dir_all(parent).map_err(error)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
    temp.write_all(directory.as_bytes()).map_err(error)?;
    temp.persist(config).map_err(error)?;
    MODEL_GENERATION.fetch_add(1, Ordering::Relaxed);
    *LOADED.lock().map_err(error)? = None;
    drop(task);
    status(app)
}
pub(crate) fn cancel_install() {
    CANCEL.store(true, Ordering::Relaxed);
}
pub(crate) fn install(
    app: &AppHandle,
    source: Source,
    package: Option<&Path>,
    channel: &Channel<InstallProgress>,
) -> Result<ModelStatus, String> {
    let task = TASK.try_lock().map_err(|_| "人物模型安装正在运行。")?;
    CANCEL.store(false, Ordering::Relaxed);
    MODEL_GENERATION.fetch_add(1, Ordering::Relaxed);
    let base = selected(app)?.ok_or("请先选择人物模型安装目录。")?;
    let (stamps, runtime) = packages::install(
        &base,
        MODEL_ID,
        ASSETS,
        &CANCEL,
        source,
        package,
        channel,
        include_str!("../../../scripts/PEOPLE-MODEL-NOTICES.md"),
        || {
            inference::initialize(app)?;
            FaceRuntime::load(&base.join(format!(".{MODEL_ID}.partial"))).map(Arc::new)
        },
        || {
            *LOADED.lock().map_err(error)? = None;
            Ok(())
        },
    )?;
    *LOADED.lock().map_err(error)? = Some(Loaded {
        directory: base.join(MODEL_ID),
        stamps,
        runtime,
    });
    MODEL_GENERATION.fetch_add(1, Ordering::Relaxed);
    drop(task);
    status(app)
}

#[cfg(test)]
pub(crate) fn test_install(base: &Path, package: &Path) -> Result<Arc<FaceRuntime>, String> {
    let (_, runtime) = packages::install(
        base,
        MODEL_ID,
        ASSETS,
        &AtomicBool::new(false),
        Source::Official,
        Some(package),
        &Channel::new(|_| Ok(())),
        include_str!("../../../scripts/PEOPLE-MODEL-NOTICES.md"),
        || FaceRuntime::load(&base.join(format!(".{MODEL_ID}.partial"))).map(Arc::new),
        || Ok(()),
    )?;
    Ok(runtime)
}
