//! Named, many-to-many media collections. Paths use the portable library key.
use super::{Filter, ItemView, Page, Session, Sort, current, favorites, session};
use crate::duplicates::Root;
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS albums (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 80)
);
CREATE TABLE IF NOT EXISTS album_members (
    album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    path BLOB NOT NULL,
    PRIMARY KEY(album_id,path)
) WITHOUT ROWID;";

fn error(e: impl std::fmt::Display) -> String {
    format!("无法读取或保存相册。{e}")
}

fn open(root: &Root, write: bool) -> Result<Connection, String> {
    let dir = favorites::data_directory(root)?.ok_or("媒体库数据目录已被移除。")?;
    let path = favorites::database(&dir)?.ok_or("媒体库数据库已被移除。")?;
    if write
        && (std::fs::metadata(&dir)
            .map_err(error)?
            .permissions()
            .readonly()
            || std::fs::metadata(&path)
                .map_err(error)?
                .permissions()
                .readonly())
    {
        return Err("此媒体库只读，可以浏览已有相册。".into());
    }
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_NOFOLLOW
            | if write {
                OpenFlags::SQLITE_OPEN_READ_WRITE
            } else {
                OpenFlags::SQLITE_OPEN_READ_ONLY
            },
    )
    .map_err(error)?;
    c.busy_timeout(Duration::from_secs(2)).map_err(error)?;
    c.execute_batch("PRAGMA foreign_keys=ON").map_err(error)?;
    Ok(c)
}

// Both absent is an old database. A partial or incompatible schema is an error.
pub(crate) fn validate_schema(c: &Connection) -> Result<bool, String> {
    let count: u32 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('albums','album_members')",
            [],
            |r| r.get(0),
        )
        .map_err(error)?;
    if count == 0 {
        return Ok(false);
    }
    if count != 2 {
        return Err("相册表结构不完整，请恢复数据库备份。".into());
    }
    c.prepare("SELECT id,name FROM albums LIMIT 0")
        .map_err(error)?;
    c.prepare("SELECT album_id,path FROM album_members LIMIT 0")
        .map_err(error)?;
    Ok(true)
}

