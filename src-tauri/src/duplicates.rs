mod platform;

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use platform::FileIdentity;

pub(crate) use platform::is_link;
#[cfg(windows)]
pub(crate) use platform::shell_path;
pub(crate) use platform::trash;

const READ_BUFFER: usize = 64 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
type Checksum = [u8; 32];

#[derive(Serialize)]
#[serde(
    tag = "phase",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum ScanProgress {
    Scanning {
        found: usize,
    },
    Hashing {
        checked: usize,
        total: usize,
        current_file: String,
    },
    Comparing {
        checked: usize,
        total: usize,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanResult {
    scan_id: u64,
    scanned: usize,
    groups: Vec<GroupView>,
    issues: Vec<ScanIssue>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanIssue {
    path: String,
    reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupView {
    id: usize,
    size: u64,
    needs_rescan: bool,
    files: Vec<FileView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileView {
    id: usize,
    name: String,
    relative_path: String,
    modified_at: String,
    hard_link: bool,
}

#[derive(Debug)]
pub(crate) struct Candidate {
    pub(crate) id: usize,
    pub(crate) relative: PathBuf,
    pub(crate) stamp: FileStamp,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FileStamp {
    identity: FileIdentity,
    links: u64,
    pub(crate) size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
}

impl FileStamp {
    pub(crate) fn hard_link(&self) -> bool {
        self.links > 1
    }

    pub(crate) fn read(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if platform::is_link(&metadata) || !metadata.is_file() {
            return Err(changed());
        }
        let (identity, links) = platform::identity(file, &metadata)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Ok(Self {
            identity,
            links,
            size: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }

    pub(crate) fn cache_key(&self) -> Vec<u8> {
        let modified = DateTime::<Utc>::from(self.modified);
        let mut key = Vec::with_capacity(60);
        for field in [self.identity.0, self.identity.1, self.links, self.size] {
            key.extend_from_slice(&field.to_le_bytes());
        }
        key.extend_from_slice(&modified.timestamp().to_le_bytes());
        key.extend_from_slice(&modified.timestamp_subsec_nanos().to_le_bytes());
        #[cfg(unix)]
        {
            key.extend_from_slice(&self.changed.0.to_le_bytes());
            key.extend_from_slice(&self.changed.1.to_le_bytes());
        }
        key
    }

    /// Portable index stamp: relative paths plus length/mtime survive copying an album.
    pub(crate) fn portable_key(&self) -> Vec<u8> {
        let modified = DateTime::<Utc>::from(self.modified);
        let mut key = self.size.to_le_bytes().to_vec();
        key.extend_from_slice(&modified.timestamp().to_le_bytes());
        key.extend_from_slice(&modified.timestamp_subsec_nanos().to_le_bytes());
        key
    }
}

impl Candidate {
    pub(crate) fn path_label(&self) -> String {
        display_path(&self.relative)
    }

    fn view(&self) -> FileView {
        FileView {
            id: self.id,
            name: display_path(Path::new(self.relative.file_name().unwrap())),
            relative_path: self.path_label(),
            modified_at: DateTime::<Utc>::from(self.stamp.modified).to_rfc3339(),
            hard_link: self.stamp.links > 1,
        }
    }
}

pub(crate) struct Root {
    pub(crate) path: PathBuf,
    identity: FileIdentity,
}

impl Root {
    pub(crate) fn new(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let identity = directory_identity(&path)?;
        Ok(Self { path, identity })
    }

    pub(crate) fn verify_directory(&self) -> io::Result<()> {
        if self.path.canonicalize()? != self.path
            || directory_identity(&self.path)? != self.identity
        {
            return Err(io::Error::other("扫描目录已变化，请重新扫描。"));
        }
        Ok(())
    }

    pub(crate) fn open_file(&self, relative: &Path) -> io::Result<File> {
        self.verify_directory()?;
        let mut path = self.path.clone();
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(changed());
            };
            path.push(name);
            let metadata = fs::symlink_metadata(&path)?;
            if platform::is_link(&metadata)
                || if components.peek().is_some() {
                    !metadata.is_dir()
                } else {
                    !metadata.is_file()
                }
            {
                return Err(changed());
            }
        }
        platform::open(&path)
    }

    pub(crate) fn open_matching(&self, candidate: &Candidate) -> io::Result<File> {
        let file = self.open_file(&candidate.relative)?;
        if FileStamp::read(&file)? != candidate.stamp {
            return Err(changed());
        }
        Ok(file)
    }

    pub(crate) fn verify_read(&self, file: &File, candidate: &Candidate) -> io::Result<()> {
        if FileStamp::read(file)? != candidate.stamp {
            return Err(changed());
        }
        self.open_matching(candidate)?;
        Ok(())
    }

    fn checksum(&self, candidate: &Candidate) -> io::Result<Checksum> {
        let mut file = self.open_matching(candidate)?;
        let mut remaining = candidate.stamp.size;
        let mut buffer = [0; READ_BUFFER];
        let mut hash = Sha256::new();
        while remaining > 0 {
            let length = remaining.min(READ_BUFFER as u64) as usize;
            file.read_exact(&mut buffer[..length])?;
            hash.update(&buffer[..length]);
            remaining -= length as u64;
        }
        check_eof(&mut file)?;
        self.verify_read(&file, candidate)?;
        Ok(hash.finalize().into())
    }

    fn compare(
        &self,
        left: &Candidate,
        right: &Candidate,
        expected: Option<&Checksum>,
    ) -> Result<bool, CompareError> {
        let mut left_file = self.open_matching(left).map_err(CompareError::Left)?;
        let mut right_file = self.open_matching(right).map_err(CompareError::Right)?;
        let mut left_buffer = [0; READ_BUFFER];
        let mut right_buffer = [0; READ_BUFFER];
        let mut remaining = left.stamp.size;
        let mut equal = true;
        let mut hash = expected.map(|_| Sha256::new());
        while remaining > 0 {
            let length = remaining.min(READ_BUFFER as u64) as usize;
            let left_bytes = &mut left_buffer[..length];
            let right_bytes = &mut right_buffer[..length];
            left_file
                .read_exact(left_bytes)
                .map_err(CompareError::Left)?;
            right_file
                .read_exact(right_bytes)
                .map_err(CompareError::Right)?;
            if let Some(hash) = &mut hash {
                hash.update(&*left_bytes);
            }
            if left_bytes != right_bytes {
                equal = false;
                if expected.is_none() {
                    break;
                }
            }
            remaining -= length as u64;
        }
        if equal {
            check_eof(&mut left_file).map_err(CompareError::Left)?;
            check_eof(&mut right_file).map_err(CompareError::Right)?;
        }
        self.verify_read(&left_file, left)
            .map_err(CompareError::Left)?;
        self.verify_read(&right_file, right)
            .map_err(CompareError::Right)?;
        if let Some((hash, expected)) = hash.zip(expected) {
            let actual: Checksum = hash.finalize().into();
            if actual != *expected {
                return Err(CompareError::Left(changed()));
            }
        }
        Ok(equal)
    }
}

fn directory_identity(path: &Path) -> io::Result<FileIdentity> {
    let file = platform::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || platform::is_link(&metadata) {
        return Err(io::Error::other("请选择有效的扫描目录。"));
    }
    Ok(platform::identity(&file, &metadata)?.0)
}

fn changed() -> io::Error {
    io::Error::other("文件已变化或已不在扫描目录内。")
}

fn check_eof(file: &mut File) -> io::Result<()> {
    match file.read_exact(&mut [0]) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
        Err(error) => Err(error),
        Ok(()) => Err(changed()),
    }
}

pub(crate) fn display_path(path: &Path) -> String {
    match path.to_str() {
        Some(path) => path.into(),
        None => format!("{:?}", path.as_os_str()),
    }
}

enum CompareError {
    Left(io::Error),
    Right(io::Error),
}

#[derive(Debug)]
struct Group {
    id: usize,
    files: Vec<Candidate>,
    checksum: Checksum,
    needs_rescan: bool,
}

pub(crate) struct Scan {
    id: u64,
    root: Root,
    groups: Vec<Group>,
}

impl Scan {
    fn group_views(&self) -> Vec<GroupView> {
        self.groups
            .iter()
            .map(|group| GroupView {
                id: group.id,
                size: group.files[0].stamp.size,
                needs_rescan: group.needs_rescan,
                files: group.files.iter().map(Candidate::view).collect(),
            })
            .collect()
    }
}

fn issue(issues: &mut Vec<ScanIssue>, path: String, error: impl std::fmt::Display) {
    issues.push(ScanIssue {
        path,
        reason: error.to_string(),
    });
}

pub(crate) fn scan(
    id: u64,
    path: &Path,
    mut progress: impl FnMut(ScanProgress),
) -> Result<(Scan, ScanResult), String> {
    let root =
        Root::new(path).map_err(|error| format!("无法访问扫描目录 {}。{error}", path.display()))?;
    let mut by_size = BTreeMap::<u64, Vec<Candidate>>::new();
    let mut issues = Vec::new();
    let mut found = 0;
    let mut last_update = Instant::now();
    let mut due = |force| {
        if force || last_update.elapsed() >= PROGRESS_INTERVAL {
            last_update = Instant::now();
            true
        } else {
            false
        }
    };

    progress(ScanProgress::Scanning { found });
    let mut entries = WalkDir::new(&root.path)
        .follow_links(false)
        .min_depth(1)
        .into_iter();
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path = error
                    .path()
                    .and_then(|path| path.strip_prefix(&root.path).ok());
                issue(
                    &mut issues,
                    path.map_or_else(|| ".".into(), display_path),
                    error,
                );
                continue;
            }
        };
        if !crate::library::include_in_scan(&entry) {
            if entry.file_type().is_dir() {
                entries.skip_current_dir();
            }
            continue;
        }
        let relative = entry.path().strip_prefix(&root.path).unwrap();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                if entry.file_type().is_dir() {
                    entries.skip_current_dir();
                }
                issue(&mut issues, display_path(relative), error);
                continue;
            }
        };
        if platform::is_link(&metadata) {
            if entry.file_type().is_dir() {
                entries.skip_current_dir();
            }
            continue;
        }
        if metadata.is_file() {
            found += 1;
            match root
                .open_file(relative)
                .and_then(|file| FileStamp::read(&file))
            {
                Ok(stamp) => by_size.entry(stamp.size).or_default().push(Candidate {
                    id: found,
                    relative: relative.to_path_buf(),
                    stamp,
                }),
                Err(error) => issue(&mut issues, display_path(relative), error),
            }
        }
        if due(false) {
            progress(ScanProgress::Scanning { found });
        }
    }
    progress(ScanProgress::Scanning { found });

    let total = by_size.values().map(Vec::len).sum();
    let mut checked = 0;
    let mut buckets = BTreeMap::<(u64, Checksum), Vec<Candidate>>::new();
    for (size, files) in by_size {
        let needs_hash = files.len() > 1;
        for file in files {
            if due(checked == 0) {
                progress(ScanProgress::Hashing {
                    checked,
                    total,
                    current_file: file.path_label(),
                });
            }
            if needs_hash {
                match root.checksum(&file) {
                    Ok(hash) => buckets.entry((size, hash)).or_default().push(file),
                    Err(error) => issue(&mut issues, file.path_label(), error),
                }
            } else if let Err(error) = root.open_matching(&file) {
                issue(&mut issues, file.path_label(), error);
            }
            checked += 1;
        }
    }

    let total = buckets.values().map(Vec::len).sum();
    let mut checked = 0;
    progress(ScanProgress::Comparing { checked, total });
    let mut groups = Vec::new();
    for ((_, checksum), files) in buckets {
        let mut partitions = Vec::<Vec<Candidate>>::new();
        for file in files {
            if let Err((file, error)) = insert_by_content(&root, &mut partitions, file, &mut issues)
            {
                issue(&mut issues, file.path_label(), error);
            }
            checked += 1;
            if due(false) {
                progress(ScanProgress::Comparing { checked, total });
            }
        }
        for mut files in partitions {
            files.retain(|file| match root.open_matching(file) {
                Ok(_) => true,
                Err(error) => {
                    issue(&mut issues, file.path_label(), error);
                    false
                }
            });
            if files.len() > 1 {
                files.sort_by(|left, right| {
                    left.relative
                        .components()
                        .count()
                        .cmp(&right.relative.components().count())
                        .then_with(|| left.relative.cmp(&right.relative))
                });
                groups.push(Group {
                    id: files[0].id,
                    files,
                    checksum,
                    needs_rescan: false,
                });
            }
        }
    }
    groups.sort_by(|left, right| {
        right.files[0]
            .stamp
            .size
            .cmp(&left.files[0].stamp.size)
            .then_with(|| left.files[0].relative.cmp(&right.files[0].relative))
    });

    let scan = Scan { id, root, groups };
    let result = ScanResult {
        scan_id: id,
        scanned: found,
        groups: scan.group_views(),
        issues,
    };
    Ok((scan, result))
}

