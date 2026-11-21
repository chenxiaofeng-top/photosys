mod platform;
pub(crate) mod session;

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanProgress {
    phase: &'static str,
    found: usize,
    checked: usize,
    total: usize,
    bytes_read: u64,
    current_file: String,
    elapsed_ms: u64,
}

struct Reporter<'a> {
    update: ScanProgress,
    start: Instant,
    last: Instant,
    cancelled: &'a AtomicBool,
    send: &'a mut dyn FnMut(ScanProgress),
}

impl Reporter<'_> {
    fn tick(&mut self, bytes: usize, force: bool) -> io::Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "扫描已取消。"));
        }
        self.update.bytes_read += bytes as u64;
        if force || self.last.elapsed() >= PROGRESS_INTERVAL {
            self.last = Instant::now();
            self.update.elapsed_ms = self.start.elapsed().as_millis() as u64;
            (self.send)(self.update.clone());
        }
        // A callback may request cancellation during this update.
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "扫描已取消。"));
        }
        Ok(())
    }

    fn phase(&mut self, phase: &'static str, total: usize) -> io::Result<()> {
        self.update.phase = phase;
        self.update.checked = 0;
        self.update.total = total;
        self.update.current_file.clear();
        self.tick(0, true)
    }
}

#[cfg(test)]
struct ScanResult {
    scanned: usize,
    groups: Vec<GroupView>,
    issues: Vec<ScanIssue>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanIssue {
    path: String,
    reason: String,
}

#[cfg(test)]
struct GroupView {
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

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub(crate) id: usize,
    pub(crate) relative: PathBuf,
    pub(crate) stamp: FileStamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone)]
pub(crate) struct Root {
    pub(crate) path: PathBuf,
    identity: FileIdentity,
    #[cfg(test)]
    pub(crate) test_data_directory: Option<PathBuf>,
}

impl Root {
    pub(crate) fn new(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let identity = directory_identity(&path)?;
        Ok(Self {
            path,
            identity,
            #[cfg(test)]
            test_data_directory: None,
        })
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

    fn checksum(
        &self,
        candidate: &Candidate,
        tick: &mut dyn FnMut(usize) -> io::Result<()>,
    ) -> io::Result<Checksum> {
        let mut file = self.open_matching(candidate)?;
        let mut remaining = candidate.stamp.size;
        let mut buffer = [0; READ_BUFFER];
        let mut hash = Sha256::new();
        while remaining > 0 {
            tick(0)?;
            let length = remaining.min(READ_BUFFER as u64) as usize;
            file.read_exact(&mut buffer[..length])?;
            hash.update(&buffer[..length]);
            tick(length)?;
            remaining -= length as u64;
        }
        check_eof(&mut file)?;
        self.verify_read(&file, candidate)?;
        Ok(hash.finalize().into())
    }

    fn sample(
        &self,
        candidate: &Candidate,
        tick: &mut dyn FnMut(usize) -> io::Result<()>,
    ) -> io::Result<Checksum> {
        let mut file = self.open_matching(candidate)?;
        let mut buffer = [0; READ_BUFFER];
        let mut hash = Sha256::new();
        let size = candidate.stamp.size;
        for offset in [
            0,
            (size - READ_BUFFER as u64) / 2,
            size - READ_BUFFER as u64,
        ] {
            tick(0)?;
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut buffer)?;
            hash.update(buffer);
            tick(READ_BUFFER)?;
        }
        self.verify_read(&file, candidate)?;
        Ok(hash.finalize().into())
    }

    fn compare(
        &self,
        left: &Candidate,
        right: &Candidate,
        expected: Option<&Checksum>,
    ) -> Result<bool, CompareError> {
        self.compare_with(left, right, expected, &mut |_| Ok(()))
    }

