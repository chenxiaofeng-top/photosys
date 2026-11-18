use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use nom_exif::{MediaKind, MediaSource};

pub(crate) const CONVERSION_LIMIT: u64 = 16 * 1024 * 1024;
const CONVERSION_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const CANCELLED: &str = "媒体解码已取消。";
static HEIF_DECODER: Mutex<()> = Mutex::new(());

pub(crate) fn is_heif(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("heic") || extension.eq_ignore_ascii_case("heif")
        })
}

fn executable() -> Result<PathBuf, String> {
    let companion = if cfg!(windows) {
        "photosys-ffmpeg.exe"
    } else {
        "photosys-ffmpeg"
    };
    let program = std::env::current_exe().map_err(|error| error.to_string())?;
    Ok(companion_for(&program, companion))
}

fn companion_for(program: &Path, companion: &str) -> PathBuf {
    // Cargo uses either <profile>/deps or <profile>/build/<crate>/<hash>/out
    // for unit tests. tauri-build places sidecars in the profile directory.
    #[cfg(test)]
    if let Some(profile) = program
        .ancestors()
        .find(|path| path.ends_with("deps") || path.ends_with("build"))
        .and_then(Path::parent)
    {
        return profile.join(companion);
    }
    program.with_file_name(companion)
}

pub(crate) enum Input<'a> {
    Mov(&'a Path),
    Video { file: File, demuxer: &'static str },
}

pub(crate) fn convert(
    input: Input<'_>,
    output: &Path,
    output_args: &[&str],
    cancelled: Option<&AtomicBool>,
) -> Result<(File, u64), String> {
    let limit_arg = CONVERSION_LIMIT.to_string();
    let mut command = Command::new(executable()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-max_alloc",
        "268435456",
        "-filter_threads",
        "2",
    ]);
    let (source, protocol, demuxer, stdin) = match input {
        Input::Mov(path) => (path, "file", "mov", Stdio::null()),
        // Unlike pipe:, fd: can seek in a regular file, including to a trailing MOV index.
        Input::Video { file, demuxer } => (Path::new("fd:"), "fd", demuxer, Stdio::from(file)),
    };
    if demuxer == "mov" {
        command.args(["-enable_drefs", "0"]);
    }
    let mut child = command
        .args(["-protocol_whitelist", protocol, "-f", demuxer])
        .args(["-threads", "2", "-max_pixels", "64000000", "-i"])
        .arg(source)
        .args(output_args)
        .args(["-map_metadata", "-1", "-threads", "2", "-fs", &limit_arg])
        .arg(output)
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("无法启动随应用安装的媒体解码器。{error}"))?;

    if !wait(&mut child, cancelled)?.success() {
        return Err("媒体文件无法解码。".into());
    }
    let file = File::open(output).map_err(|_| "媒体文件无法解码。")?;
    let size = file.metadata().map_err(|error| error.to_string())?.len();
    // FFmpeg may exit successfully when -fs stops a conversion early.
    if size >= CONVERSION_LIMIT {
        return Err("媒体转换结果超过大小限制。".into());
    }
    if size == 0 {
        return Err("媒体文件无法解码。".into());
    }
    Ok((file, size))
}

pub(crate) fn video_jpeg(file: &File, path: &Path, edge: u32) -> Result<Vec<u8>, String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let demuxer = if extension.eq_ignore_ascii_case("mkv") || extension.eq_ignore_ascii_case("webm")
    {
        "matroska"
    } else {
        "mov"
    };
    let input = Input::Video {
        file: file.try_clone().map_err(|error| error.to_string())?,
        demuxer,
    };
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    // Accurate rounding keeps colors intact when swscale converts to full-range JPEG.
    let scale = format!(
        "scale=w='min(iw,{edge})':h='min(ih,{edge})':force_original_aspect_ratio=decrease:reset_sar=1:flags=bicubic+accurate_rnd"
    );
    let (jpeg, size) = convert(
        input,
        &directory.path().join("preview.jpg"),
        &[
            "-map",
            "0:V:0",
            "-frames:v",
            "1",
            "-vf",
            &scale,
            "-c:v",
            "mjpeg",
            "-q:v",
            "3",
            "-f",
            "image2",
            "-update",
            "1",
        ],
        None,
    )
    .map_err(|error| format!("无法提取视频画面。{error}"))?;
    let mut bytes = Vec::with_capacity(size as usize);
    jpeg.take(CONVERSION_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn wait(child: &mut Child, cancelled: Option<&AtomicBool>) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + CONVERSION_TIMEOUT;
    let error = loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            break CANCELLED.to_owned();
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) => break "媒体解码超过 30 秒。".into(),
            Err(error) => break format!("媒体解码异常结束。{error}"),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    Err(error)
}

pub(crate) fn heif_jpeg(
    input: &Path,
    output: &Path,
    cancelled: Option<&AtomicBool>,
) -> Result<(File, u64), String> {
    // The MOV demuxer also accepts videos. Require a still-image source here.
    if !MediaSource::open(input).is_ok_and(|source| source.kind() == MediaKind::Image) {
        return Err("无法识别 HEIC/HEIF 照片。".into());
    }
    let _slot = loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(CANCELLED.into());
        }
        match HEIF_DECODER.try_lock() {
            Ok(guard) => break guard,
            Err(TryLockError::WouldBlock) => thread::sleep(POLL_INTERVAL),
            Err(TryLockError::Poisoned(_)) => return Err("无法使用 HEIC/HEIF 解码器。".into()),
        }
    };
    // FFmpeg's automatic stream selection assembles the primary HEIF grid.
    // Resize afterwards because the grid already uses a complex filtergraph.
    convert(
        Input::Mov(input),
        output,
        &[
            "-xerror",
            "-an",
            "-frames:v",
            "1",
            "-c:v",
            "mjpeg",
            "-q:v",
            "3",
            "-f",
            "image2",
            "-update",
            "1",
        ],
        cancelled,
    )
    .map_err(|error| format!("无法解码 HEIC/HEIF 照片。{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locates_sidecars_with_both_cargo_test_layouts() {
        for (program, expected) in [
            (
                "target/debug/deps/photosys_lib-hash",
                "target/debug/photosys-ffmpeg",
            ),
            (
                "target/debug/build/photosys/hash/out/photosys_lib-hash",
                "target/debug/photosys-ffmpeg",
            ),
            (
                "target/release/build/photosys/hash/out/photosys_lib-hash",
                "target/release/photosys-ffmpeg",
            ),
            (
                "target/coverage/deps/photosys_lib-hash",
                "target/coverage/photosys-ffmpeg",
            ),
            ("app/PhotoSys", "app/photosys-ffmpeg"),
        ] {
            assert_eq!(
                companion_for(Path::new(program), "photosys-ffmpeg"),
                Path::new(expected)
            );
        }
        assert_eq!(
            companion_for(
                Path::new("target/debug/deps/photosys_lib.exe"),
                "photosys-ffmpeg.exe"
            ),
            Path::new("target/debug/photosys-ffmpeg.exe")
        );
    }

    #[test]
    #[cfg(unix)]
    fn cancels_and_reaps_an_active_decoder() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let cancelled = AtomicBool::new(true);
        assert_eq!(wait(&mut child, Some(&cancelled)).unwrap_err(), CANCELLED);
        assert!(!child.try_wait().unwrap().unwrap().success());
    }
}