fn insert_by_content(
    root: &Root,
    partitions: &mut Vec<Vec<Candidate>>,
    file: Candidate,
    issues: &mut Vec<ScanIssue>,
) -> Result<(), (Candidate, io::Error)> {
    for matches in partitions.iter_mut() {
        while let Some(representative) = matches.last() {
            match root.compare(representative, &file, None) {
                Ok(true) => {
                    matches.push(file);
                    return Ok(());
                }
                Ok(false) => break,
                Err(CompareError::Left(error)) => {
                    issue(issues, representative.path_label(), error);
                    matches.pop();
                }
                Err(CompareError::Right(error)) => return Err((file, error)),
            }
        }
    }
    if let Some(empty) = partitions.iter_mut().find(|files| files.is_empty()) {
        empty.push(file);
    } else {
        partitions.push(vec![file]);
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum TrashStatus {
    Moved,
    Skipped,
    Failed,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrashOutcome {
    id: usize,
    relative_path: String,
    status: TrashStatus,
    reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrashResult {
    groups: Vec<GroupView>,
    outcomes: Vec<TrashOutcome>,
}

impl Scan {
    pub(crate) fn recycle(
        &mut self,
        scan_id: u64,
        file_ids: Vec<usize>,
        mut trash: impl FnMut(&Path) -> Result<(), String>,
    ) -> Result<TrashResult, String> {
        if scan_id != self.id {
            return Err("扫描结果已过期，请重新扫描。".into());
        }
        let selected: HashSet<_> = file_ids.into_iter().collect();
        if selected.is_empty() {
            return Err("请先勾选要移到回收站的副本。".into());
        }
        let mut found = 0;
        for group in &self.groups {
            let mut selected_count = 0;
            for file in &group.files {
                if selected.contains(&file.id) {
                    if file.stamp.links > 1 || group.needs_rescan {
                        return Err("部分文件无法清理，请检查勾选项或重新扫描。".into());
                    }
                    selected_count += 1;
                }
            }
            if selected_count == group.files.len() {
                return Err("每组重复文件至少需要保留一份。".into());
            }
            found += selected_count;
        }
        if found != selected.len() {
            return Err("部分文件不在当前扫描结果中，请重新选择。".into());
        }

        let mut outcomes = Vec::new();
        for group in &mut self.groups {
            let Some(keep) = group.files.iter().find(|file| !selected.contains(&file.id)) else {
                continue;
            };
            let mut moved = HashSet::new();
            let mut keep_error: Option<String> = None;
            for target in group
                .files
                .iter()
                .filter(|file| selected.contains(&file.id))
            {
                let (status, reason) = if let Some(reason) = &keep_error {
                    (TrashStatus::Skipped, Some(reason.clone()))
                } else {
                    match self.root.check_before_trash(keep, target, &group.checksum) {
                        Ok(()) => match trash(&self.root.path.join(&target.relative)) {
                            Ok(()) => {
                                moved.insert(target.id);
                                (TrashStatus::Moved, None)
                            }
                            Err(error) => (TrashStatus::Failed, Some(error)),
                        },
                        Err(CompareError::Left(error)) => {
                            let reason = format!(
                                "保留文件 {} 已失效，请重新扫描。{error}",
                                keep.path_label()
                            );
                            keep_error = Some(reason.clone());
                            group.needs_rescan = true;
                            (TrashStatus::Skipped, Some(reason))
                        }
                        Err(CompareError::Right(error)) => {
                            group.needs_rescan = true;
                            (
                                TrashStatus::Skipped,
                                Some(format!("无法确认该副本，请重新扫描。{error}")),
                            )
                        }
                    }
                };
                outcomes.push(TrashOutcome {
                    id: target.id,
                    relative_path: target.path_label(),
                    status,
                    reason,
                });
            }
            group.files.retain(|file| !moved.contains(&file.id));
        }
        self.groups.retain(|group| group.files.len() > 1);
        Ok(TrashResult {
            groups: self.group_views(),
            outcomes,
        })
    }
}

impl Root {
    fn check_before_trash(
        &self,
        keep: &Candidate,
        target: &Candidate,
        checksum: &Checksum,
    ) -> Result<(), CompareError> {
        if !self.compare(keep, target, Some(checksum))? {
            return Err(CompareError::Right(changed()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
