use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::Client;
use reqwest::header::{CONTENT_RANGE, RANGE};
use sha2::{Digest, Sha256};

use super::error;

pub(super) const CANCELLED: &str = "已取消，可继续下载。";

pub(super) fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}

pub(super) fn digest(path: &Path, cancel: &AtomicBool) -> Result<String, String> {
    let mut file = File::open(path).map_err(error)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 128 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut bytes).map_err(error)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent("PhotoSys/0.1 model-installer")
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()
        .map_err(error)
}

pub(super) fn output(path: &Path, append: bool) -> Result<File, String> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (!metadata.is_file() || crate::duplicates::is_link(&metadata))
    {
        return Err("模型下载目标必须是普通文件。".into());
    }
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options
        .open(path)
        .map_err(|e| format!("无法写入模型，请检查目录权限和磁盘空间。{e}"))?;
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() || crate::duplicates::is_link(&metadata) {
        return Err("模型下载目标必须是普通文件。".into());
    }
    Ok(file)
}

fn range_matches(header: &str, offset: u64, size: u64) -> bool {
    header == format!("bytes {offset}-{}/{size}", size - 1)
}

/// Only append after a precise Content-Range match. A server ignoring Range restarts the file.
pub(super) fn transfer(
    client: &Client,
    url: &str,
    path: &Path,
    size: u64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    tauri::async_runtime::block_on(async {
        check_cancel(cancel)?;
        let offset = fs::metadata(path).map_or(0, |m| m.len());
        if offset == size {
            return Ok(());
        }
        let offset = if offset > size {
            fs::remove_file(path).map_err(error)?;
            0
        } else {
            offset
        };
        let mut request = client.get(url);
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| format!("下载连接失败。{e}"))?
            .error_for_status()
            .map_err(|e| format!("下载源返回错误。{e}"))?;
        let append = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if append
            && !response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| range_matches(h, offset, size))
        {
            return Err("下载源返回的续传范围不正确，请更换下载源重试。".into());
        }
        if !append && response.status() != reqwest::StatusCode::OK {
            return Err("下载源未返回完整文件。".into());
        }
        let mut written = if append { offset } else { 0 };
        let mut output = output(path, append)?;
        let mut last = Instant::now();
        progress(written);
        loop {
            check_cancel(cancel)?;
            let Some(bytes) = response
                .chunk()
                .await
                .map_err(|e| format!("下载中断，可继续下载。{e}"))?
            else {
                break;
            };
            let count = bytes.len();
            if written + count as u64 > size {
                return Err("模型文件超过预期大小。".into());
            }
            output
                .write_all(&bytes[..count])
                .map_err(|e| format!("模型写入失败，请检查磁盘空间。{e}"))?;
            written += count as u64;
            if last.elapsed() >= Duration::from_millis(150) {
                progress(written);
                last = Instant::now();
            }
        }
        output.sync_all().map_err(error)?;
        progress(written);
        if written != size {
            return Err("下载尚未完成，请继续下载。".into());
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[cfg(unix)]
    #[test]
    fn never_follows_a_download_destination_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        let link = temp.path().join("partial");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(output(&link, false).is_err());
        assert!(!outside.exists());
    }

    fn serve(response: &'static str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let join = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&bytes[..count]).to_string()
        });
        (url, join)
    }

    #[test]
    fn resumes_and_restarts_when_server_ignores_range() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("partial");
        let cancel = AtomicBool::new(false);
        let client = Client::builder().no_proxy().build().unwrap();
        for response in [
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-4/5\r\nContent-Length: 3\r\nConnection: close\r\n\r\ncde",
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde",
        ] {
            fs::write(&path, b"ab").unwrap();
            let (url, join) = serve(response);
            transfer(&client, &url, &path, 5, &cancel, |_| {}).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"abcde");
            assert!(
                join.join()
                    .unwrap()
                    .to_lowercase()
                    .contains("range: bytes=2-")
            );
        }
    }

    #[test]
    fn rejects_wrong_ranges_and_keeps_partial_on_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("partial");
        fs::write(&path, b"ab").unwrap();
        let (url, join) = serve(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 1-4/5\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbcde",
        );
        assert!(
            transfer(
                &Client::builder().no_proxy().build().unwrap(),
                &url,
                &path,
                5,
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
        join.join().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"ab");
        assert!(
            transfer(
                &client().unwrap(),
                &url,
                &path,
                5,
                &AtomicBool::new(true),
                |_| {}
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"ab");
    }
}
