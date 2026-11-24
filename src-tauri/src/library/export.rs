use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Seek};
use std::path::Path;

use serde::Serialize;
use tempfile::NamedTempFile;

use super::{Scan, scan};
use crate::archive::{
    NameMatch, available_numbered, name_is_occupied, numbered_name, numbered_suffix, same_bytes,
};
use crate::duplicates::{Candidate, FileStamp, Root, is_link};

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ExportSummary {
    succeeded: usize,
    skipped: usize,
    failed: usize,
}

struct Destination {
    root: Root,
    numbered: HashMap<String, BTreeSet<u64>>,
}

impl Destination {
    fn open(path: &Path) -> io::Result<Self> {
        let mut destination = Self {
            root: Root::new(path)?,
            numbered: HashMap::new(),
        };
        for entry in fs::read_dir(&destination.root.path)? {
            destination.remember_numbered(&entry?.file_name());
        }
        Ok(destination)
    }

    fn remember_numbered(&mut self, name: &OsStr) {
        let path = Path::new(name);
        if let (Some(stem), Some(extension)) = (path.file_stem(), path.extension())
            && let Some((base, number)) = numbered_suffix(&stem.to_string_lossy())
        {
            let key = numbered_key(base, extension);
            self.numbered.entry(key).or_default().insert(number);
        }
    }

    fn available_name(
        &self,
        name: &OsStr,
        source: &mut File,
        size: u64,
    ) -> io::Result<Option<u64>> {
        let original = self.matches(name, source, size)?;
        if matches!(original, NameMatch::Identical) {
            return Ok(None);
        }

        let path = Path::new(name);
        let key = numbered_key(
            &path.file_stem().expect("media file stem").to_string_lossy(),
            path.extension().expect("media file extension"),
        );
        let numbers = self.numbered.get(&key).into_iter().flatten().copied();
        let next = available_numbered(numbers, |number| {
            self.matches(&numbered_name(name, number), source, size)
        })?;
        Ok(next.map(|number| {
            if matches!(original, NameMatch::Missing) {
                0
            } else {
                number
            }
        }))
    }

    fn matches(&self, name: &OsStr, source: &mut File, size: u64) -> io::Result<NameMatch> {
        let metadata = match fs::symlink_metadata(self.root.path.join(name)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(NameMatch::Missing),
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || is_link(&metadata) || metadata.len() != size {
            return Ok(NameMatch::Occupied);
        }

        let relative = Path::new(name);
        let mut target = self.root.open_file(relative)?;
        let stamp = FileStamp::read(&target)?;
        source.rewind()?;
        if stamp.size != size || !same_bytes(source, &mut target, size, &mut || {})? {
            return Ok(NameMatch::Occupied);
        }
        self.root.verify_read(
            &target,
            &Candidate {
                id: 0,
                relative: relative.to_path_buf(),
                stamp,
            },
        )?;
        Ok(NameMatch::Identical)
    }
}

fn numbered_key(stem: &str, extension: &OsStr) -> String {
    // The index narrows the search. The filesystem decides which names actually match.
    format!("{stem}.{}", extension.to_string_lossy()).to_lowercase()
}

enum Outcome {
    Copied,
    Skipped,
}

fn export_file(
    source: &Root,
    item: &Candidate,
    destination: &mut Destination,
) -> io::Result<Outcome> {
    let name = item.relative.file_name().expect("media file name");
    let mut input = source.open_matching(item)?;
    let mut staged = None;
    loop {
        destination.root.verify_directory()?;
        let Some(number) = destination.available_name(name, &mut input, item.stamp.size)? else {
            source.verify_read(&input, item)?;
            return Ok(Outcome::Skipped);
        };
        let output_name = numbered_name(name, number);
        let output = destination.root.path.join(&output_name);
        let file = match staged.take() {
            Some(file) => file,
            None => {
                input.rewind()?;
                let mut file = NamedTempFile::new_in(&destination.root.path)?;
                if io::copy(&mut input, &mut file)? != item.stamp.size {
                    return Err(io::Error::other("原文件在导出期间已变化。"));
                }
                file.as_file().sync_all()?;
                file
            }
        };
        source.verify_read(&input, item)?;
        destination.root.verify_directory()?;
        match file.persist_noclobber(&output) {
            Ok(_) => {
                destination.remember_numbered(&output_name);
                return Ok(Outcome::Copied);
            }
            Err(error) if name_is_occupied(&output, &error.error) => {
                destination.remember_numbered(&output_name);
                staged = Some(error.file);
            }
            Err(error) => return Err(error.error),
        }
    }
}

fn export(scan: &Scan, file_ids: Vec<usize>, destination: &Path) -> Result<ExportSummary, String> {
    let mut destination =
        Destination::open(destination).map_err(|error| format!("无法打开导出目录。{error}"))?;
    let mut processed = HashSet::new();
    let mut summary = ExportSummary::default();
    for id in file_ids.into_iter().filter(|&id| processed.insert(id)) {
        let result = scan
            .item(id)
            .map(|item| export_file(&scan.root, &item.file, &mut destination));
        match result {
            Ok(Ok(Outcome::Copied)) => summary.succeeded += 1,
            Ok(Ok(Outcome::Skipped)) => summary.skipped += 1,
            _ => summary.failed += 1,
        }
    }
    Ok(summary)
}

fn export_current(
    scan_id: u64,
    file_ids: Vec<usize>,
    destination: &Path,
) -> Result<ExportSummary, String> {
    let scan = scan(scan_id)?;
    export(&scan, file_ids, destination)
}

#[tauri::command]
pub(crate) async fn export_library_originals(
    scan_id: u64,
    file_ids: Vec<usize>,
    destination: String,
) -> Result<ExportSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        export_current(scan_id, file_ids, Path::new(&destination))
    })
    .await
    .map_err(|error| format!("导出任务异常结束。{error}"))?
}

#[cfg(test)]
mod tests;
