use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use nom_exif::MediaParser;
use serde::Serialize;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use crate::media_date::{self, Media};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const COPY_BUFFER_SIZE: usize = 256 * 1024;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "phase",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ArchiveProgress {
    Scanning {
        found: u64,
    },
    Archiving {
        processed: u64,
        total: u64,
        current_file: String,
        transfer: Option<CopyProgress>,
    },
    Reporting {
        processed: u64,
        total: u64,
    },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyProgress {
    bytes_written: u64,
    total_bytes: u64,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveSummary {
    found: u64,
    copied: u64,
    duplicates: u64,
    renamed: u64,
    other: u64,
    failed: u64,
    scan_errors: u64,
    report_path: Option<String>,
    report_error: Option<String>,
}

struct ArchiveRun {
    summary: ArchiveSummary,
    copied_dates: BTreeSet<NaiveDate>,
    renames: Vec<(PathBuf, PathBuf)>,
    errors: Vec<(PathBuf, String)>,
}

pub fn run(
    source: &Path,
    destination: &Path,
    mut on_progress: impl FnMut(ArchiveProgress),
) -> Result<ArchiveSummary, String> {
    let source = checked_directory(source, "源目录")?;
    let destination = checked_directory(destination, "目标目录")?;
    if source == destination {
        return Err("源目录和目标目录相同，请重新选择。".into());
    }
    if source.starts_with(&destination) {
        return Err("源目录位于目标目录内，请重新选择。".into());
    }

    let started = Local::now();
    let mut job = ArchiveRun {
        summary: ArchiveSummary::default(),
        copied_dates: BTreeSet::new(),
        renames: Vec::new(),
        errors: Vec::new(),
    };
    on_progress(ArchiveProgress::Scanning { found: 0 });
    let mut last_update = Instant::now();
    let mut progress_due = |phase_changed| {
        let now = Instant::now();
        if phase_changed || now.duration_since(last_update) >= PROGRESS_INTERVAL {
            last_update = now;
            true
        } else {
            false
        }
    };

    let mut candidates = Vec::new();
    let entries = WalkDir::new(&source)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            entry.path() != destination && crate::library::include_in_scan(entry)
        });

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path = error.path().unwrap_or(&source);
                job.summary.scan_errors += 1;
                job.errors.push((path.to_path_buf(), error.to_string()));
                continue;
            }
        };
        if entry.file_type().is_file()
            && let Some(media) = Media::from_path(entry.path())
        {
            candidates.push((entry.into_path(), media));
            if progress_due(false) {
                on_progress(ArchiveProgress::Scanning {
                    found: candidates.len() as u64,
                });
            }
        }
    }

    let total = candidates.len() as u64;
    job.summary.found = total;
    let mut parser = MediaParser::new();
    for (index, (path, media)) in candidates.into_iter().enumerate() {
        let processed = index as u64;
        let mut report_file = |phase_changed, transfer| {
            if progress_due(phase_changed) {
                on_progress(ArchiveProgress::Archiving {
                    processed,
                    total,
                    current_file: path.file_name().unwrap().to_string_lossy().into_owned(),
                    transfer,
                });
            }
        };
        report_file(index == 0, None);
        job.archive_file(&mut parser, &path, media, &destination, |transfer| {
            report_file(false, transfer);
        });
    }

    on_progress(ArchiveProgress::Reporting {
        processed: job.summary.copied + job.summary.duplicates + job.summary.failed,
        total,
    });
    let finished = Local::now();
    let report_name = format!("复制说明_{}.txt", started.format("%Y%m%d_%H%M%S"));
    let started = started.format("%Y-%m-%d %H:%M:%S %:z");
    let finished = finished.format("%Y-%m-%d %H:%M:%S %:z");
    let report = (|| {
        let mut file = NamedTempFile::new_in(&destination)?;
        writeln!(file, "PhotoSys 复制说明")?;
        writeln!(file, "开始时间：{started}\n结束时间：{finished}")?;
        writeln!(
            file,
            "源目录：{}\n目标目录：{}",
            source.display(),
            destination.display()
        )?;
        job.write_report(&mut file)?;
        file.as_file().sync_all()?;
        let mut number = 0;
        loop {
            let path = destination.join(numbered_name(OsStr::new(&report_name), number));
            match file.persist_noclobber(&path) {
                Ok(_) => return Ok(path),
                Err(error) if name_is_occupied(&path, &error.error) => {
                    file = error.file;
                    number += 1;
                }
                Err(error) => return Err(error.error),
            }
        }
    })();

    match report {
        Ok(path) => job.summary.report_path = Some(path.display().to_string()),
        Err(error) => job.summary.report_error = Some(error.to_string()),
    }
    Ok(job.summary)
}

