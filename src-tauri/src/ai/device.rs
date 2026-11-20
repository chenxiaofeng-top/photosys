use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use super::{ModelKind, error};

const SUPPORTS_DIRECTML: bool = cfg!(all(target_os = "windows", target_arch = "x86_64"));
const DEVICE_CHANGED: &str = "ai-inference-changed";

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Preference {
    #[default]
    Auto,
    Cpu,
}

pub(super) fn prefers_directml(app: &AppHandle) -> bool {
    SUPPORTS_DIRECTML && app.state::<Settings>().current == Preference::Auto
}

pub(super) fn indexing_uses_directml() -> bool {
    let (Ok(search), Ok(people)) = (
        super::model::inference_status(),
        super::people_model::inference_status(),
    ) else {
        return false;
    };
    // The other model may still be cold. A known CPU fallback keeps admission serial.
    !(search.is_empty() && people.is_empty())
        && search.iter().chain(&people).all(|session| session.directml)
}

struct Settings {
    current: Preference,
    saved: Mutex<Preference>,
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(error)?
        .join("ai-inference-device.json"))
}

fn read_preference(path: &Path) -> Preference {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(Preference::Cpu),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Preference::Auto,
        Err(_) => Preference::Cpu,
    }
}

pub(crate) fn initialize_settings(app: &AppHandle) {
    let current = if SUPPORTS_DIRECTML {
        settings_path(app).map_or(Preference::Cpu, |path| read_preference(&path))
    } else {
        Preference::Cpu
    };
    app.manage(Settings {
        current,
        saved: Mutex::new(current),
    });
}

pub(super) fn notify(app: &AppHandle) {
    let _ = app.emit(DEVICE_CHANGED, ());
}

#[tauri::command]
pub(crate) fn ai_set_inference_preference(
    app: AppHandle,
    preference: Preference,
) -> Result<(), String> {
    if !SUPPORTS_DIRECTML {
        return Err("当前平台使用 CPU 推理。".into());
    }
    let settings = app.state::<Settings>();
    let mut saved = settings.saved.lock().map_err(error)?;
    if *saved != preference {
        let path = settings_path(&app)?;
        let parent = path.parent().ok_or("缺少配置目录。")?;
        fs::create_dir_all(parent).map_err(error)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        serde_json::to_writer(&mut temp, &preference).map_err(error)?;
        temp.flush().map_err(error)?;
        temp.as_file().sync_all().map_err(error)?;
        temp.persist(path).map_err(error)?;
        *saved = preference;
    }
    drop(saved);
    notify(&app);
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionStatus {
    pub(super) directml: bool,
    pub(super) fallback: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceStatus {
    supports_directml: bool,
    current: Preference,
    saved: Preference,
    sessions: Vec<SessionStatus>,
}

#[tauri::command]
pub(crate) async fn ai_inference_status(
    app: AppHandle,
    kind: Option<ModelKind>,
) -> Result<InferenceStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = app.state::<Settings>();
        let saved = *settings.saved.lock().map_err(error)?;
        let sessions = match kind.unwrap_or_default() {
            ModelKind::Search => super::model::inference_status(),
            ModelKind::People => super::people_model::inference_status(),
        }?;
        Ok(InferenceStatus {
            supports_directml: SUPPORTS_DIRECTML,
            current: settings.current,
            saved,
            sessions,
        })
    })
    .await
    .map_err(error)?
}

#[derive(Clone)]
pub(crate) struct SessionDevice {
    pub(super) status: SessionStatus,
    on_fallback: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
    pub(super) profile_dir: Option<PathBuf>,
}

impl SessionDevice {
    pub(super) fn cpu() -> Self {
        Self {
            status: SessionStatus {
                directml: false,
                fallback: None,
            },
            on_fallback: None,
            #[cfg(all(test, target_os = "windows", target_arch = "x86_64"))]
            profile_dir: None,
        }
    }

