use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, params};

use crate::duplicates::{Root, is_link};

pub(super) const DATA_DIR: &str = ".photosys";

fn database(root: &Root, create: bool) -> Result<Option<PathBuf>, String> {
    root.verify_directory().map_err(|error| error.to_string())?;
    let directory = root.path.join(DATA_DIR);
    if create {
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => {}
        Ok(_) => return Err("相册内的 .photosys 必须是普通目录。".into()),
        Err(error) if !create && error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let path = directory.join("library.sqlite");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => Ok(Some(path)),
        Ok(_) => Err("收藏数据库必须是普通文件。".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(create.then_some(path)),
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

pub(super) fn read(root: &Root) -> Result<HashSet<Vec<u8>>, String> {
    let Some(path) = database(root, false)? else {
        return Ok(HashSet::new());
    };
    read_all(&path).map_err(|error| error.to_string())
}

fn read_all(path: &Path) -> rusqlite::Result<HashSet<Vec<u8>>> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let mut rows = connection.prepare("SELECT path FROM favorites")?;
    rows.query_map([], |row| row.get(0))?.collect()
}

pub(super) fn write(root: &Root, relative: &Path, favorite: bool) -> Result<(), String> {
    if let Some(path) = database(root, favorite)? {
        update(&path, relative, favorite).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn update(path: &Path, relative: &Path, favorite: bool) -> rusqlite::Result<()> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if favorite {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let mut connection = Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "CREATE TABLE IF NOT EXISTS favorites (path BLOB PRIMARY KEY) WITHOUT ROWID",
        [],
    )?;
    let sql = if favorite {
        "INSERT INTO favorites (path) VALUES (?1) ON CONFLICT DO NOTHING"
    } else {
        "DELETE FROM favorites WHERE path = ?1"
    };
    transaction.execute(sql, params![path_key(relative)])?;
    transaction.commit()
}
