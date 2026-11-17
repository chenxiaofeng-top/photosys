use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chrono::Local;
use nom_exif::MediaParser;
use serde::Serialize;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use crate::media_date::{self, Media};

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
    renames: Vec<(PathBuf, PathBuf)>,
    errors: Vec<(PathBuf, String)>,
}

pub fn run(source: &Path, destination: &Path) -> Result<ArchiveSummary, String> {
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
        renames: Vec::new(),
        errors: Vec::new(),
    };
    let mut parser = MediaParser::new();
    let entries = WalkDir::new(&source)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| entry.path() != destination);

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
            job.archive_file(&mut parser, entry.path(), media, &destination);
        }
    }

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
    ) {
        self.summary.found += 1;
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
            copy_media(source, &directory)
        })();

        match result {
            Ok(None) => self.summary.duplicates += 1,
            Ok(Some((path, renamed))) => {
                self.summary.copied += 1;
                self.summary.other += u64::from(date.is_none());
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

fn copy_media(source: &Path, directory: &Path) -> io::Result<Option<(PathBuf, bool)>> {
    let name = source.file_name().unwrap();
    let size = source.metadata()?.len();
    let mut pending = None;
    loop {
        let Some(number) = available_number(source, directory, name, size)? else {
            return Ok(None);
        };
        let path = directory.join(numbered_name(name, number));
        let file = match pending.take() {
            Some(file) => file,
            None => {
                let mut file = NamedTempFile::new_in(directory)?;
                io::copy(&mut File::open(source)?, &mut file)?;
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
) -> io::Result<Option<u64>> {
    let original = directory.join(name);
    match match_file(source, &original, size)? {
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
        match match_file(source, &path, size)? {
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

fn match_file(source: &Path, candidate: &Path, size: u64) -> io::Result<Candidate> {
    let metadata = match fs::symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Candidate::Missing),
        Err(error) => return Err(error),
    };
    if metadata.is_file()
        && metadata.len() == size
        && same_bytes(&mut File::open(source)?, &mut File::open(candidate)?, size)?
    {
        return Ok(Candidate::Identical);
    }
    Ok(Candidate::Occupied)
}

fn same_bytes(first: &mut File, second: &mut File, mut remaining: u64) -> io::Result<bool> {
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