fn writable(root: &Root) -> bool {
    open(root, true)
        .and_then(|c| c.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").map_err(error))
        .is_ok()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Album {
    id: i64,
    name: String,
    count: usize,
    cover: Option<ItemView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AlbumPage {
    #[serde(flatten)]
    page: Page<Album>,
    writable: bool,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Edit {
    Create {
        name: String,
        #[serde(default)]
        file_ids: Vec<usize>,
    },
    Rename {
        album_id: i64,
        name: String,
    },
    Delete {
        album_id: i64,
    },
    Add {
        album_id: i64,
        file_ids: Vec<usize>,
    },
    Remove {
        album_id: i64,
        file_ids: Vec<usize>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditResult {
    album_id: i64,
}

fn paths(c: &Connection, album_id: i64) -> Result<HashSet<Vec<u8>>, String> {
    let mut q = c
        .prepare("SELECT path FROM album_members WHERE album_id=?1")
        .map_err(error)?;
    q.query_map([album_id], |r| r.get(0))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
}

fn require_album(c: &Connection, id: i64) -> Result<(), String> {
    if !c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM albums WHERE id=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )
        .map_err(error)?
    {
        return Err("相册已不存在，请刷新相册列表。".into());
    }
    Ok(())
}

fn list(s: &Session, query: &str, requested: usize) -> Result<AlbumPage, String> {
    let c = open(&s.scan.root, false)?;
    // Read names and membership within one consistent SQLite snapshot.
    c.execute_batch("BEGIN").map_err(error)?;
    let mut albums = Vec::new();
    if validate_schema(&c)? {
        let mut q = c
            .prepare("SELECT id,name FROM albums ORDER BY id DESC")
            .map_err(error)?;
        let needle = query.trim().to_lowercase();
        for row in q
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(error)?
        {
            let (id, name) = row.map_err(error)?;
            if name.to_lowercase().contains(&needle) {
                albums.push((id, name));
            }
        }
    }
    let total = albums.len();
    let (page, offset) = Page::<Album>::bounds(requested, total);
    let by_path: HashMap<_, _> = s
        .scan
        .items
        .iter()
        .map(|item| (favorites::path_key(&item.file.relative), item))
        .collect();
    let mut valid = HashMap::new();
    let mut items = Vec::new();
    for (id, name) in albums.into_iter().skip(offset).take(super::PAGE_SIZE) {
        let keys = paths(&c, id)?;
        let mut members: Vec<_> = keys
            .iter()
            .filter_map(|key| by_path.get(key).copied())
            .filter(|item| {
                *valid
                    .entry(item.file.id)
                    .or_insert_with(|| s.scan.root.open_matching(&item.file).is_ok())
            })
            .collect();
        members.sort_unstable_by(|a, b| Sort::DateDesc.compare(a, b));
        let cover = members.iter().find(|i| i.preview_available).map(|item| {
            item.view(
                s.favorites.contains(&item.file.id),
                s.rotations.get(&item.file.id).copied().unwrap_or(0),
            )
        });
        items.push(Album {
            id,
            name,
            count: members.len(),
            cover,
        });
    }
    drop(c);
    Ok(AlbumPage {
        page: Page { page, total, items },
        writable: writable(&s.scan.root),
    })
}

fn items(
    s: &Session,
    album_id: i64,
    filter: Filter,
    sort: Sort,
    requested: usize,
) -> Result<Page<ItemView>, String> {
    let c = open(&s.scan.root, false)?;
    c.execute_batch("BEGIN").map_err(error)?;
    if !validate_schema(&c)? {
        return Err("相册已不存在，请刷新相册列表。".into());
    }
    require_album(&c, album_id)?;
    let keys = paths(&c, album_id)?;
    let mut matching: Vec<_> = s
        .matching_items(&filter)?
        .into_iter()
        .filter(|i| {
            keys.contains(&favorites::path_key(&i.file.relative))
                && s.scan.root.open_matching(&i.file).is_ok()
        })
        .collect();
    matching.sort_unstable_by(|a, b| sort.compare(a, b));
    let total = matching.len();
    let (page, offset) = Page::<ItemView>::bounds(requested, total);
    Ok(Page {
        page,
        total,
        items: matching
            .into_iter()
            .skip(offset)
            .take(super::PAGE_SIZE)
            .map(|i| {
                i.view(
                    s.favorites.contains(&i.file.id),
                    s.rotations.get(&i.file.id).copied().unwrap_or(0),
                )
            })
            .collect(),
    })
}

fn name(value: &str) -> Result<&str, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 {
        return Err("相册名称不能为空，且最多 80 个字符。".into());
    }
    Ok(value)
}

fn edit(s: &Session, edit: Edit) -> Result<EditResult, String> {
    let mut c = open(&s.scan.root, true)?;
    validate_schema(&c)?;
    let tx = c
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    tx.execute_batch(SCHEMA).map_err(error)?;
    let (id, ids, add) = match edit {
        Edit::Create {
            name: value,
            file_ids,
        } => {
            tx.execute("INSERT INTO albums(name) VALUES(?1)", [name(&value)?])
                .map_err(error)?;
            (tx.last_insert_rowid(), file_ids, true)
        }
        Edit::Rename {
            album_id,
            name: value,
        } => {
            require_album(&tx, album_id)?;
            tx.execute(
                "UPDATE albums SET name=?1 WHERE id=?2",
                params![name(&value)?, album_id],
            )
            .map_err(error)?;
            (album_id, Vec::new(), false)
        }
        Edit::Delete { album_id } => {
            require_album(&tx, album_id)?;
            tx.execute("DELETE FROM albums WHERE id=?1", [album_id])
                .map_err(error)?;
            (album_id, Vec::new(), false)
        }
        Edit::Add { album_id, file_ids } => {
            require_album(&tx, album_id)?;
            (album_id, file_ids, true)
        }
        Edit::Remove { album_id, file_ids } => {
            require_album(&tx, album_id)?;
            (album_id, file_ids, false)
        }
    };
    for file_id in ids.into_iter().collect::<HashSet<_>>() {
        let item = s.scan.item(file_id)?;
        s.scan
            .root
            .open_matching(&item.file)
            .map_err(|_| super::STALE_SCAN.to_string())?;
        let key = favorites::path_key(&item.file.relative);
        tx.execute(
            if add {
                "INSERT INTO album_members(album_id,path) VALUES(?1,?2) ON CONFLICT DO NOTHING"
            } else {
                "DELETE FROM album_members WHERE album_id=?1 AND path=?2"
            },
            params![id, key],
        )
        .map_err(error)?;
    }
    tx.commit().map_err(error)?;
    Ok(EditResult { album_id: id })
}

#[tauri::command]
pub(crate) async fn albums_list(
    scan_id: u64,
    query: String,
    page: usize,
) -> Result<AlbumPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _db = crate::database_access::read()?;
        list(current(&*session()?, scan_id)?, &query, page)
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub(crate) async fn album_items(
    scan_id: u64,
    album_id: i64,
    filter: Filter,
    sort: Option<Sort>,
    page: usize,
) -> Result<Page<ItemView>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _db = crate::database_access::read()?;
        items(
            current(&*session()?, scan_id)?,
            album_id,
            filter,
            sort.unwrap_or_default(),
            page,
        )
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub(crate) async fn albums_edit(scan_id: u64, edit: Edit) -> Result<EditResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _task = crate::file_task()?;
        let _db = crate::database_access::read()?;
        // Hold the session through commit so scan-local IDs cannot be reused.
        edit_album(scan_id, edit)
    })
    .await
    .map_err(error)?
}

fn edit_album(scan_id: u64, change: Edit) -> Result<EditResult, String> {
    edit(current(&*session()?, scan_id)?, change)
}

#[cfg(test)]
mod tests;