impl ArchiveRun {
    fn archive_file(
        &mut self,
        parser: &mut MediaParser,
        source: &Path,
        media: Media,
        destination: &Path,
        on_progress: impl FnMut(Option<CopyProgress>),
    ) {
        let date = media_date::from_filename(source)
            .or_else(|| media_date::from_metadata(parser, source, media));

        let result = (|| {
            let directory = match date {
                Some(date) => {
                    let year = ensure_subdirectory(destination, &date.format("%Y").to_string())?;
                    ensure_subdirectory(&year, &date.format("%Y-%m").to_string())?
                }
                None => ensure_subdirectory(destination, "其它")?,
            };
            copy_media(source, &directory, on_progress)
        })();

        match result {
            Ok(None) => self.summary.duplicates += 1,
            Ok(Some((path, renamed))) => {
                self.summary.copied += 1;
                self.summary.other += u64::from(date.is_none());
                if let Some(date) = date {
                    self.copied_dates.insert(date);
                }
                if renamed {
                    self.summary.renamed += 1;
                    self.renames.push((source.to_path_buf(), path));
                }
            }
            Err(error) => {
                self.summary.failed += 1;
                self.errors.push((source.to_path_buf(), error.to_string()));
            }
        }
    }

    fn write_report(&self, mut writer: impl Write) -> io::Result<()> {
        let summary = &self.summary;
        writeln!(writer, "\n发现媒体：{}", summary.found)?;
        writeln!(writer, "成功复制：{}", summary.copied)?;
        writeln!(writer, "重复跳过：{}", summary.duplicates)?;
        writeln!(writer, "重命名：{}", summary.renamed)?;
        writeln!(writer, "归入其它：{}", summary.other)?;
        writeln!(writer, "复制失败：{}", summary.failed)?;
        writeln!(writer, "扫描错误：{}", summary.scan_errors)?;

        writeln!(writer, "\n成功复制媒体日期段：")?;
        if self.copied_dates.is_empty() {
            writeln!(writer, "无可用日期")?;
        }
        let mut dates = self.copied_dates.iter().copied().peekable();
        while let Some(start) = dates.next() {
            let mut end = start;
            while let Some(&next) = dates.peek() {
                if end.succ_opt() != Some(next) {
                    break;
                }
                end = next;
                dates.next();
            }
            writeln!(
                writer,
                "{}-{}",
                start.format("%Y-%-m-%-d"),
                end.format("%Y-%-m-%-d")
            )?;
        }

        if !self.renames.is_empty() {
            writeln!(writer, "\n重命名明细")?;
            for (source, destination) in &self.renames {
                writeln!(writer, "{} → {}", source.display(), destination.display())?;
            }
        }
        if !self.errors.is_empty() {
            writeln!(writer, "\n失败明细")?;
            for (path, error) in &self.errors {
                writeln!(writer, "{}：{error}", path.display())?;
            }
        }
        Ok(())
    }
}

fn checked_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("无法访问{label} {}。{error}", path.display()))?;
    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err(format!("请选择有效的{label}。"))
    }
}

fn ensure_subdirectory(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(&path)?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} 已存在且无法用作归档目录。", path.display()),
                ));
            }
        }
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("{}：{error}", path.display()),
            ));
        }
    }
    Ok(path)
}

