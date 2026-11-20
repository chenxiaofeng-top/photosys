use super::super::{Scan, favorites};
use crate::ai::{
    error,
    faces::{Face, VERSION},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    time::Duration,
};

pub(super) struct Store {
    pub connection: Connection,
}
pub(crate) const SCHEMA_VERSION: u32 = 5;
const SCHEMA:&str="
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS people(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL DEFAULT '',archived INTEGER NOT NULL DEFAULT 0,ignored INTEGER NOT NULL DEFAULT 0,important INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS media(path BLOB PRIMARY KEY,stamp BLOB NOT NULL,rotation INTEGER NOT NULL,version TEXT NOT NULL,error TEXT);
CREATE TABLE IF NOT EXISTS faces(id INTEGER PRIMARY KEY AUTOINCREMENT,path BLOB NOT NULL,bbox TEXT NOT NULL,confidence REAL NOT NULL,vector BLOB,thumbnail BLOB NOT NULL,person_id INTEGER REFERENCES people(id),manual INTEGER NOT NULL DEFAULT 0,ignored INTEGER NOT NULL DEFAULT 0,active INTEGER NOT NULL DEFAULT 1,review INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS faces_path ON faces(path,active);
CREATE INDEX IF NOT EXISTS faces_person ON faces(person_id,active,ignored);
CREATE INDEX IF NOT EXISTS faces_pending ON faces(active,ignored,person_id,review);
CREATE TABLE IF NOT EXISTS rejections(face_id INTEGER NOT NULL REFERENCES faces(id),person_id INTEGER NOT NULL REFERENCES people(id),PRIMARY KEY(face_id,person_id));
CREATE TABLE IF NOT EXISTS edits(id INTEGER PRIMARY KEY AUTOINCREMENT,description TEXT NOT NULL,payload TEXT NOT NULL);";
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
    #[serde(default)]
    ignored: bool,
    #[serde(default)]
    important: bool,
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
pub(super) struct Undo {
    people: Vec<PersonState>,
    faces: Vec<FaceState>,
    created: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_assignments: Vec<(i64, i64)>,
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
    SetImportant {
        person_id: i64,
        important: bool,
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
    Unassign {
        face_ids: Vec<i64>,
    },
    Ignore {
        face_ids: Vec<i64>,
    },
    Restore {
        face_ids: Vec<i64>,
    },
    IgnorePeople {
        person_ids: Vec<i64>,
    },
    RestorePeople {
        person_ids: Vec<i64>,
    },
    DeletePeople {
        person_ids: Vec<i64>,
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
    // The same evidence is used for the writable migration and the read-only projection.
    pub(super) fn classify_legacy(
        &self,
        ignored: &str,
        important: &str,
    ) -> Result<Vec<(i64, String, Undo)>, String> {
        let c = &self.connection;
        let mut q = c
            .prepare("SELECT id,description,payload FROM edits ORDER BY id")
            .map_err(error)?;
        let mut edits = q
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(error)?
            .map(|row| {
                let (id, description, json) = row.map_err(error)?;
                let undo = serde_json::from_str::<Undo>(&json).map_err(error)?;
                Ok((id, description, undo))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let mut evidence = HashSet::new();
        let mut confirmed = HashMap::new();
        let mut dispositions = HashMap::new();
        for (_, description, undo) in &mut edits {
            evidence.extend(undo.created.iter().copied());
            let manages_person = matches!(
                description.as_str(),
                "修改人物名称"
                    | "标记重要人物"
                    | "取消重要人物标记"
                    | "忽略人物"
                    | "恢复忽略的人物"
                    | "合并人物"
            );
            for person in &undo.people {
                if manages_person
                    || !person.name.trim().is_empty()
                    || person.important
                    || person.ignored
                {
                    evidence.insert(person.id);
                }
            }
            let is_disposition =
                matches!(description.as_str(), "忽略人脸或旧标注" | "恢复忽略的人脸");
            for face in &mut undo.faces {
                let previous = face.person_id;
                if let Some(person) = previous {
                    let manually_assigned =
                        face.manual && dispositions.get(&face.id) != Some(&person);
                    if manually_assigned || confirmed.get(&face.id) == Some(&person) {
                        evidence.insert(person);
                        face.manual = true;
                    } else {
                        face.person_id = None;
                    }
                }
                if is_disposition {
                    if face.person_id.is_none()
                        && let Some(person) = previous
                    {
                        dispositions.insert(face.id, person);
                    }
                } else {
                    dispositions.remove(&face.id);
                    confirmed.remove(&face.id);
                }
            }
            for &(face, person) in &undo.pending_assignments {
                evidence.insert(person);
                confirmed.insert(face, person);
                dispositions.remove(&face);
            }
        }

        c.execute_batch("CREATE TEMP TABLE legacy_people(id INTEGER PRIMARY KEY);
            CREATE TEMP TABLE legacy_confirmations(id INTEGER PRIMARY KEY,person_id INTEGER NOT NULL);
            CREATE TEMP TABLE legacy_dispositions(id INTEGER PRIMARY KEY,person_id INTEGER NOT NULL);")
            .map_err(error)?;
        for (table, pairs) in [
            ("legacy_confirmations", confirmed),
            ("legacy_dispositions", dispositions),
        ] {
            let mut insert = c
                .prepare(&format!("INSERT INTO temp.{table} VALUES(?1,?2)"))
                .map_err(error)?;
            for (face, person) in pairs {
                insert.execute(params![face, person]).map_err(error)?;
            }
        }
        let mut insert = c
            .prepare("INSERT OR IGNORE INTO temp.legacy_people VALUES(?1)")
            .map_err(error)?;
        for person in evidence {
            insert.execute([person]).map_err(error)?;
        }
        c.execute_batch(&format!(
            "INSERT OR IGNORE INTO temp.legacy_people
            SELECT id FROM main.people WHERE trim(name)!='' OR ({ignored}) OR ({important});
            INSERT OR IGNORE INTO temp.legacy_people
            SELECT f.person_id FROM main.faces f LEFT JOIN temp.legacy_dispositions d
                ON d.id=f.id AND d.person_id=f.person_id
            WHERE f.manual=1 AND f.person_id IS NOT NULL AND d.id IS NULL;"
        ))
        .map_err(error)?;
        let mut known = c
            .prepare("SELECT EXISTS(SELECT 1 FROM temp.legacy_people WHERE id=?1)")
            .map_err(error)?;
        for (_, _, undo) in &mut edits {
            for person in &mut undo.people {
                if !known
                    .query_row([person.id], |row| row.get::<_, bool>(0))
                    .map_err(error)?
                {
                    person.archived = true;
                }
            }
        }
        Ok(edits)
    }

    fn migrate_automatic_groups(&self) -> Result<(), String> {
        let edits = self.classify_legacy("ignored", "important")?;
        self.connection.execute_batch("UPDATE main.faces AS f SET manual=1
            WHERE EXISTS(SELECT 1 FROM temp.legacy_confirmations c WHERE c.id=f.id AND c.person_id=f.person_id);
            UPDATE main.faces AS f SET person_id=NULL WHERE f.person_id IS NOT NULL
            AND NOT EXISTS(SELECT 1 FROM temp.legacy_confirmations c WHERE c.id=f.id AND c.person_id=f.person_id)
            AND (f.manual=0 OR EXISTS(SELECT 1 FROM temp.legacy_dispositions d WHERE d.id=f.id AND d.person_id=f.person_id));
            UPDATE main.people SET archived=1 WHERE id NOT IN(SELECT id FROM temp.legacy_people);").map_err(error)?;
        let mut update = self
            .connection
            .prepare("UPDATE edits SET payload=?2 WHERE id=?1")
            .map_err(error)?;
        for (id, _, undo) in edits {
            update
                .execute(params![id, serde_json::to_string(&undo).map_err(error)?])
                .map_err(error)?;
        }
        self.connection
            .execute_batch(
                "DROP TABLE temp.legacy_people;
            DROP TABLE temp.legacy_confirmations; DROP TABLE temp.legacy_dispositions;",
            )
            .map_err(error)
    }

    fn has_column(c: &Connection, table: &str, column: &str) -> Result<bool, String> {
        c.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
            params![table, column],
            |r| r.get(0),
        )
        .map_err(error)
    }
    pub fn geometry_expression(&self) -> Result<String, String> {
        Ok(
            if Self::has_column(&self.connection, "media", "geometry_version")? {
                "m.geometry_version".into()
            } else {
                format!(
                    "CASE WHEN m.version='{VERSION}' THEN '{}' ELSE '' END",
                    crate::ai::faces::GEOMETRY_VERSION
                )
            },
        )
    }

    pub fn schema_version(&self) -> Result<u32, String> {
        self.connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(error)
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
        let store = Self { connection: c };
        let version = store.schema_version()?;
        if version > SCHEMA_VERSION {
            return Err("人物数据库来自较新版本，请更新 PhotoSys。".into());
        }
        if write && version < SCHEMA_VERSION {
            let c = &store.connection;
            c.execute_batch("BEGIN IMMEDIATE").map_err(error)?;
            let migration = (|| -> Result<(), String> {
                c.execute_batch(SCHEMA).map_err(error)?;
                if !Self::has_column(c, "media", "geometry_version")? {
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
                if !Self::has_column(c, "people", "ignored")? {
                    c.execute_batch(
                        "ALTER TABLE people ADD COLUMN ignored INTEGER NOT NULL DEFAULT 0;",
                    )
                    .map_err(error)?;
                }
                if !Self::has_column(c, "people", "important")? {
                    c.execute_batch(
                        "ALTER TABLE people ADD COLUMN important INTEGER NOT NULL DEFAULT 0;",
                    )
                    .map_err(error)?;
                }
                if version < 5 {
                    store.migrate_automatic_groups()?;
                }
                c.pragma_update(None, "user_version", SCHEMA_VERSION)
                    .map_err(error)?;
                Ok(())
            })();
            if let Err(e) = migration {
                let _ = c.execute_batch("ROLLBACK");
                return Err(e);
            }
            c.execute_batch("COMMIT").map_err(error)?;
        }
        Ok(Some(store))
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
                "SELECT id,name,archived,ignored,important FROM people WHERE id=?1 AND archived=0",
                [id],
                |r| {
                    Ok(PersonState {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        archived: r.get(2)?,
                        ignored: r.get(3)?,
                        important: r.get(4)?,
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
    fn person_faces(&self, person_id: i64) -> Result<Vec<FaceState>, String> {
        let mut query = self
            .connection
            .prepare("SELECT id FROM faces WHERE person_id=?1")
            .map_err(error)?;
        let ids = query
            .query_map([person_id], |row| row.get::<_, i64>(0))
            .map_err(error)?;
        ids.map(|id| self.face(id.map_err(error)?)).collect()
    }
    pub fn edit(&mut self, edit: Edit) -> Result<(), String> {
        self.edit_checked(edit, |_| Ok(()))
    }
    pub fn edit_checked<T>(
        &mut self,
        edit: Edit,
        check: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<(), String> {
        self.apply_edit(edit, false, check)
    }
    pub fn edit_pending_group(&mut self, edit: Edit) -> Result<(), String> {
        self.apply_edit(edit, true, |_| Ok(()))
    }
    fn apply_edit<T>(
        &mut self,
        edit: Edit,
        pending_group: bool,
        check: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<(), String> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(error)?;
        let _validated = check(self)?;
        self.apply(edit, pending_group)?;
        tx.commit().map_err(error)
    }
    fn apply(&self, edit: Edit, pending_group: bool) -> Result<(), String> {
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
            Edit::SetImportant {
                person_id,
                important,
            } => {
                let previous = self.person(person_id)?;
                if previous.ignored {
                    return Err("请先恢复该人物。".into());
                }
                if previous.important == important {
                    return Ok(());
                }
                undo.people.push(previous);
                self.connection
                    .execute(
                        "UPDATE people SET important=?2 WHERE id=?1",
                        params![person_id, important],
                    )
                    .map_err(error)?;
                description = if important {
                    "标记重要人物"
                } else {
                    "取消重要人物标记"
                };
            }
            Edit::Merge {
                source_id,
                target_id,
            } => {
                if source_id == target_id {
                    return Err("请选择两个不同人物。".into());
                }
                let source = self.person(source_id)?;
                let target = self.person(target_id)?;
                if source.ignored || target.ignored {
                    return Err("请先恢复已忽略的人物再合并。".into());
                }
                if source.important && !target.important {
                    self.connection
                        .execute("UPDATE people SET important=1 WHERE id=?1", [target_id])
                        .map_err(error)?;
                }
                undo.people.extend([source, target]);
                undo.faces = self.person_faces(source_id)?;
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
                if face_ids.is_empty() || (!pending_group && face_ids.len() > 1000) {
                    return Err("请选择 1～1000 张人脸。".into());
                }
                let needs_pending = pending_group || person_id.is_none();
                let target = if let Some(id) = person_id {
                    if self.person(id)?.ignored {
                        return Err("归属目标已被忽略，请选择其他人物。".into());
                    }
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
                    if needs_pending && (old.person_id.is_some() || old.ignored || old.review) {
                        return Err("请选择仍在待整理中的当前人脸。".into());
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
            Edit::Unassign { face_ids } => {
                if face_ids.is_empty() || face_ids.len() > 1000 {
                    return Err("请选择 1～1000 张人脸。".into());
                }
                let mut seen = HashSet::new();
                for id in face_ids.into_iter().filter(|id| seen.insert(*id)) {
                    let old = self.face(id)?;
                    let Some(previous) = old.person_id.filter(|_| !old.ignored && !old.review)
                    else {
                        return Err("请选择已归入人物的当前人脸。".into());
                    };
                    self.connection
                        .execute(
                            "INSERT OR IGNORE INTO rejections VALUES(?1,?2)",
                            params![id, previous],
                        )
                        .map_err(error)?;
                    self.connection
                        .execute("UPDATE faces SET person_id=NULL,manual=1 WHERE id=?1", [id])
                        .map_err(error)?;
                    undo.faces.push(old);
                }
                description = "移回待整理";
            }
            Edit::Ignore { face_ids } => return self.disposition(face_ids, false, pending_group),
            Edit::Restore { face_ids } => return self.disposition(face_ids, true, false),
            Edit::IgnorePeople { person_ids } => return self.people_disposition(person_ids, true),
            Edit::RestorePeople { person_ids } => {
                return self.people_disposition(person_ids, false);
            }
            Edit::DeletePeople { person_ids } => return self.delete_people(person_ids),
        }
        self.save_undo(description, undo)
    }
    fn people_disposition(&self, ids: Vec<i64>, ignored: bool) -> Result<(), String> {
        if ids.is_empty() || ids.len() > 1000 {
            return Err("请选择 1～1000 个人物。".into());
        }
        let mut undo = Undo::default();
        let mut seen = std::collections::HashSet::new();
        for id in ids.into_iter().filter(|id| seen.insert(*id)) {
            let previous = self.person(id)?;
            if previous.ignored != ignored {
                undo.people.push(previous);
                self.connection
                    .execute(
                        "UPDATE people SET ignored=?2 WHERE id=?1",
                        params![id, ignored],
                    )
                    .map_err(error)?;
            }
        }
        if undo.people.is_empty() {
            return Ok(());
        }
        self.save_undo(
            if ignored {
                "忽略人物"
            } else {
                "恢复忽略的人物"
            },
            undo,
        )
    }

    fn delete_people(&self, person_ids: Vec<i64>) -> Result<(), String> {
        if person_ids.is_empty() || person_ids.len() > 1000 {
            return Err("请选择 1～1000 个人物。".into());
        }
        let mut undo = Undo::default();
        let mut seen = std::collections::HashSet::new();
        for id in person_ids.into_iter().filter(|id| seen.insert(*id)) {
            let person = self.person(id)?;
            if person.ignored {
                return Err("请先恢复已忽略的人物再删除。".into());
            }
            undo.people.push(person);
            undo.faces.extend(self.person_faces(id)?);
            self.connection
                .execute(
                    "UPDATE faces SET person_id=NULL,
                     manual=CASE WHEN active=1 AND ignored=0 THEN 1 ELSE manual END
                     WHERE person_id=?1",
                    [id],
                )
                .map_err(error)?;
            self.connection
                .execute("UPDATE people SET archived=1 WHERE id=?1", [id])
                .map_err(error)?;
        }
        self.save_undo("删除人物", undo)
    }

    fn disposition(&self, ids: Vec<i64>, restore: bool, pending_group: bool) -> Result<(), String> {
        if ids.is_empty() || (!pending_group && ids.len() > 1000) {
            return Err("请选择 1～1000 张人脸。".into());
        }
        let mut undo = Undo::default();
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            if !seen.insert(id) {
                continue;
            }
            let old = self.face(id)?;
            if pending_group && (old.person_id.is_some() || old.ignored || old.review) {
                return Err("请选择仍在待整理中的当前人脸。".into());
            }
            undo.faces.push(old);
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
                "UPDATE people SET name=?2,archived=?3,ignored=?4,important=?5 WHERE id=?1",
                params![p.id, p.name, p.archived, p.ignored, p.important],
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
        if !u.pending_assignments.is_empty() {
            let mut update = tx
                .prepare("UPDATE faces SET person_id=NULL,manual=0 WHERE id=?1 AND person_id=?2")
                .map_err(error)?;
            for (face, person) in u.pending_assignments {
                update.execute(params![face, person]).map_err(error)?;
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
