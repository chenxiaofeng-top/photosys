use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, params};
use tempfile::NamedTempFile;

use crate::duplicates::{Root, is_link};

pub(super) const DATA_DIR: &str = ".photosys";
const SCHEMA: &str = "CREATE TABLE favorites (path BLOB PRIMARY KEY) WITHOUT ROWID";

fn data_path(root: &Root) -> PathBuf {
    #[cfg(test)]
    if let Some(path) = &root.test_data_directory {
        return path.clone();
    }
    root.path.join(DATA_DIR)
}

pub(super) fn data_directory(root: &Root) -> Result<Option<PathBuf>, String> {
    root.verify_directory().map_err(|error| error.to_string())?;
    let directory = data_path(root);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => Ok(Some(directory)),
        Ok(_) => Err("相册内的 .photosys 必须是普通目录。".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn database(directory: &Path) -> Result<Option<PathBuf>, String> {
    let path = directory.join("library.sqlite");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => Ok(Some(path)),
        Ok(_) => Err("收藏数据库必须是普通文件。".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn path_key(relative: &Path) -> Vec<u8> {
    let mut path = Vec::new();
    for component in relative.iter() {
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(component.as_encoded_bytes());
    }
    path
}

pub(super) fn read(root: &Root) -> Result<Option<HashSet<Vec<u8>>>, String> {
    let Some(directory) = data_directory(root)? else {
        return Ok(None);
    };
    database(&directory)?
        .map(|path| read_all(&path).map_err(|error| error.to_string()))
        .transpose()
}

pub(super) fn initialize(root: &Root) -> Result<HashSet<Vec<u8>>, String> {
    let directory = match data_directory(root)? {
        Some(directory) => directory,
        None => {
            match fs::create_dir(data_path(root)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.to_string()),
            }
            data_directory(root)?.ok_or("媒体库数据目录已被移除。")?
        }
    };
    if let Some(path) = database(&directory)? {
        return read_all(&path).map_err(|error| error.to_string());
    }

    let temporary = NamedTempFile::new_in(&directory).map_err(|error| error.to_string())?;
    Connection::open_with_flags(
        temporary.path(),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .and_then(|connection| connection.execute(SCHEMA, []))
    .map_err(|error| error.to_string())?;
    match temporary.persist_noclobber(directory.join("library.sqlite")) {
        Ok(_) => {}
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.error.to_string()),
    }
    read(root)?.ok_or_else(|| "收藏数据库已被移除。".into())
}

fn read_all(path: &Path) -> rusqlite::Result<HashSet<Vec<u8>>> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let mut rows = connection.prepare("SELECT path FROM favorites")?;
    rows.query_map([], |row| row.get(0))?.collect()
}

pub(super) fn write(root: &Root, relative: &Path, favorite: bool) -> Result<(), String> {
    let directory = data_directory(root)?.ok_or("媒体库数据目录已被移除。")?;
    let path = database(&directory)?.ok_or("收藏数据库已被移除。")?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(|error| error.to_string())?;
    let sql = if favorite {
        "INSERT INTO favorites (path) VALUES (?1) ON CONFLICT DO NOTHING"
    } else {
        "DELETE FROM favorites WHERE path = ?1"
    };
    connection
        .execute(sql, params![path_key(relative)])
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(super) fn read_rotations(root: &Root) -> Result<HashMap<Vec<u8>, u16>, String> {
    let directory = data_directory(root)?.ok_or("媒体库数据目录已被移除。")?;
    let path = database(&directory)?.ok_or("媒体库数据库已被移除。")?;
    let read = || -> rusqlite::Result<HashMap<Vec<u8>, u16>> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(2))?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'rotations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(HashMap::new());
        }
        let mut rows = connection.prepare("SELECT path, rotation FROM rotations")?;
        rows.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    };
    read().map_err(|error| error.to_string())
}

pub(super) fn write_rotation(root: &Root, relative: &Path, rotation: u16) -> Result<(), String> {
    if !matches!(rotation, 0 | 90 | 180 | 270) {
        return Err("旋转角度必须为 0、90、180 或 270 度。".into());
    }
    let directory = data_directory(root)?.ok_or("媒体库数据目录已被移除。")?;
    let path = database(&directory)?.ok_or("媒体库数据库已被移除。")?;
    let write = || -> rusqlite::Result<()> {
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(2))?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "CREATE TABLE IF NOT EXISTS rotations (
                path BLOB PRIMARY KEY,
                rotation INTEGER NOT NULL CHECK (rotation IN (90, 180, 270))
            ) WITHOUT ROWID",
            [],
        )?;
        if rotation == 0 {
            transaction.execute(
                "DELETE FROM rotations WHERE path = ?1",
                params![path_key(relative)],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO rotations (path, rotation) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET rotation = excluded.rotation",
                params![path_key(relative), rotation],
            )?;
        }
        transaction.commit()
    };
    write().map_err(|error| error.to_string())
}
