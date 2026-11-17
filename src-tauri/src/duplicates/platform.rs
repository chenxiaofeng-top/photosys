use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileIdentity(pub u64, pub u64);

pub(super) fn open(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    options.open(path)
}

pub(crate) fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

#[cfg(unix)]
pub(super) fn identity(_: &File, metadata: &Metadata) -> io::Result<(FileIdentity, u64)> {
    use std::os::unix::fs::MetadataExt;
    Ok((
        FileIdentity(metadata.dev(), metadata.ino()),
        metadata.nlink(),
    ))
}

#[cfg(windows)]
pub(super) fn identity(file: &File, _: &Metadata) -> io::Result<(FileIdentity, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // The borrowed file handle and the output structure stay valid for the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(io::Error::other)?;
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok((
        FileIdentity(u64::from(info.dwVolumeSerialNumber), index),
        info.nNumberOfLinks.into(),
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn trash(path: &Path) -> Result<(), String> {
    use gio::prelude::FileExt;
    gio::File::for_path(path)
        .trash(gio::Cancellable::NONE)
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
pub(crate) fn trash(path: &Path) -> Result<(), String> {
    use objc2::rc::autoreleasepool;
    use objc2_foundation::{NSFileManager, NSURL};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::ptr::NonNull;

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    autoreleasepool(|_| {
        // CString supplies a valid NUL-terminated filesystem representation for the call.
        let url = unsafe {
            NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
                NonNull::from(&path.as_bytes_with_nul()[0]).cast(),
                false,
                None,
            )
        };
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|error| error.to_string())
    })
}

#[cfg(windows)]
pub(crate) fn shell_path(path: &Path) -> Result<std::ffi::OsString, String> {
    use std::ffi::OsString;
    use std::path::{Component, Prefix};

    // The Shell needs DOS drive or UNC syntax instead of std's verbatim prefix.
    let mut components = path.components();
    match components.next() {
        Some(Component::Prefix(prefix)) => {
            let mut base = match prefix.kind() {
                Prefix::VerbatimDisk(drive) => OsString::from(format!("{}:", char::from(drive))),
                Prefix::VerbatimUNC(host, share) => {
                    let mut base = OsString::from(r"\\");
                    base.push(host);
                    base.push(r"\");
                    base.push(share);
                    base
                }
                _ => prefix.as_os_str().to_os_string(),
            };
            base.push(components.as_path());
            Ok(base)
        }
        _ => Err("无法识别 Windows 文件路径。".into()),
    }
}

#[cfg(windows)]
pub(crate) fn trash(path: &Path) -> Result<(), String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
        CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        FOF_NO_UI, FOF_NORECURSION, FOF_WANTNUKEWARNING, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE,
        FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
    };
    use windows::core::PCWSTR;

    let shell_path = shell_path(path)?;
    let wide_path: Vec<_> = shell_path.encode_wide().chain([0]).collect();

    struct ComApartment;
    impl Drop for ComApartment {
        fn drop(&mut self) {
            // This value is created only after a successful initialization on this thread.
            unsafe { CoUninitialize() };
        }
    }

    // COM interfaces are created and released on this thread. The wide path is
    // NUL-terminated and remains valid while the Shell creates its path item.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
            .ok()
            .map_err(|error| error.to_string())?;
        let _apartment = ComApartment;
        let recycle = || -> windows::core::Result<bool> {
            let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            operation.SetOperationFlags(
                FOFX_RECYCLEONDELETE
                    | FOF_WANTNUKEWARNING
                    | FOF_NO_UI
                    | FOF_NORECURSION
                    | FOFX_EARLYFAILURE,
            )?;
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide_path.as_ptr()), None)?;
            // Shell parsing can normalize names that the verbatim path preserved.
            let resolved = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let resolved_path = OsString::from_wide(resolved.as_wide());
            CoTaskMemFree(Some(resolved.0.cast()));
            if !std::fs::canonicalize(resolved_path).is_ok_and(|resolved| resolved == path) {
                return Ok(false);
            }
            operation.DeleteItem(&item, None)?;
            operation.PerformOperations()?;
            Ok(!operation.GetAnyOperationsAborted()?.as_bool())
        };
        match recycle() {
            Ok(true) => Ok(()),
            Ok(false) => Err("系统回收站拒绝接收此文件，或操作已取消。".into()),
            Err(error) => Err(error.to_string()),
        }
    }
}
