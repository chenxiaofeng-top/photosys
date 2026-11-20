//! Independent face model selection and verified installation.
use super::{
    catalog::{self, ModelSpec},
    device::{self, SessionDevice, SessionStatus},
    error,
    faces::FaceRuntime,
    model::{
        self, InstallProgress, ModelKind, ModelStatus, PEOPLE_GENERATION, Source,
        regular_directory, regular_file,
    },
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
    runtime_for(app, catalog::active(ModelKind::People), true)
}

pub(super) fn inference_status() -> Result<Vec<SessionStatus>, String> {
    let loaded = LOADED.lock().map_err(error)?;
    match loaded.as_ref() {
        Some(model) if model.runtime.spec.id == catalog::active(ModelKind::People).id => {
            model.runtime.inference_status()
        }
        _ => Ok(Vec::new()),
    }
}

fn runtime_for(
    app: &AppHandle,
    spec: &'static ModelSpec,
    prefer_device: bool,
) -> Result<Arc<FaceRuntime>, String> {
    let base = selected(app)?.ok_or("请先选择人物模型目录并安装模型。")?;
    regular_directory(&base)?;
    let directory = base.join(spec.id);
    regular_directory(&directory)?;
    let stamps = packages::stamps(&directory, &spec.assets)?;
    let mut loaded = LOADED.lock().map_err(error)?;
    if prefer_device
        && let Some(l) = loaded.as_ref()
        && l.directory == directory
        && l.stamps == stamps
    {
        return Ok(l.runtime.clone());
    }
    model::verify_spec(&directory, spec)?;
    let runtime = Arc::new(if prefer_device {
        FaceRuntime::load_with_device(&directory, spec, &SessionDevice::preferred(app)?)?
    } else {
        device::initialize(app)?;
        FaceRuntime::load_spec(&directory, spec)?
    });
    if spec.id == catalog::active(ModelKind::People).id {
        *loaded = Some(Loaded {
            directory,
            stamps,
            runtime: runtime.clone(),
        });
        device::notify(app);
    }
    Ok(runtime)
}
pub(crate) fn status_for(app: &AppHandle, spec: &'static ModelSpec) -> Result<ModelStatus, String> {
    let base = selected(app)?;
    let active = catalog::active(ModelKind::People);
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
        if let Ok(_task) = TASK.try_lock() {
            if base.join(spec.id).exists() {
                match runtime_for(app, spec, spec.id == active.id) {
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
        } else {
            value.message = Some("正在安装人物模型…".into());
        }
    }
    Ok(value)
}
pub(crate) fn set_directory(app: &AppHandle, directory: &str) -> Result<ModelStatus, String> {
    let task = TASK.try_lock().map_err(|_| "请先取消人物模型安装。")?;
    let _index = crate::library::people::stop_and_wait()?;
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
    PEOPLE_GENERATION.fetch_add(1, Ordering::Relaxed);
    *LOADED.lock().map_err(error)? = None;
    device::notify(app);
    drop(task);
    status_for(app, catalog::active(ModelKind::People))
}
pub(crate) fn cancel_install() {
    CANCEL.store(true, Ordering::Relaxed);
}
pub(crate) fn install_spec(
    app: &AppHandle,
    spec: &'static ModelSpec,
    source: Source,
    package: Option<&Path>,
    channel: &Channel<InstallProgress>,
) -> Result<ModelStatus, String> {
    let task = TASK.try_lock().map_err(|_| "人物模型安装正在运行。")?;
    CANCEL.store(false, Ordering::Relaxed);
    let base = selected(app)?.ok_or("请先选择人物模型安装目录。")?;
    let _index = if spec.id == catalog::active(ModelKind::People).id {
        Some(crate::library::people::stop_and_wait()?)
    } else {
        None
    };
    packages::install(
        &base,
        spec.id,
        &spec.assets,
        &CANCEL,
        source,
        package,
        channel,
        include_str!("../../../scripts/PEOPLE-MODEL-NOTICES.md"),
        || {
            device::initialize(app)?;
            FaceRuntime::load_spec(&base.join(format!(".{}.partial", spec.id)), spec)
        },
        || {
            if spec.id == catalog::active(ModelKind::People).id {
                *LOADED.lock().map_err(error)? = None;
                device::notify(app);
            }
            Ok(())
        },
    )?;
    drop(_index);
    drop(task);
    status_for(app, spec)
}
pub(crate) fn activate(app: &AppHandle, spec: &'static ModelSpec) -> Result<ModelStatus, String> {
    let task = TASK.try_lock().map_err(|_| "请等待人物模型安装结束。")?;
    let _index = crate::library::people::stop_and_wait()?;
    let runtime = runtime_for(app, spec, true)?;
    let directory = selected(app)?.ok_or("缺少模型目录。")?.join(spec.id);
    let stamps = packages::stamps(&directory, &spec.assets)?;
    model::persist_choice(app, spec)?;
    PEOPLE_GENERATION.fetch_add(1, Ordering::Relaxed);
    *LOADED.lock().map_err(error)? = Some(Loaded {
        directory,
        stamps,
        runtime,
    });
    device::notify(app);
    drop(task);
    status_for(app, spec)
}
#[cfg(test)]
pub(crate) fn test_install(base: &Path, package: &Path) -> Result<Arc<FaceRuntime>, String> {
    let spec = catalog::default_model(ModelKind::People);
    let (_, runtime) = packages::install(
        base,
        spec.id,
        &spec.assets,
        &AtomicBool::new(false),
        Source::Official,
        Some(package),
        &Channel::new(|_| Ok(())),
        include_str!("../../../scripts/PEOPLE-MODEL-NOTICES.md"),
        || FaceRuntime::load_spec(&base.join(format!(".{}.partial", spec.id)), spec).map(Arc::new),
        || Ok(()),
    )?;
    Ok(runtime)
}