    pub(super) fn preferred(app: &AppHandle) -> Result<Self, String> {
        let mut device = Self::cpu();
        device.status = initialized_runtime(app)?;
        let app = app.clone();
        device.on_fallback = Some(Arc::new(move || notify(&app)));
        Ok(device)
    }

    pub(super) fn notify_fallback(&self) {
        if let Some(notify) = &self.on_fallback {
            notify();
        }
    }
}

static INITIALIZED: OnceLock<Result<SessionStatus, String>> = OnceLock::new();

pub(super) fn initialize(app: &AppHandle) -> Result<(), String> {
    initialized_runtime(app).map(|_| ())
}

fn initialized_runtime(app: &AppHandle) -> Result<SessionStatus, String> {
    INITIALIZED.get_or_init(|| select_runtime(app)).clone()
}

fn select_runtime(app: &AppHandle) -> Result<SessionStatus, String> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    let fallback = if prefers_directml(app) {
        match load_directml(&runtime_path(
            app,
            "onnxruntime/directml/x86_64/onnxruntime.dll",
        )?) {
            Ok(()) => {
                return Ok(SessionStatus {
                    directml: true,
                    fallback: None,
                });
            }
            Err(e) => Some(format!("无法加载 DirectML 运行库。{e}")),
        }
    } else {
        None
    };
    #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
    let fallback = None;
    let name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    let relative = format!("onnxruntime/{}/{name}", std::env::consts::ARCH);
    let path = runtime_path(app, &relative)?;
    if !path.is_file() {
        return Err(
            "缺少 AI 运行时。请重新安装 PhotoSys，或在开发环境运行 scripts/prepare-ai-runtime.py。"
                .into(),
        );
    }
    initialize_from(&path)?;
    Ok(SessionStatus {
        directml: false,
        fallback,
    })
}

fn runtime_path(app: &AppHandle, relative: &str) -> Result<PathBuf, String> {
    let resource = app.path().resource_dir().map_err(error)?.join(relative);
    if tauri::is_dev() && !resource.is_file() {
        return Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(relative));
    }
    Ok(resource)
}

pub(crate) fn initialize_from(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let _runtime = WindowsLibrary::load(path)?;
    ort::init_from(path)
        .map_err(error)?
        .with_telemetry(false)
        .with_name("PhotoSys local search")
        .commit();
    Ok(())
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub(super) fn load_directml(runtime: &Path) -> Result<(), String> {
    let dependency = runtime.with_file_name("DirectML.dll");
    if !runtime.is_file() || !dependency.is_file() {
        return Err("缺少随安装包提供的 DirectML DLL。".into());
    }
    // ORT delay-loads DirectML.dll. Preload the bundled version for the life of the process.
    let directml = WindowsLibrary::load(&dependency)?;
    initialize_from(runtime)?;
    std::mem::forget(directml);
    Ok(())
}

#[cfg(target_os = "windows")]
struct WindowsLibrary(windows::Win32::Foundation::HMODULE);

#[cfg(target_os = "windows")]
impl WindowsLibrary {
    fn load(path: &Path) -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW,
        };
        use windows::core::PCWSTR;

        let path_wide: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
        // Search for dependencies beside the DLL, beside the executable and in system DLL directories.
        let handle = unsafe {
            LoadLibraryExW(
                PCWSTR(path_wide.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        }
        .map_err(|e| format!("{}：{e}", path.display()))?;
        Ok(Self(handle))
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsLibrary {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::FreeLibrary(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_auto_and_recovers_from_invalid_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.json");
        assert!(read_preference(&path) == Preference::Auto);
        for (bytes, expected) in [
            ("\"cpu\"", Preference::Cpu),
            ("\"auto\"", Preference::Auto),
            ("{", Preference::Cpu),
        ] {
            fs::write(&path, bytes).unwrap();
            assert!(read_preference(&path) == expected);
        }
    }
}
