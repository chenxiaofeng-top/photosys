use std::path::Path;

#[cfg(target_os = "linux")]
pub(super) fn open_default(path: &Path) -> Result<(), String> {
    use gio::prelude::FileExt;

    gio::AppInfo::launch_default_for_uri(
        &gio::File::for_path(path).uri(),
        gio::AppLaunchContext::NONE,
    )
    .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
pub(super) fn open_default(path: &Path) -> Result<(), String> {
    let output = std::process::Command::new("/usr/bin/open")
        .arg(path)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
}

#[cfg(windows)]
pub(super) fn open_default(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    use windows::core::PCWSTR;

    let path: Vec<_> = crate::duplicates::shell_path(path)?
        .encode_wide()
        .chain([0])
        .collect();
    // The worker initializes COM for the Shell call. The wide path is NUL-terminated.
    let status = unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
            .ok()
            .map_err(|error| error.to_string())?;
        let handle = ShellExecuteW(
            None,
            PCWSTR::null(),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        CoUninitialize();
        handle.0 as isize
    };
    if status > 32 {
        return Ok(());
    }
    Err(format!("Windows Shell 返回错误代码 {status}。"))
}
