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

// The opener plugin can silently fall back to opening a directory on Linux and
// Windows. Use the strict reveal APIs there so the UI can report that fallback.
#[cfg(target_os = "linux")]
pub(crate) fn reveal(path: &Path) -> Result<(), String> {
    use gio::glib::variant::ToVariant;
    use gio::prelude::FileExt;

    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|error| error.to_string())?;
    let parameters = (vec![gio::File::for_path(path).uri().to_string()], "").to_variant();
    connection
        .call_sync(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            Some(&parameters),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
pub(crate) fn reveal(path: &Path) -> Result<(), String> {
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(|error| error.to_string())
}

#[cfg(windows)]
pub(crate) fn reveal(path: &Path) -> Result<(), String> {
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        Common::ITEMIDLIST, ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems,
    };
    use windows::core::HSTRING;

    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            // Balanced with the successful CoInitializeEx on this worker.
            unsafe { CoUninitialize() };
        }
    }
    struct ItemIdList(*const ITEMIDLIST);
    impl ItemIdList {
        fn new(path: &Path) -> Result<Self, String> {
            let path = std::path::PathBuf::from(crate::duplicates::shell_path(path)?);
            // The HSTRING remains alive for the duration of the Shell call.
            let item = unsafe { ILCreateFromPathW(&HSTRING::from(path.as_path())) };
            if item.is_null() {
                Err("Windows Shell 无法识别此路径。".into())
            } else {
                Ok(Self(item))
            }
        }
    }
    impl Drop for ItemIdList {
        fn drop(&mut self) {
            // ILCreateFromPathW allocated this non-null list with the Shell allocator.
            unsafe { ILFree(Some(self.0)) };
        }
    }
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
            .ok()
            .map_err(|error| error.to_string())?;
    }
    let _com = Com;
    let item = ItemIdList::new(path)?;
    // With an empty selection array, the absolute PIDL identifies the single
    // item to select and the Shell opens its parent. Keep it alive for the call.
    unsafe { SHOpenFolderAndSelectItems(item.0, None, 0) }.map_err(|error| error.to_string())
}
