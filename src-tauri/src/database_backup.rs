//! Portable database snapshots and journaled, recoverable replacement of album databases.
use crate::{
    ai::error,
    duplicates::{Root, is_link},
    library,
};
use chrono::{Local, Utc};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tauri::{AppHandle, WebviewWindow, ipc::Channel};
use tauri_plugin_dialog::DialogExt;
use tempfile::{NamedTempFile, TempDir};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const DATABASES: [&str; 3] = ["library.sqlite", "ai.sqlite", "people.sqlite"];
const SUFFIXES: [&str; 4] = ["", "-wal", "-shm", "-journal"];
const MANIFEST: &str = "photosys-backup.json";
const JOURNAL: &str = "restore-journal.json";
const RESERVE: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Entry {
    name: String,
    size: u64,
    sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Manifest {
    format_version: u32,
    app_version: String,
    created_at: String,
    source_album: String,
    databases: Vec<Entry>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Inspection {
    manifest: Manifest,
    pub(crate) fingerprint: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Progress {
    phase: &'static str,
    database: Option<String>,
}
fn progress(send: &impl Fn(Progress), phase: &'static str, database: Option<&str>) {
    send(Progress {
        phase,
        database: database.map(str::to_owned),
    });
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestoreResult {
    directory: String,
    rollback_path: String,
    summary: Option<library::Summary>,
    reopen_error: Option<String>,
    warning: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    format_version: u32,
    backup_directory: String,
    #[serde(default)]
    staging_directory: Option<String>,
    original_files: Vec<Entry>,
    committed: bool,
}

fn regular(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && !is_link(&m) => Ok(true),
        Ok(_) => Err(format!("必须是普通文件：{}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(error(e)),
    }
}
fn directory(path: &Path) -> Result<(), String> {
    let m = fs::symlink_metadata(path).map_err(error)?;
    if !m.is_dir() || is_link(&m) {
        return Err(format!("必须是普通目录：{}", path.display()));
    }
    Ok(())
}
fn data_directory(root: &Root, create: bool) -> Result<PathBuf, String> {
    root.verify_directory().map_err(error)?;
    let path = root.path.join(".photosys");
    if create && !path.try_exists().map_err(error)? {
        fs::create_dir(&path).map_err(error)?;
    }
    directory(&path)?;
    Ok(path)
}
fn connection(path: &Path) -> Result<Connection, String> {
    if !regular(path)? {
        return Err(format!("缺少数据库：{}", path.display()));
    }
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(error)?;
    c.busy_timeout(Duration::from_secs(3)).map_err(error)?;
    c.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")
        .map_err(error)?;
    Ok(c)
}
fn descriptor(path: &Path, name: &str) -> Result<Entry, String> {
    if !regular(path)? {
        return Err(format!("缺少文件：{}", path.display()));
    }
    let mut file = File::open(path).map_err(error)?;
    let mut hash = Sha256::new();
    let size = std::io::copy(&mut file, &mut hash).map_err(error)?;
    Ok(Entry {
        name: name.into(),
        size,
        sha256: format!("{:x}", hash.finalize()),
    })
}
fn verify_file(path: &Path, entry: &Entry) -> Result<(), String> {
    let actual = descriptor(path, &entry.name)?;
    if actual.size != entry.size || actual.sha256 != entry.sha256 {
        return Err(format!("文件校验失败：{}", entry.name));
    }
    Ok(())
}
fn validate_database(path: &Path, name: &str) -> Result<(), String> {
    let c = connection(path)?;
    let mut check = c.prepare("PRAGMA integrity_check").map_err(error)?;
    let rows = check
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(error)?;
    for row in rows {
        if row.map_err(error)? != "ok" {
            return Err(format!("数据库完整性检查失败：{name}"));
        }
    }
    let version: u32 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(error)?;
    if version > if name == "people.sqlite" { 2 } else { 0 } {
        return Err(format!("{name} 来自较新版本，请更新 PhotoSys。"));
    }
    let queries: &[&str] = match name {
        "library.sqlite" => &["SELECT path FROM favorites LIMIT 0"],
        "ai.sqlite" => &[
            "SELECT key,value FROM settings LIMIT 0",
            "SELECT path,stamp,rotation,version,vector,error FROM embeddings LIMIT 0",
        ],
        "people.sqlite" => &[
            "SELECT key,value FROM settings LIMIT 0",
            "SELECT id,name,archived FROM people LIMIT 0",
            "SELECT path,stamp,rotation,version,error FROM media LIMIT 0",
            "SELECT id,path,bbox,confidence,vector,thumbnail,person_id,manual,ignored,active,review FROM faces LIMIT 0",
            "SELECT face_id,person_id FROM rejections LIMIT 0",
            "SELECT id,description,payload FROM edits LIMIT 0",
        ],
        _ => return Err("未知数据库。".into()),
    };
    for sql in queries {
        c.prepare(sql)
            .map_err(|e| format!("{name} 表结构不兼容。{e}"))?;
    }
    if name == "library.sqlite" {
        for (table, sql) in [
            ("rotations", "SELECT path,rotation FROM rotations LIMIT 0"),
            (
                "media_locations",
                "SELECT path,stamp,version,captured,location FROM media_locations LIMIT 0",
            ),
        ] {
            let exists: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                    [table],
                    |r| r.get(0),
                )
                .map_err(error)?;
            if exists {
                c.prepare(sql).map_err(error)?;
            }
        }
    }
    Ok(())
}
fn snapshot(source: &Path, destination: &Path, mut tick: impl FnMut()) -> Result<(), String> {
    let source = connection(source)?;
    let mut target = Connection::open(destination).map_err(error)?;
    let backup = Backup::new(&source, &mut target).map_err(error)?;
    let mut waiting = Instant::now();
    loop {
        tick();
        match backup.step(256).map_err(error)? {
            StepResult::Done => break,
            StepResult::More => waiting = Instant::now(),
            StepResult::Busy | StepResult::Locked => {
                if waiting.elapsed() > Duration::from_secs(3) {
                    return Err("数据库被其他任务占用，请稍后重试。".into());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => return Err("无法完成数据库快照。".into()),
        }
    }
    drop(backup);
    target.close().map_err(|(_, e)| error(e))?;
    File::open(destination)
        .and_then(|f| f.sync_all())
        .map_err(error)
}
fn space(path: &Path, bytes: u64) -> Result<(), String> {
    if fs2::available_space(path).map_err(error)? < bytes.saturating_add(RESERVE) {
        return Err("可用磁盘空间不足，请释放空间后重试。".into());
    }
    Ok(())
}
#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path).and_then(|f| f.sync_all()).map_err(error)
}
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path.parent().ok_or("缺少父目录。")?;
    let mut temp = NamedTempFile::new_in(parent).map_err(error)?;
    serde_json::to_writer(&mut temp, value).map_err(error)?;
    temp.as_file().sync_all().map_err(error)?;
    temp.persist(path).map_err(|e| error(e.error))?;
    sync_directory(parent)
}

pub(crate) fn export(
    root: &Root,
    destination: &Path,
    send: impl Fn(Progress),
) -> Result<(), String> {
    recover(root)?;
    let data = data_directory(root, false)?;
    let parent = destination
        .parent()
        .ok_or("请选择备份保存目录。")?
        .canonicalize()
        .map_err(error)?;
    let filename = destination.file_name().ok_or("缺少备份文件名。")?;
    let destination = parent.join(filename);
    if destination
        .extension()
        .is_none_or(|e| !e.eq_ignore_ascii_case("zip"))
    {
        return Err("备份文件扩展名必须为 .zip。".into());
    }
    if parent.starts_with(&data) {
        return Err("请将备份保存到 .photosys 数据目录之外。".into());
    }
    regular(&destination)?;
    let work = TempDir::new_in(&parent).map_err(error)?;
    let mut databases = Vec::new();
    for name in DATABASES {
        let path = data.join(name);
        if !regular(&path)? {
            if name == DATABASES[0] {
                return Err("找不到媒体库数据库。".into());
            }
            continue;
        }
        space(
            &parent,
            fs::metadata(&path).map_err(error)?.len().saturating_mul(2),
        )?;
        progress(&send, "snapshot", Some(name));
        snapshot(&path, &work.path().join(name), || {})?;
        validate_database(&work.path().join(name), name)?;
        databases.push(descriptor(&work.path().join(name), name)?);
    }
    let manifest = Manifest {
        format_version: 1,
        app_version: env!("CARGO_PKG_VERSION").into(),
        created_at: Utc::now().to_rfc3339(),
        source_album: root
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        databases,
    };
    progress(&send, "packing", None);
    let mut temp = NamedTempFile::new_in(&parent).map_err(error)?;
    {
        let mut zip = ZipWriter::new(temp.as_file_mut());
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o600)
            .large_file(true);
        zip.start_file(MANIFEST, options).map_err(error)?;
        serde_json::to_writer(&mut zip, &manifest).map_err(error)?;
        for entry in &manifest.databases {
            zip.start_file(&entry.name, options).map_err(error)?;
            std::io::copy(
                &mut File::open(work.path().join(&entry.name)).map_err(error)?,
                &mut zip,
            )
            .map_err(error)?;
        }
        zip.finish().map_err(error)?;
    }
    temp.as_file().sync_all().map_err(error)?;
    root.verify_directory().map_err(error)?;
    regular(&destination)?;
    temp.persist(&destination).map_err(|e| error(e.error))?;
    sync_directory(&parent)?;
    progress(&send, "complete", None);
    Ok(())
}

// ZipArchive indexes entries by name and silently collapses duplicates. Inspect
// the central records as well so a duplicate cannot bypass the whitelist.
fn validate_central_directory(file: &mut File, offset: u64, expected: usize) -> Result<(), String> {
    file.seek(SeekFrom::Start(offset)).map_err(error)?;
    let mut names = HashSet::new();
    loop {
        let mut header = [0u8; 46];
        file.read_exact(&mut header[..4]).map_err(error)?;
        if matches!(&header[..4], b"PK\x05\x06" | b"PK\x06\x06") {
            break;
        }
        if &header[..4] != b"PK\x01\x02" || names.len() >= 4 {
            return Err("备份包中央目录无效。".into());
        }
        file.read_exact(&mut header[4..]).map_err(error)?;
        let length = u16::from_le_bytes([header[28], header[29]]) as usize;
        if length > 64 {
            return Err("备份包文件名无效。".into());
        }
        let mut raw = vec![0; length];
        file.read_exact(&mut raw).map_err(error)?;
        let name = String::from_utf8(raw).map_err(error)?;
        if name != MANIFEST && !DATABASES.contains(&name.as_str()) {
            return Err("备份包包含非法文件。".into());
        }
        if !names.insert(name) {
            return Err("备份包包含重复文件。".into());
        }
        let extra = u16::from_le_bytes([header[30], header[31]]) as i64;
        let comment = u16::from_le_bytes([header[32], header[33]]) as i64;
        file.seek(SeekFrom::Current(extra + comment))
            .map_err(error)?;
    }
    if names.len() != expected {
        return Err("备份包文件数量不符。".into());
    }
    Ok(())
}

pub(crate) fn inspect(
    source: &Path,
    work: &Path,
    send: &impl Fn(Progress),
) -> Result<Inspection, String> {
    progress(send, "checking", None);
    if !regular(source)? {
        return Err("找不到备份文件。".into());
    }
    let mut file = File::open(source).map_err(error)?;
    let mut zip = ZipArchive::new(file.try_clone().map_err(error)?)
        .map_err(|e| format!("无法读取备份包。{e}"))?;
    validate_central_directory(&mut file, zip.central_directory_start(), zip.len())?;
    if !(2..=4).contains(&zip.len()) {
        return Err("备份包文件数量无效。".into());
    }
    let mut names = HashSet::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(error)?;
        let name = entry.name();
        let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
        if (name != MANIFEST && !DATABASES.contains(&name))
            || !names.insert(name.to_owned())
            || !matches!(kind, 0 | 0o100000)
            || entry.is_dir()
        {
            return Err("备份包包含非法、重复或非普通文件。".into());
        }
    }
    let raw = {
        let entry = zip.by_name(MANIFEST).map_err(error)?;
        if entry.size() > 65536 {
            return Err("备份清单过大。".into());
        }
        let mut raw = Vec::new();
        entry.take(65537).read_to_end(&mut raw).map_err(error)?;
        if raw.len() > 65536 {
            return Err("备份清单过大。".into());
        }
        raw
    };
    let manifest: Manifest =
        serde_json::from_slice(&raw).map_err(|e| format!("备份清单无效。{e}"))?;
    if manifest.format_version != 1 {
        return Err("不支持此备份格式版本。".into());
    }
    chrono::DateTime::parse_from_rfc3339(&manifest.created_at).map_err(|_| "备份时间无效。")?;
    let expected: HashSet<_> = manifest.databases.iter().map(|e| e.name.as_str()).collect();
    if expected.len() != manifest.databases.len()
        || !expected.contains(DATABASES[0])
        || expected.len() + 1 != names.len()
    {
        return Err("备份数据库清单不完整或重复。".into());
    }
    let mut total = 0u64;
    for e in &manifest.databases {
        if !DATABASES.contains(&e.name.as_str())
            || !names.contains(&e.name)
            || e.sha256.len() != 64
            || !e.sha256.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("备份数据库清单无效。".into());
        }
        total = total.checked_add(e.size).ok_or("备份大小无效。")?;
        if zip.by_name(&e.name).map_err(error)?.size() != e.size {
            return Err("备份数据库大小不符。".into());
        }
    }
    space(work, total)?;
    for entry in &manifest.databases {
        progress(send, "checking", Some(&entry.name));
        let mut input = zip.by_name(&entry.name).map_err(error)?;
        let path = work.join(&entry.name);
        let mut output = File::create(&path).map_err(error)?;
        let copied = std::io::copy(
            &mut (&mut input).take(entry.size.saturating_add(1)),
            &mut output,
        )
        .map_err(error)?;
        if copied != entry.size {
            return Err("备份数据库大小不符。".into());
        }
        output.sync_all().map_err(error)?;
        drop(output);
        verify_file(&path, entry)?;
        validate_database(&path, &entry.name)?;
    }
    Ok(Inspection {
        manifest,
        fingerprint: format!("{:x}", Sha256::digest(&raw)),
    })
}
fn database_files() -> Vec<String> {
    DATABASES
        .iter()
        .flat_map(|name| SUFFIXES.iter().map(move |s| format!("{name}{s}")))
        .collect()
}
fn valid_backup_name(name: &str) -> bool {
    name.starts_with("restore-backup-")
        && (30..=80).contains(&name.len())
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
fn read_journal(data: &Path) -> Result<Option<Journal>, String> {
    let path = data.join(JOURNAL);
    if !regular(&path)? {
        return Ok(None);
    }
    if fs::metadata(&path).map_err(error)?.len() > 65536 {
        return Err("恢复日志无效，请保留目录并检查回退副本。".into());
    }
    let journal: Journal =
        serde_json::from_reader(File::open(path).map_err(error)?).map_err(error)?;
    let files = database_files();
    let mut names = HashSet::new();
    if journal.format_version != 1
        || !valid_backup_name(&journal.backup_directory)
        || journal.staging_directory.as_ref().is_some_and(|s| {
            s.len() > 80
                || s.strip_prefix(".restore-stage-").is_none_or(|suffix| {
                    suffix.is_empty() || !suffix.bytes().all(|c| c.is_ascii_alphanumeric())
                })
        })
        || journal
            .original_files
            .iter()
            .any(|e| !files.contains(&e.name) || !names.insert(&e.name))
    {
        return Err("恢复日志无效，请保留目录并检查回退副本。".into());
    }
    Ok(Some(journal))
}
fn remove_file(path: &Path) -> Result<(), String> {
    if regular(path)? {
        fs::remove_file(path).map_err(error)?;
    }
    Ok(())
}
fn copy_synced(source: &Path, destination: &Path) -> Result<(), String> {
    if !regular(source)? {
        return Err(format!("回退文件缺失：{}", source.display()));
    }
    let parent = destination.parent().ok_or("缺少父目录。")?;
    let mut temp = NamedTempFile::new_in(parent).map_err(error)?;
    std::io::copy(&mut File::open(source).map_err(error)?, &mut temp).map_err(error)?;
    temp.as_file().sync_all().map_err(error)?;
    regular(destination)?;
    temp.persist(destination).map_err(|e| error(e.error))?;
    Ok(())
}
fn clear_journal(data: &Path, journal: &Journal) -> Result<(), String> {
    if let Some(name) = &journal.staging_directory {
        let staging = data.join(name);
        match fs::symlink_metadata(&staging) {
            Ok(_) => {
                directory(&staging)?;
                fs::remove_dir_all(&staging).map_err(error)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(e)),
        }
    }
    remove_file(&data.join(JOURNAL))?;
    sync_directory(data)
}

fn rollback(
    data: &Path,
    journal: &Journal,
    mut before: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    let backup = data.join(&journal.backup_directory);
    directory(&backup)?;
    // Validate every preserved file before modifying the target. Originals stay untouched.
    for entry in &journal.original_files {
        verify_file(&backup.join(&entry.name), entry)?;
    }
    for name in database_files() {
        before(&name)?;
        if let Some(entry) = journal.original_files.iter().find(|e| e.name == name) {
            // Unchanged originals need neither rewriting nor extra free disk space.
            if verify_file(&data.join(&name), entry).is_err() {
                copy_synced(&backup.join(&name), &data.join(&name))?;
            }
        } else {
            remove_file(&data.join(&name))?;
        }
        sync_directory(data)?;
    }
    clear_journal(data, journal)
}
/// Invoked before opening an album, exporting it, or beginning another restore.
/// Caller has exclusive database access and has closed the reusable people reader.
pub(crate) fn recover(root: &Root) -> Result<(), String> {
    root.verify_directory().map_err(error)?;
    let data = root.path.join(".photosys");
    if !data.try_exists().map_err(error)? {
        return Ok(());
    }
    directory(&data)?;
    let Some(journal) = read_journal(&data)? else {
        return Ok(());
    };
    if journal.committed {
        clear_journal(&data, &journal)
    } else {
        rollback(&data, &journal, |_| Ok(())).map_err(|e| {
            format!(
                "未完成的恢复无法回滚，请保留 {}。{e}",
                data.join(&journal.backup_directory).display()
            )
        })
    }
}

pub(crate) fn restore(
    root: &Root,
    source: &Path,
    fingerprint: &str,
    backup_name: &str,
    send: impl Fn(Progress),
    mut before: impl FnMut(&str) -> Result<(), String>,
) -> Result<(PathBuf, Option<String>), String> {
    if !valid_backup_name(backup_name) {
        return Err("回退副本名称无效。".into());
    }
    recover(root)?;
    // Validate the entire input before creating or changing any target database.
    let checked = tempfile::tempdir().map_err(error)?;
    let inspection = inspect(source, checked.path(), &send)?;
    if inspection.fingerprint != fingerprint {
        return Err("备份文件已变化，请重新选择并确认。".into());
    }
    let data = data_directory(root, true)?;
    let backup = data.join(backup_name);
    let files = database_files();
    let mut original_files = Vec::new();
    for name in &files {
        if regular(&data.join(name))? {
            original_files.push(descriptor(&data.join(name), name)?);
        }
    }
    let needed = original_files
        .iter()
        .chain(&inspection.manifest.databases)
        .try_fold(0u64, |sum, e| {
            sum.checked_add(e.size).ok_or("文件大小无效。")
        })?;
    space(&data, needed)?;
    let staging = tempfile::Builder::new()
        .prefix(".restore-stage-")
        .tempdir_in(&data)
        .map_err(error)?;
    for entry in &inspection.manifest.databases {
        copy_synced(
            &checked.path().join(&entry.name),
            &staging.path().join(&entry.name),
        )?;
    }
    progress(&send, "preserving", None);
    fs::create_dir(&backup).map_err(error)?;
    for entry in &original_files {
        copy_synced(&data.join(&entry.name), &backup.join(&entry.name))?;
        verify_file(&backup.join(&entry.name), entry)?;
    }
    let mut journal = Journal {
        format_version: 1,
        backup_directory: backup_name.into(),
        staging_directory: Some(
            staging
                .path()
                .file_name()
                .ok_or("缺少暂存目录名。")?
                .to_string_lossy()
                .into_owned(),
        ),
        original_files,
        committed: false,
    };
    write_json(&backup.join("recovery.json"), &journal)?;
    sync_directory(&backup)?;
    root.verify_directory().map_err(error)?;
    write_json(&data.join(JOURNAL), &journal)?;
    let applied = (|| {
        for name in &files {
            before(name)?;
            progress(&send, "restoring", Some(name));
            root.verify_directory().map_err(error)?;
            directory(&data)?;
            regular(&data.join(name))?;
            if inspection
                .manifest
                .databases
                .iter()
                .any(|e| &e.name == name)
            {
                fs::rename(staging.path().join(name), data.join(name)).map_err(error)?;
            } else {
                remove_file(&data.join(name))?;
            }
            sync_directory(&data)?;
        }
        journal.committed = true;
        write_json(&data.join(JOURNAL), &journal)
    })();
    if let Err(cause) = applied {
        progress(&send, "rollingBack", None);
        // A commit marker may have been renamed successfully before directory sync
        // failed. Persist a pending marker before modifying any files during rollback.
        if journal.committed {
            journal.committed = false;
            if let Err(e) = write_json(&data.join(JOURNAL), &journal) {
                let _ = library::invalidate();
                return Err(format!(
                    "恢复提交失败，暂无法安全回滚。请保留 {} 并重新打开相册检查恢复日志。{cause}；{e}",
                    backup.display()
                ));
            }
        }
        return match rollback(&data, &journal, |_| Ok(())) {
            Ok(()) => Err(format!(
                "恢复失败，已还原原数据库。{cause} 回退副本：{}",
                backup.display()
            )),
            Err(e) => {
                let _ = library::invalidate();
                Err(format!(
                    "恢复失败，回滚尚未完成。请保留 {} 并重新打开相册以重试回滚。{cause}；{e}",
                    backup.display()
                ))
            }
        };
    }
    // The durable commit is the success boundary; journal cleanup is retryable.
    let warning = clear_journal(&data, &journal)
        .err()
        .map(|e| format!("数据库已恢复，恢复日志将在下次打开时清理。{e}"));
    Ok((backup, warning))
}

#[tauri::command]
pub(crate) async fn database_backup_file(
    window: WebviewWindow,
    save: bool,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dialog = window
            .dialog()
            .file()
            .set_parent(&window)
            .add_filter("PhotoSys 数据库备份", &["zip"]);
        let chosen = if save {
            dialog
                .set_title("保存数据库备份")
                .set_file_name(format!(
                    "PhotoSys_数据库备份_{}.zip",
                    Local::now().format("%Y%m%d_%H%M%S")
                ))
                .blocking_save_file()
        } else {
            dialog.set_title("选择数据库备份").blocking_pick_file()
        };
        chosen
            .map(|file| {
                file.into_path().map_err(error).and_then(|p| {
                    p.into_os_string()
                        .into_string()
                        .map_err(|_| "备份路径无法显示。".into())
                })
            })
            .transpose()
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn database_export(
    scan_id: u64,
    destination: String,
    progress: Channel<Progress>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let _database = crate::database_access::maintenance()?;
        library::people::close_reader()?;
        let scan = library::scan(scan_id)?;
        export(&scan.root, Path::new(&destination), |p| {
            let _ = progress.send(p);
        })
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn database_inspect(source: String) -> Result<Inspection, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let work = tempfile::tempdir().map_err(error)?;
        inspect(Path::new(&source), work.path(), &|_| {})
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub(crate) async fn database_restore(
    app: AppHandle,
    source: String,
    directory: String,
    fingerprint: String,
    backup_name: String,
    progress: Channel<Progress>,
) -> Result<RestoreResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let _database = crate::database_access::maintenance()?;
        library::people::close_reader()?;
        let root = Root::new(Path::new(&directory)).map_err(error)?;
        let directory = root.path.to_str().ok_or("相册路径无法显示。")?.to_owned();
        let (rollback, mut warning) = restore(
            &root,
            Path::new(&source),
            &fingerprint,
            &backup_name,
            |p| {
                let _ = progress.send(p);
            },
            |_| Ok(()),
        )?;
        let invalidation_error = library::invalidate().err();
        progress
            .send(Progress {
                phase: "reopening",
                database: None,
            })
            .ok();
        let reopened = invalidation_error.map_or_else(
            || library::open_directory(&directory, library::OpenMode::Refresh, |_| {}),
            Err,
        );
        let (summary, reopen_error) = match reopened {
            Ok(summary) => (Some(summary), None),
            Err(e) => (None, Some(e)),
        };
        if let Err(e) = library::directory_preference(&app)
            .and_then(|p| library::save_preference(&p, &directory).map_err(error))
        {
            warning = Some(format!(
                "{} 无法记住恢复后的相册目录。{e}",
                warning.unwrap_or_default()
            ));
        }
        Ok(RestoreResult {
            directory,
            rollback_path: rollback.to_string_lossy().into_owned(),
            summary,
            reopen_error,
            warning,
        })
    })
    .await
    .map_err(error)?
}

#[cfg(test)]
mod tests;