fn copy_media(
    source: &Path,
    directory: &Path,
    mut on_progress: impl FnMut(Option<CopyProgress>),
) -> io::Result<Option<(PathBuf, bool)>> {
    let name = source.file_name().unwrap();
    let size = source.metadata()?.len();
    let mut pending = None;
    loop {
        let Some(number) = available_number(source, directory, name, size, &mut || {
            on_progress(None);
        })?
        else {
            return Ok(None);
        };
        let path = directory.join(numbered_name(name, number));
        let file = match pending.take() {
            Some(file) => file,
            None => {
                let mut file = NamedTempFile::new_in(directory)?;
                let mut input = File::open(source)?;
                let mut buffer = vec![0; COPY_BUFFER_SIZE];
                let mut bytes_written = 0;
                loop {
                    let count = match input.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => count,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error),
                    };
                    file.write_all(&buffer[..count])?;
                    bytes_written += count as u64;
                    on_progress(Some(CopyProgress {
                        bytes_written,
                        total_bytes: size,
                    }));
                }
                file.as_file().sync_all()?;
                file
            }
        };
        match file.persist_noclobber(&path) {
            Ok(_) => return Ok(Some((path, number != 0))),
            Err(error) if name_is_occupied(&path, &error.error) => pending = Some(error.file),
            Err(error) => return Err(error.error),
        }
    }
}

fn available_number(
    source: &Path,
    directory: &Path,
    name: &OsStr,
    size: u64,
    on_compare: &mut impl FnMut(),
) -> io::Result<Option<u64>> {
    let original = directory.join(name);
    match match_file(source, &original, size, on_compare)? {
        Candidate::Missing => return Ok(Some(0)),
        Candidate::Identical => return Ok(None),
        Candidate::Occupied => {}
    }

    // Check existing suffixes beyond gaps, so deleting one numbered copy doesn't create duplicates.
    let mut suffixes = BTreeSet::new();
    let extension = Path::new(name).extension().unwrap().as_encoded_bytes();
    for entry in fs::read_dir(directory)? {
        let file_name = entry?.file_name();
        let path = Path::new(&file_name);
        if !path
            .extension()
            .is_some_and(|ext| ext.as_encoded_bytes().eq_ignore_ascii_case(extension))
        {
            continue;
        }
        let stem = path.file_stem().unwrap().as_encoded_bytes();
        let Some(separator) = stem.iter().rposition(|&byte| byte == b'_') else {
            continue;
        };
        let suffix = &stem[separator + 1..];
        if suffix
            .first()
            .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
            && let Ok(suffix) = std::str::from_utf8(suffix)
            && let Ok(number) = suffix.parse::<u64>()
        {
            suffixes.insert(number);
        }
    }

    let mut available = 1;
    for number in suffixes {
        let path = directory.join(numbered_name(name, number));
        match match_file(source, &path, size, on_compare)? {
            Candidate::Identical => return Ok(None),
            Candidate::Occupied if number == available => available += 1,
            _ => {}
        }
    }
    Ok(Some(available))
}

enum Candidate {
    Missing,
    Identical,
    Occupied,
}

fn match_file(
    source: &Path,
    candidate: &Path,
    size: u64,
    on_compare: &mut impl FnMut(),
) -> io::Result<Candidate> {
    let metadata = match fs::symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Candidate::Missing),
        Err(error) => return Err(error),
    };
    if metadata.is_file()
        && metadata.len() == size
        && same_bytes(
            &mut File::open(source)?,
            &mut File::open(candidate)?,
            size,
            on_compare,
        )?
    {
        return Ok(Candidate::Identical);
    }
    Ok(Candidate::Occupied)
}

fn same_bytes(
    first: &mut File,
    second: &mut File,
    mut remaining: u64,
    on_compare: &mut impl FnMut(),
) -> io::Result<bool> {
    let mut left = [0; 64 * 1024];
    let mut right = [0; 64 * 1024];
    while remaining > 0 {
        let length = remaining.min(left.len() as u64) as usize;
        first.read_exact(&mut left[..length])?;
        second.read_exact(&mut right[..length])?;
        if left[..length] != right[..length] {
            return Ok(false);
        }
        remaining -= length as u64;
        on_compare();
    }
    Ok(first.read(&mut left[..1])? == 0 && second.read(&mut right[..1])? == 0)
}

fn name_is_occupied(path: &Path, error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::AlreadyExists
        // Windows can report PermissionDenied when a directory occupies the name.
        || (error.kind() == io::ErrorKind::PermissionDenied && fs::symlink_metadata(path).is_ok())
}

fn numbered_name(name: &OsStr, number: u64) -> OsString {
    if number == 0 {
        return name.to_os_string();
    }

    let path = Path::new(name);
    let mut numbered = path.file_stem().unwrap().to_os_string();
    numbered.push(format!("_{number}"));
    if let Some(extension) = path.extension() {
        numbered.push(".");
        numbered.push(extension);
    }
    numbered
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
