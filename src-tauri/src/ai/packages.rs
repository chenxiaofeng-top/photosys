//! Shared verified, resumable model installation for independent model packages.
use super::{
    download, error,
    model::{Asset, InstallProgress, Source, regular_directory, regular_file},
};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
use tauri::ipc::Channel;
pub(super) type Stamps = Vec<(u64, std::time::SystemTime)>;
pub(super) fn stamps(path: &Path, assets: &[Asset]) -> Result<Stamps, String> {
    assets
        .iter()
        .map(|a| {
            let m = regular_file(&path.join(a.name))?;
            Ok((m.len(), m.modified().map_err(error)?))
        })
        .collect()
}
fn ensure_directory(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => regular_directory(path),
        Err(e) => Err(error(e)),
    }
}
fn progress(
    channel: &Channel<InstallProgress>,
    assets: &[Asset],
    phase: &'static str,
    asset: &Asset,
    downloaded: u64,
    source: Source,
) {
    let _ = channel.send(InstallProgress {
        phase,
        file: asset.name.into(),
        downloaded,
        total: assets.iter().map(|a| a.size).sum(),
        source,
    });
}
#[allow(clippy::too_many_arguments)]
pub(super) fn install<T>(
    base: &Path,
    id: &str,
    assets: &[Asset],
    cancel: &AtomicBool,
    source: Source,
    package: Option<&Path>,
    channel: &Channel<InstallProgress>,
    notices_text: &str,
    check: impl FnOnce() -> Result<T, String>,
    before_publish: impl FnOnce() -> Result<(), String>,
) -> Result<(Stamps, T), String> {
    regular_directory(base)?;
    let staging = base.join(format!(".{id}.partial"));
    ensure_directory(&staging)?;
    let total: u64 = assets.iter().map(|a| a.size).sum();
    let reusable: u64 = assets
        .iter()
        .map(|a| regular_file(&staging.join(a.name)).map_or(0, |m| m.len().min(a.size)))
        .sum();
    if fs2::available_space(base).map_err(error)?
        < total.saturating_sub(reusable) + 64 * 1024 * 1024
    {
        return Err("模型目录的可用空间不足，请释放空间后重试。".into());
    }
    let client = download::client()?;
    let mut archive = package
        .map(|path| {
            regular_file(path)?;
            zip::ZipArchive::new(File::open(path).map_err(error)?).map_err(error)
        })
        .transpose()?;
    let mut completed = 0;
    for asset in assets {
        download::check_cancel(cancel)?;
        let path = staging.join(asset.name);
        if path.exists() {
            regular_file(&path)?;
        }
        if regular_file(&path).is_ok_and(|m| m.len() == asset.size) {
            progress(
                channel,
                assets,
                "verifying",
                asset,
                completed + asset.size,
                source,
            );
            if download::digest(&path, cancel)? == asset.sha {
                completed += asset.size;
                continue;
            }
            fs::remove_file(&path).map_err(error)?;
        }
        if let Some(archive) = archive.as_mut() {
            let mut entry = archive
                .by_name(asset.name)
                .map_err(|_| format!("离线包缺少 {}，请导入完整模型包。", asset.name))?;
            if !entry.is_file() || entry.size() != asset.size {
                return Err(format!("离线包中的 {} 大小不正确。", asset.name));
            }
            let mut output = download::output(&path, false)?;
            let mut buffer = [0; 128 * 1024];
            let mut written = 0;
            loop {
                download::check_cancel(cancel)?;
                let count = entry.read(&mut buffer).map_err(error)?;
                if count == 0 {
                    break;
                }
                written += count as u64;
                if written > asset.size {
                    return Err("离线包文件超过预期大小。".into());
                }
                output.write_all(&buffer[..count]).map_err(error)?;
                progress(
                    channel,
                    assets,
                    "importing",
                    asset,
                    completed + written,
                    source,
                );
            }
            output.sync_all().map_err(error)?;
        } else {
            let sources = match source {
                Source::Mirror => [Source::Mirror, Source::Official],
                Source::Official => [Source::Official, Source::Mirror],
            };
            let mut last_error = None;
            for origin in sources {
                let host = match origin {
                    Source::Mirror => "https://hf-mirror.com",
                    Source::Official => "https://huggingface.co",
                };
                let url = format!(
                    "{host}/{}/resolve/{}/{}",
                    asset.repository, asset.revision, asset.name
                );
                match download::transfer(&client, &url, &path, asset.size, cancel, |bytes| {
                    progress(
                        channel,
                        assets,
                        "downloading",
                        asset,
                        completed + bytes,
                        origin,
                    )
                }) {
                    Ok(()) => {
                        progress(
                            channel,
                            assets,
                            "verifying",
                            asset,
                            completed + asset.size,
                            origin,
                        );
                        if download::digest(&path, cancel)? == asset.sha {
                            last_error = None;
                            break;
                        }
                        fs::remove_file(&path).map_err(error)?;
                        last_error = Some(format!(
                            "{} SHA-256 校验失败，请重试或切换下载源。",
                            asset.name
                        ));
                    }
                    Err(e) => {
                        download::check_cancel(cancel)?;
                        last_error = Some(e);
                    }
                }
            }
            if let Some(e) = last_error {
                return Err(e);
            }
        }
        if archive.is_some() {
            progress(
                channel,
                assets,
                "verifying",
                asset,
                completed + asset.size,
                source,
            );
            if regular_file(&path)?.len() != asset.size
                || download::digest(&path, cancel)? != asset.sha
            {
                fs::remove_file(&path).map_err(error)?;
                return Err(format!(
                    "{} SHA-256 校验失败，请重试或切换下载源。",
                    asset.name
                ));
            }
        }
        completed += asset.size;
    }
    download::check_cancel(cancel)?;
    progress(channel, assets, "testing", &assets[0], total, source);
    let checked_stamps = stamps(&staging, assets)?;
    let checked_runtime = check()?;
    if checked_stamps != stamps(&staging, assets)? {
        return Err("模型文件在校验后发生变化，请重新安装。".into());
    }
    download::check_cancel(cancel)?;
    let mut notices = tempfile::NamedTempFile::new_in(&staging).map_err(error)?;
    notices.write_all(notices_text.as_bytes()).map_err(error)?;
    notices.persist(staging.join("README.txt")).map_err(error)?;
    let destination = base.join(id);
    before_publish()?;
    if destination.exists() {
        regular_directory(&destination)?;
        // Preserve the previous directory, including unrelated user files, on repair.
        let backup = tempfile::Builder::new()
            .prefix(".photosys-model-backup-")
            .tempdir_in(&base)
            .map_err(error)?
            .keep();
        fs::remove_dir(&backup).map_err(error)?;
        fs::rename(&destination, &backup).map_err(error)?;
        if let Err(e) = fs::rename(&staging, &destination) {
            fs::rename(&backup, &destination).map_err(|restore| {
                format!(
                    "安装未完成，原模型保存在 {}。{e}；{restore}",
                    backup.display()
                )
            })?;
            return Err(error(e));
        }
    } else {
        fs::rename(&staging, &destination).map_err(error)?;
    }
    Ok((checked_stamps, checked_runtime))
}