    fn compare_with(
        &self,
        left: &Candidate,
        right: &Candidate,
        expected: Option<&Checksum>,
        tick: &mut dyn FnMut(usize) -> io::Result<()>,
    ) -> Result<bool, CompareError> {
        let mut left_file = self.open_matching(left).map_err(CompareError::Left)?;
        let mut right_file = self.open_matching(right).map_err(CompareError::Right)?;
        let mut left_buffer = [0; READ_BUFFER];
        let mut right_buffer = [0; READ_BUFFER];
        let mut remaining = left.stamp.size;
        let mut equal = true;
        let mut hash = expected.map(|_| Sha256::new());
        while remaining > 0 {
            tick(0).map_err(CompareError::Right)?;
            let length = remaining.min(READ_BUFFER as u64) as usize;
            let left_bytes = &mut left_buffer[..length];
            let right_bytes = &mut right_buffer[..length];
            left_file
                .read_exact(left_bytes)
                .map_err(CompareError::Left)?;
            right_file
                .read_exact(right_bytes)
                .map_err(CompareError::Right)?;
            tick(length * 2).map_err(CompareError::Right)?;
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
    issues: Vec<ScanIssue>,
    scanned: usize,
    bytes_read: u64,
    elapsed_ms: u64,
}

#[cfg(test)]
impl Scan {
    fn group_views(&self) -> Vec<GroupView> {
        self.groups
            .iter()
            .map(|group| GroupView {
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

#[cfg(test)]
fn scan(
    id: u64,
    path: &Path,
    progress: impl FnMut(ScanProgress),
) -> Result<(Scan, ScanResult), String> {
    let scan = run_scan(id, path, &AtomicBool::new(false), progress, true)?;
    let result = ScanResult {
        scanned: scan.scanned,
        groups: scan.group_views(),
        issues: scan.issues.clone(),
    };
    Ok((scan, result))
}

fn run_scan(
    id: u64,
    path: &Path,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(ScanProgress),
    sampling: bool,
) -> Result<Scan, String> {
    let root =
        Root::new(path).map_err(|error| format!("无法访问扫描目录 {}。{error}", path.display()))?;
    let mut by_size = BTreeMap::<u64, Vec<Candidate>>::new();
    let mut issues = Vec::new();
    let mut found = 0;
    let mut reporter = Reporter {
        update: ScanProgress {
            phase: "scanning",
            found: 0,
            checked: 0,
            total: 0,
            bytes_read: 0,
            current_file: String::new(),
            elapsed_ms: 0,
        },
        start: Instant::now(),
        last: Instant::now(),
        cancelled,
        send: &mut progress,
    };
    let report_error = |error: io::Error| error.to_string();
    reporter.phase("scanning", 0).map_err(report_error)?;
    let mut entries = WalkDir::new(&root.path)
        .follow_links(false)
        .min_depth(1)
        .into_iter();
    while let Some(entry) = entries.next() {
        reporter.tick(0, false).map_err(report_error)?;
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
        reporter.update.found = found;
    }
    reporter.update.found = found;
    reporter.tick(0, true).map_err(report_error)?;

    let total = by_size
        .values()
        .filter(|files| files.len() > 1)
        .map(Vec::len)
        .sum();
    reporter.phase("sampling", total).map_err(report_error)?;
    let mut sampled = Vec::new();
    for (size, files) in by_size {
        if files.len() < 2 {
            continue;
        }
        if !sampling || size <= (READ_BUFFER * 3) as u64 {
            reporter.update.checked += files.len();
            sampled.push((size, files));
            reporter.tick(0, false).map_err(report_error)?;
            continue;
        }
        let mut samples = BTreeMap::<Checksum, Vec<Candidate>>::new();
        for file in files {
            reporter.update.current_file = file.path_label();
            reporter.tick(0, false).map_err(report_error)?;
            match root.sample(&file, &mut |bytes| reporter.tick(bytes, false)) {
                Ok(hash) => samples.entry(hash).or_default().push(file),
                Err(error) => issue(&mut issues, file.path_label(), error),
            }
            reporter.update.checked += 1;
            reporter.tick(0, false).map_err(report_error)?;
        }
        sampled.extend(
            samples
                .into_values()
                .filter(|files| files.len() > 1)
                .map(|files| (size, files)),
        );
    }
    reporter.tick(0, true).map_err(report_error)?;
    reporter
        .phase(
            "hashing",
            sampled.iter().map(|(_, files)| files.len()).sum(),
        )
        .map_err(report_error)?;
    let mut buckets = BTreeMap::<(u64, Checksum), Vec<Candidate>>::new();
    for (size, files) in sampled {
        for file in files {
            reporter.update.current_file = file.path_label();
            reporter
                .tick(0, reporter.update.checked == 0)
                .map_err(report_error)?;
            match root.checksum(&file, &mut |bytes| reporter.tick(bytes, false)) {
                Ok(hash) => buckets.entry((size, hash)).or_default().push(file),
                Err(error) => issue(&mut issues, file.path_label(), error),
            }
            reporter.update.checked += 1;
            reporter.tick(0, false).map_err(report_error)?;
        }
    }
    reporter.tick(0, true).map_err(report_error)?;
    reporter
        .phase(
            "comparing",
            buckets
                .values()
                .filter(|files| files.len() > 1)
                .map(Vec::len)
                .sum(),
        )
        .map_err(report_error)?;
    let mut groups = Vec::new();
    for ((_, checksum), files) in buckets {
        if files.len() < 2 {
            continue;
        }
        let mut partitions = Vec::<Vec<Candidate>>::new();
        for file in files {
            reporter.update.current_file = file.path_label();
            reporter.tick(0, false).map_err(report_error)?;
            if let Err((file, error)) =
                insert_by_content(&root, &mut partitions, file, &mut issues, &mut |bytes| {
                    reporter.tick(bytes, false)
                })
            {
                issue(&mut issues, file.path_label(), error);
            }
            reporter.update.checked += 1;
            reporter.tick(0, false).map_err(report_error)?;
        }
        for mut files in partitions {
            let mut verified = Vec::new();
            for file in files.drain(..) {
                reporter.tick(0, false).map_err(report_error)?;
                match root.open_matching(&file) {
                    Ok(_) => verified.push(file),
                    Err(error) => issue(&mut issues, file.path_label(), error),
                }
            }
            if verified.len() > 1 {
                verified.sort_by(|a, b| {
                    a.relative
                        .components()
                        .count()
                        .cmp(&b.relative.components().count())
                        .then_with(|| a.relative.cmp(&b.relative))
                });
                groups.push(Group {
                    id: verified[0].id,
                    files: verified,
                    checksum,
                    needs_rescan: false,
                });
            }
        }
    }
    reporter.tick(0, true).map_err(report_error)?;
    groups.sort_by(|a, b| {
        b.files[0]
            .stamp
            .size
            .cmp(&a.files[0].stamp.size)
            .then_with(|| a.files[0].relative.cmp(&b.files[0].relative))
    });
    Ok(Scan {
        id,
        root,
        groups,
        issues,
        scanned: found,
        bytes_read: reporter.update.bytes_read,
        elapsed_ms: reporter.start.elapsed().as_millis() as u64,
    })
}

fn insert_by_content(
    root: &Root,
    partitions: &mut Vec<Vec<Candidate>>,
    file: Candidate,
    issues: &mut Vec<ScanIssue>,
    tick: &mut dyn FnMut(usize) -> io::Result<()>,
) -> Result<(), (Candidate, io::Error)> {
    for matches in partitions.iter_mut() {
        while let Some(representative) = matches.last() {
            match root.compare_with(representative, &file, None, tick) {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum TrashStatus {
    Moved,
    Skipped,
    Failed,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrashOutcome {
    id: usize,
    relative_path: String,
    status: TrashStatus,
    reason: Option<String>,
}

pub(crate) struct TrashResult {
    #[cfg(test)]
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
            #[cfg(test)]
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
