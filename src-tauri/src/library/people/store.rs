use super::super::{Scan, favorites};
use crate::ai::{
    error,
    faces::{Face, VERSION},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, time::Duration};

pub(super) struct Store {
    pub connection: Connection,
}
const SCHEMA:&str="
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS people(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL DEFAULT '',archived INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS media(path BLOB PRIMARY KEY,stamp BLOB NOT NULL,rotation INTEGER NOT NULL,version TEXT NOT NULL,error TEXT);
CREATE TABLE IF NOT EXISTS faces(id INTEGER PRIMARY KEY AUTOINCREMENT,path BLOB NOT NULL,bbox TEXT NOT NULL,confidence REAL NOT NULL,vector BLOB,thumbnail BLOB NOT NULL,person_id INTEGER REFERENCES people(id),manual INTEGER NOT NULL DEFAULT 0,ignored INTEGER NOT NULL DEFAULT 0,active INTEGER NOT NULL DEFAULT 1,review INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS faces_path ON faces(path,active);
CREATE INDEX IF NOT EXISTS faces_person ON faces(person_id,active,ignored);
CREATE INDEX IF NOT EXISTS faces_pending ON faces(active,ignored,person_id,review);
CREATE TABLE IF NOT EXISTS rejections(face_id INTEGER NOT NULL REFERENCES faces(id),person_id INTEGER NOT NULL REFERENCES people(id),PRIMARY KEY(face_id,person_id));
CREATE TABLE IF NOT EXISTS edits(id INTEGER PRIMARY KEY AUTOINCREMENT,description TEXT NOT NULL,payload TEXT NOT NULL);
PRAGMA user_version=2;";
#[derive(Clone)]
pub(super) struct Record {
    pub stamp: Vec<u8>,
    pub rotation: u16,
    pub version: String,
    pub geometry_version: String,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct PersonState {
    id: i64,
    name: String,
    archived: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct FaceState {
    id: i64,
    person_id: Option<i64>,
    manual: bool,
    ignored: bool,
    review: bool,
    rejections: Vec<i64>,
}
#[derive(Default, Serialize, Deserialize)]
struct Undo {
    people: Vec<PersonState>,
    faces: Vec<FaceState>,
    created: Vec<i64>,
}
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Edit {
    Rename {
        person_id: i64,
        name: String,
    },
    Merge {
        source_id: i64,
        target_id: i64,
    },
    Assign {
        face_ids: Vec<i64>,
        person_id: Option<i64>,
        name: Option<String>,
    },
    Ignore {
        face_ids: Vec<i64>,
    },
    Restore {
        face_ids: Vec<i64>,
    },
}
fn name(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.chars().count() > 80 || s.chars().any(char::is_control) {
        Err("人物名称最多 80 个字符，不能包含控制字符。".into())
    } else {
        Ok(s.into())
    }
}
impl Store {
    fn has_geometry(c: &Connection) -> Result<bool, String> {
        let mut q = c.prepare("PRAGMA table_info(media)").map_err(error)?;
        let names = q
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        Ok(names.iter().any(|n| n == "geometry_version"))
    }
    pub fn geometry_expression(&self) -> Result<String, String> {
        Ok(if Self::has_geometry(&self.connection)? {
            "m.geometry_version".into()
        } else {
            format!(
                "CASE WHEN m.version='{VERSION}' THEN '{}' ELSE '' END",
                crate::ai::faces::GEOMETRY_VERSION
            )
        })
    }

    pub fn open(scan: &Scan, write: bool) -> Result<Option<Self>, String> {
        let dir = favorites::data_directory(&scan.root)?.ok_or("媒体库数据目录不可用。")?;
        Self::at(&dir.join("people.sqlite"), write)
    }
    pub fn at(path: &Path, write: bool) -> Result<Option<Self>, String> {
        match fs::symlink_metadata(path) {
            Ok(m) if !m.is_file() || crate::duplicates::is_link(&m) => {
                return Err("人物数据库必须是普通文件。".into());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !write => return Ok(None),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(error(e)),
            _ => {}
        }
        let flags = if write {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let c = Connection::open_with_flags(path, flags).map_err(error)?;
        c.busy_timeout(Duration::from_secs(3)).map_err(error)?;
        c.execute_batch("PRAGMA foreign_keys=ON;").map_err(error)?;
        let version: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(error)?;
        if version > 2 {
            return Err("人物数据库来自较新版本，请更新 PhotoSys。".into());
        }
        if write {
            c.execute_batch("BEGIN IMMEDIATE").map_err(error)?;
            let migration = (|| -> Result<(), String> {
                c.execute_batch(SCHEMA).map_err(error)?;
                if !Self::has_geometry(&c)? {
                    c.execute_batch(
                        "ALTER TABLE media ADD COLUMN geometry_version TEXT NOT NULL DEFAULT '';",
                    )
                    .map_err(error)?;
                    c.execute(
                        "UPDATE media SET geometry_version=?1 WHERE version=?2",
                        rusqlite::params![crate::ai::faces::GEOMETRY_VERSION, VERSION],
                    )
                    .map_err(error)?;
                }
                Ok(())
            })();
            if let Err(e) = migration {
                let _ = c.execute_batch("ROLLBACK");
                return Err(e);
            }
            c.execute_batch("COMMIT").map_err(error)?;
        }
        Ok(Some(Self { connection: c }))
    }
    pub fn writable(scan: &Scan) -> bool {
        let Ok(Some(dir)) = favorites::data_directory(&scan.root) else {
            return false;
        };
        if tempfile::NamedTempFile::new_in(&dir).is_err() {
            return false;
        }
        let path = dir.join("people.sqlite");
        if !path.exists() {
            return true;
        }
        Self::at(&path, true)
            .and_then(|s| s.ok_or("找不到人物数据库。".into()))
            .and_then(|s| {
                s.connection
                    .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
                    .map_err(error)
            })
            .is_ok()
    }
    pub fn enabled(&self) -> Result<bool, String> {
        self.connection
            .query_row("SELECT value FROM settings WHERE key='enabled'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .map(|s| s.as_deref() == Some("1"))
            .map_err(error)
    }
    pub fn records(&self) -> Result<std::collections::HashMap<Vec<u8>, Record>, String> {
        let geometry = self.geometry_expression()?.replace("m.", "");
        let mut q = self
            .connection
            .prepare(&format!(
                "SELECT path,stamp,rotation,version,error,{geometry} FROM media"
            ))
            .map_err(error)?;
        q.query_map([], |r| {
            Ok((
                r.get(0)?,
                Record {
                    stamp: r.get(1)?,
                    rotation: r.get(2)?,
                    version: r.get(3)?,
                    error: r.get(4)?,
                    geometry_version: r.get(5)?,
                },
            ))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
    }
    #[cfg(test)]
    pub fn save(
        &mut self,
        path: &[u8],
        stamp: &[u8],
        rotation: u16,
        result: &Result<Vec<Face>, String>,
    ) -> Result<Vec<i64>, String> {
        self.save_for_model(path, stamp, rotation, result, VERSION)
    }
    pub fn save_for_model(
        &mut self,
        path: &[u8],
        stamp: &[u8],
        rotation: u16,
        result: &Result<Vec<Face>, String>,
        version: &str,
    ) -> Result<Vec<i64>, String> {
        let tx = self.connection.transaction().map_err(error)?;
        let previous: Option<(Vec<u8>, u16, String, String, Option<String>)> = tx
            .query_row(
                "SELECT stamp,rotation,version,geometry_version,error FROM media WHERE path=?1",
                [path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(error)?;
        let can_preserve = previous.as_ref().is_some_and(|(s, r, _v, g, _e)| {
            s == stamp && *r == rotation && g == crate::ai::faces::GEOMETRY_VERSION
        });
        let old: Vec<(i64, [f32; 4])> = if can_preserve {
            let mut q = tx
                .prepare("SELECT id,bbox FROM faces WHERE path=?1 AND active=1")
                .map_err(error)?;
            let raw = q
                .query_map([path], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            raw.into_iter()
                .map(|(id, b)| Ok((id, serde_json::from_str(&b).map_err(error)?)))
                .collect::<Result<_, String>>()?
        } else {
            Vec::new()
        };
        // Failed re-embedding leaves valid geometry and manual organization intact.
        if can_preserve && result.is_err() {
            tx.execute(
                "UPDATE media SET version=?2,error=?3 WHERE path=?1",
                params![path, version, result.as_ref().err()],
            )
            .map_err(error)?;
            tx.commit().map_err(error)?;
            return Ok(Vec::new());
        }
        let new_faces = result.as_ref().ok();
        let same = |a: &[f32; 4], b: &[f32; 4]| {
            a.iter()
                .zip(b)
                .all(|(x, y)| x.is_finite() && y.is_finite() && (x - y).abs() <= 1e-6)
        };
        let mapping: Vec<Option<i64>> = new_faces
            .map(|fs| {
                fs.iter()
                    .map(|f| {
                        let candidates: Vec<_> =
                            old.iter().filter(|(_, b)| same(b, &f.bbox)).collect();
                        if candidates.len() == 1
                            && fs
                                .iter()
                                .filter(|n| same(&n.bbox, &candidates[0].1))
                                .count()
                                == 1
                        {
                            Some(candidates[0].0)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let preserved: std::collections::HashSet<_> = mapping.iter().flatten().copied().collect();
        // Only unmatched old manual faces require review. Never use embeddings for correspondence.
        let mut q = tx
            .prepare("SELECT id FROM faces WHERE path=?1 AND active=1")
            .map_err(error)?;
        let old_ids = q
            .query_map([path], |r| r.get::<_, i64>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        drop(q);
        for id in old_ids {
            if !preserved.contains(&id) {
                tx.execute("UPDATE faces SET active=0,review=CASE WHEN manual=1 AND ignored=0 THEN 1 ELSE review END WHERE id=?1",[id]).map_err(error)?;
            }
        }
        let issue = result.as_ref().err().map(String::as_str);
        tx.execute("INSERT INTO media(path,stamp,rotation,version,error,geometry_version) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(path) DO UPDATE SET stamp=excluded.stamp,rotation=excluded.rotation,version=excluded.version,error=excluded.error,geometry_version=excluded.geometry_version",params![path,stamp,rotation,version,issue,if result.is_ok() {crate::ai::faces::GEOMETRY_VERSION}else{""}]).map_err(error)?;
        let mut ids = Vec::new();
        if let Ok(faces) = result {
            for (i, f) in faces.iter().enumerate() {
                let vector = f
                    .vector
                    .as_ref()
                    .map(|v| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
                let bbox = serde_json::to_string(&f.bbox).map_err(error)?;
                if let Some(id) = mapping[i] {
                    tx.execute(
                        "UPDATE faces SET bbox=?2,confidence=?3,vector=?4,thumbnail=?5 WHERE id=?1",
                        params![id, bbox, f.confidence, vector, f.thumbnail],
                    )
                    .map_err(error)?;
                    ids.push(id);
                } else {
                    tx.execute("INSERT INTO faces(path,bbox,confidence,vector,thumbnail) VALUES(?1,?2,?3,?4,?5)",params![path,bbox,f.confidence,vector,f.thumbnail]).map_err(error)?;
                    ids.push(tx.last_insert_rowid());
                }
            }
        }
        tx.commit().map_err(error)?;
        Ok(ids)
    }
    pub fn prune(&mut self, paths: &std::collections::HashSet<Vec<u8>>) -> Result<(), String> {
        let missing: Vec<_> = self
            .records()?
            .into_keys()
            .filter(|p| !paths.contains(p))
            .collect();
        let tx = self.connection.transaction().map_err(error)?;
        for p in missing {
            tx.execute("UPDATE faces SET active=0,review=CASE WHEN manual=1 AND ignored=0 THEN 1 ELSE review END WHERE path=?1",[&p]).map_err(error)?;
            tx.execute("DELETE FROM media WHERE path=?1", [&p])
                .map_err(error)?;
        }
        tx.commit().map_err(error)
    }
    fn person(&self, id: i64) -> Result<PersonState, String> {
        self.connection
            .query_row(
                "SELECT id,name,archived FROM people WHERE id=?1 AND archived=0",
                [id],
                |r| {
                    Ok(PersonState {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        archived: r.get(2)?,
                    })
                },
            )
            .map_err(|_| "人物已变化，请刷新。".into())
    }
    fn face(&self, id: i64) -> Result<FaceState, String> {
        let mut state = self
            .connection
            .query_row(
                "SELECT id,person_id,manual,ignored,review FROM faces WHERE id=?1",
                [id],
                |r| {
                    Ok(FaceState {
                        id: r.get(0)?,
                        person_id: r.get(1)?,
                        manual: r.get(2)?,
                        ignored: r.get(3)?,
                        review: r.get(4)?,
                        rejections: Vec::new(),
                    })
                },
            )
            .map_err(|_| "人脸已变化，请刷新。".to_string())?;
        let mut q = self
            .connection
            .prepare("SELECT person_id FROM rejections WHERE face_id=?1")
            .map_err(error)?;
        state.rejections = q
            .query_map([id], |r| r.get(0))
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?;
        Ok(state)
    }
    pub fn edit(&mut self, edit: Edit) -> Result<(), String> {
        self.edit_checked(edit, |_| Ok(()))
    }
    pub fn edit_checked(
        &mut self,
        edit: Edit,
        check: impl FnOnce(&Self) -> Result<(), String>,
    ) -> Result<(), String> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(error)?;
        check(self)?;
        self.apply(edit)?;
        tx.commit().map_err(error)
    }
    fn apply(&self, edit: Edit) -> Result<(), String> {
        let mut undo = Undo::default();
        let description;
        match edit {
            Edit::Rename { person_id, name: n } => {
                undo.people.push(self.person(person_id)?);
                let n = name(&n)?;
                self.connection
                    .execute(
                        "UPDATE people SET name=?2 WHERE id=?1",
                        params![person_id, n],
                    )
                    .map_err(error)?;
                description = "修改人物名称";
            }
            Edit::Merge {
                source_id,
                target_id,
            } => {
                if source_id == target_id {
                    return Err("请选择两个不同人物。".into());
                }
                undo.people.push(self.person(source_id)?);
                undo.people.push(self.person(target_id)?);
                let mut q = self
                    .connection
                    .prepare("SELECT id FROM faces WHERE person_id=?1")
                    .map_err(error)?;
                let ids = q
                    .query_map([source_id], |r| r.get::<_, i64>(0))
                    .map_err(error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(error)?;
                for id in ids {
                    undo.faces.push(self.face(id)?);
                }
                self.connection
                    .execute(
                        "UPDATE faces SET person_id=?2,manual=1 WHERE person_id=?1",
                        params![source_id, target_id],
                    )
                    .map_err(error)?;
                self.connection
                    .execute("UPDATE people SET archived=1 WHERE id=?1", [source_id])
                    .map_err(error)?;
                description = "合并人物";
            }
            Edit::Assign {
                face_ids,
                person_id,
                name: n,
            } => {
                if face_ids.is_empty() || face_ids.len() > 1000 {
                    return Err("请选择 1～1000 张人脸。".into());
                }
                let target = if let Some(id) = person_id {
                    self.person(id)?;
                    id
                } else {
                    let n = name(n.as_deref().unwrap_or(""))?;
                    self.connection
                        .execute("INSERT INTO people(name) VALUES(?1)", [n])
                        .map_err(error)?;
                    let id = self.connection.last_insert_rowid();
                    undo.created.push(id);
                    id
                };
                let mut seen = std::collections::HashSet::new();
                for id in face_ids {
                    if !seen.insert(id) {
                        continue;
                    }
                    let old = self.face(id)?;
                    let active: bool = self
                        .connection
                        .query_row("SELECT active FROM faces WHERE id=?1", [id], |r| r.get(0))
                        .map_err(error)?;
                    if !active {
                        return Err("旧标注需与当前检出的人脸核对，请在当前人脸上选择人物。".into());
                    }
                    if let Some(previous) = old.person_id
                        && previous != target
                    {
                        self.connection
                            .execute(
                                "INSERT OR IGNORE INTO rejections VALUES(?1,?2)",
                                params![id, previous],
                            )
                            .map_err(error)?;
                    }
                    self.connection
                        .execute(
                            "DELETE FROM rejections WHERE face_id=?1 AND person_id=?2",
                            params![id, target],
                        )
                        .map_err(error)?;
                    undo.faces.push(old);
                    self.connection
                        .execute(
                            "UPDATE faces SET person_id=?2,manual=1,ignored=0,review=0 WHERE id=?1",
                            params![id, target],
                        )
                        .map_err(error)?;
                }
                description = "调整人脸归属";
            }
            Edit::Ignore { face_ids } => return self.disposition(face_ids, false),
            Edit::Restore { face_ids } => return self.disposition(face_ids, true),
        }
        self.save_undo(description, undo)
    }
    fn disposition(&self, ids: Vec<i64>, restore: bool) -> Result<(), String> {
        if ids.is_empty() || ids.len() > 1000 {
            return Err("请选择 1～1000 张人脸。".into());
        }
        let mut undo = Undo::default();
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            if !seen.insert(id) {
                continue;
            }
            undo.faces.push(self.face(id)?);
            self.connection.execute("UPDATE faces SET ignored=?2,manual=1,review=CASE WHEN active=0 AND ?2=0 THEN 1 ELSE 0 END WHERE id=?1",params![id,!restore]).map_err(error)?;
        }
        self.save_undo(
            if restore {
                "恢复忽略的人脸"
            } else {
                "忽略人脸或旧标注"
            },
            undo,
        )
    }
    fn save_undo(&self, description: &str, undo: Undo) -> Result<(), String> {
        self.connection
            .execute(
                "INSERT INTO edits(description,payload) VALUES(?1,?2)",
                params![description, serde_json::to_string(&undo).map_err(error)?],
            )
            .map_err(error)?;
        self.connection.execute("DELETE FROM edits WHERE id NOT IN(SELECT id FROM edits ORDER BY id DESC LIMIT 100)",[]).map_err(error)?;
        Ok(())
    }
    pub fn undo(&mut self) -> Result<(), String> {
        let tx = self.connection.transaction().map_err(error)?;
        let row: Option<(i64, String)> = tx
            .query_row(
                "SELECT id,payload FROM edits ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((id, json)) = row else {
            return Err("没有可撤销的操作。".into());
        };
        let u: Undo = serde_json::from_str(&json).map_err(error)?;
        for p in u.people {
            tx.execute(
                "UPDATE people SET name=?2,archived=?3 WHERE id=?1",
                params![p.id, p.name, p.archived],
            )
            .map_err(error)?;
        }
        for f in u.faces {
            tx.execute("UPDATE faces SET person_id=?2,manual=?3,ignored=?4,review=CASE WHEN active=0 AND ?3=1 AND ?4=0 THEN 1 ELSE ?5 END WHERE id=?1",params![f.id,f.person_id,f.manual,f.ignored,f.review]).map_err(error)?;
            tx.execute("DELETE FROM rejections WHERE face_id=?1", [f.id])
                .map_err(error)?;
            for p in f.rejections {
                tx.execute("INSERT INTO rejections VALUES(?1,?2)", params![f.id, p])
                    .map_err(error)?;
            }
        }
        for p in u.created {
            // New faces may have been added since this edit; keep them pending.
            tx.execute("UPDATE faces SET person_id=NULL WHERE person_id=?1", [p])
                .map_err(error)?;
            tx.execute("DELETE FROM rejections WHERE person_id=?1", [p])
                .map_err(error)?;
            tx.execute("DELETE FROM people WHERE id=?1", [p])
                .map_err(error)?;
        }
        tx.execute("DELETE FROM edits WHERE id=?1", [id])
            .map_err(error)?;
        tx.commit().map_err(error)
    }
}
