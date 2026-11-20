//! Reusable read connection with an indexed, per-scan in-memory media catalog.
use super::{
    super::{self as library, Filter, ItemView, Kind, Page, Sort, favorites},
    store::Store,
};
use crate::ai::{error, faces::GEOMETRY_VERSION};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};
static READER: Mutex<Option<Reader>> = Mutex::new(None);
#[cfg(test)]
pub(super) fn close_for_test() {
    *READER.lock().unwrap() = None;
}

pub(super) fn close() -> Result<(), String> {
    *READER.lock().map_err(error)? = None;
    Ok(())
}

struct Reader {
    scan_id: u64,
    store: Store,
    favorites: HashSet<usize>,
    rotations: HashMap<usize, u16>,
    geometry_expression: String,
    schema_version: u32,
}
pub(super) fn with<T>(
    scan_id: u64,
    f: impl FnOnce(Option<&Store>) -> Result<T, String>,
) -> Result<T, String> {
    let scan = library::scan(scan_id)?;
    let (saved_favorites, saved_rotations) = {
        let s = library::session()?;
        let s = library::current(&s, scan_id)?;
        (s.favorites.clone(), s.rotations.clone())
    };
    let mut reader = READER.lock().map_err(error)?;
    let reset = match reader.as_ref() {
        Some(r) => {
            r.scan_id != scan_id
                || r.geometry_expression != r.store.geometry_expression()?
                || r.schema_version != r.store.schema_version()?
        }
        None => true,
    };
    if reset {
        *reader = None;
        if let Some(store) = Store::open(&scan, false)? {
            store.connection.execute_batch("CREATE TEMP TABLE current_media(path BLOB PRIMARY KEY,file_id INTEGER NOT NULL,captured TEXT,favorite INTEGER NOT NULL,rotation INTEGER NOT NULL,stamp BLOB NOT NULL) WITHOUT ROWID; BEGIN;").map_err(error)?;
            {
                let mut insert = store
                    .connection
                    .prepare("INSERT INTO current_media VALUES(?1,?2,?3,?4,?5,?6)")
                    .map_err(error)?;
                for item in scan
                    .items
                    .iter()
                    .filter(|i| i.kind == Kind::Photo && i.preview_available)
                {
                    insert
                        .execute(params![
                            favorites::path_key(&item.file.relative),
                            item.file.id as i64,
                            item.captured_on.map(|d| d.to_string()),
                            saved_favorites.contains(&item.file.id),
                            saved_rotations.get(&item.file.id).copied().unwrap_or(0),
                            item.file.stamp.portable_key()
                        ])
                        .map_err(error)?;
                }
            }
            store
                .connection
                .execute_batch("COMMIT; CREATE INDEX current_file_id ON current_media(file_id);")
                .map_err(error)?;
            let geometry = store.geometry_expression()?;
            let schema_version = store.schema_version()?;
            let ignored = if schema_version >= super::SCHEMA_VERSION {
                "ignored"
            } else {
                "0"
            };
            store.connection.execute_batch(&format!(
                "CREATE TEMP VIEW person_groups AS SELECT id,name,archived,{ignored} AS ignored FROM main.people;"
            )).map_err(error)?;
            store.connection.execute_batch(&format!("CREATE TEMP VIEW visible_faces AS SELECT f.*,c.file_id,c.captured,c.favorite FROM faces f JOIN media m ON m.path=f.path JOIN current_media c ON c.path=m.path AND c.stamp=m.stamp AND c.rotation=m.rotation WHERE f.active=1 AND ({geometry})='{GEOMETRY_VERSION}';")).map_err(error)?;
            store.connection.execute_batch(&format!("
                CREATE TEMP VIEW face_cards AS SELECT f.*,c.file_id,c.captured,c.favorite,
                  COALESCE(f.active=1 AND ({geometry})='{GEOMETRY_VERSION}' AND m.stamp=c.stamp AND m.rotation=c.rotation,0) valid
                  FROM faces f JOIN current_media c ON c.path=f.path LEFT JOIN media m ON m.path=f.path;
                CREATE TEMP VIEW pending_faces AS
                  WITH stale AS MATERIALIZED (SELECT c.path FROM current_media c LEFT JOIN media m ON m.path=c.path
                    WHERE m.path IS NULL OR ({geometry})!='{GEOMETRY_VERSION}' OR m.stamp!=c.stamp OR m.rotation!=c.rotation)
                  SELECT id FROM visible_faces WHERE ignored=0 AND person_id IS NULL
                  UNION ALL SELECT f.id FROM faces f JOIN current_media c ON c.path=f.path LEFT JOIN person_groups p ON p.id=f.person_id
                    WHERE f.active=0 AND f.review=1 AND f.manual=1 AND f.ignored=0 AND COALESCE(p.ignored,0)=0
                  UNION ALL SELECT f.id FROM stale s CROSS JOIN faces f LEFT JOIN person_groups p ON p.id=f.person_id
                    WHERE f.path=s.path AND f.active=1 AND f.manual=1 AND f.ignored=0 AND COALESCE(p.ignored,0)=0;
            ")).map_err(error)?;
            *reader = Some(Reader {
                scan_id,
                store,
                favorites: saved_favorites.clone(),
                rotations: saved_rotations.clone(),
                geometry_expression: geometry,
                schema_version,
            });
        }
    }
    if let Some(r) = reader.as_mut() {
        for &id in r.favorites.symmetric_difference(&saved_favorites) {
            r.store
                .connection
                .execute(
                    "UPDATE current_media SET favorite=?2 WHERE file_id=?1",
                    params![id as i64, saved_favorites.contains(&id)],
                )
                .map_err(error)?;
        }
        let ids: HashSet<_> = r
            .rotations
            .keys()
            .chain(saved_rotations.keys())
            .copied()
            .collect();
        for id in ids {
            let old = r.rotations.get(&id).copied().unwrap_or(0);
            let new = saved_rotations.get(&id).copied().unwrap_or(0);
            if old != new {
                r.store
                    .connection
                    .execute(
                        "UPDATE current_media SET rotation=?2 WHERE file_id=?1",
                        params![id as i64, new],
                    )
                    .map_err(error)?;
            }
        }
        r.favorites = saved_favorites;
        r.rotations = saved_rotations;
    }
    let result = f(reader.as_ref().map(|r| &r.store))?;
    library::scan(scan_id)?;
    Ok(result)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersonView {
    pub id: i64,
    pub name: String,
    pub photo_count: usize,
    pub face_count: usize,
    pub thumbnail: Option<String>,
}
#[derive(Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FaceScope {
    #[default]
    Pending,
    Person,
    Ignored,
    Photo,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FaceView {
    pub id: i64,
    pub file_id: usize,
    pub file_name: String,
    pub person_id: Option<i64>,
    pub person_name: Option<String>,
    pub thumbnail: String,
    pub bbox: [f32; 4],
    pub manual: bool,
    pub ignored: bool,
    pub person_ignored: bool,
    pub needs_review: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Month {
    pub month: Option<String>,
    pub count: usize,
}
pub(super) fn image(bytes: Vec<u8>) -> String {
    format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes))
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct PeopleFilter {
    ignored: bool,
    unnamed_only: bool,
    single_photo_only: bool,
}

pub(super) fn people(
    scan_id: u64,
    query: &str,
    page: usize,
    filter: PeopleFilter,
) -> Result<Page<PersonView>, String> {
    if query.len() > 1024 {
        return Err("搜索词过长。".into());
    }
    with(scan_id, |store| {
        let Some(s) = store else {
            return Ok(Page {
                page: 0,
                total: 0,
                items: Vec::new(),
            });
        };
        const MATCH: &str = "FROM person_groups g WHERE g.archived=0 AND g.ignored=?2
            AND (?1='' OR instr(lower(g.name),lower(?1))>0) AND (?3=0 OR g.name='')
            AND (?4=0 OR (SELECT COUNT(*) FROM (SELECT DISTINCT path FROM visible_faces
                WHERE person_id=g.id AND ignored=0 LIMIT 2))=1)";
        let args = params![
            query.trim(),
            filter.ignored,
            filter.unnamed_only,
            filter.single_photo_only
        ];
        let total = s
            .connection
            .query_row(&format!("SELECT COUNT(*) {MATCH}"), args, |r| {
                r.get::<_, i64>(0).map(|v| v as usize)
            })
            .map_err(error)?;
        let (page, offset) = Page::<PersonView>::bounds(page, total);
        let mut q=s.connection.prepare(&format!("WITH selected AS (SELECT g.id,g.name {MATCH} ORDER BY g.name='',g.name,g.id LIMIT 24 OFFSET ?5) SELECT p.id,p.name,(SELECT COUNT(DISTINCT path) FROM visible_faces WHERE person_id=p.id AND ignored=0),(SELECT COUNT(*) FROM visible_faces WHERE person_id=p.id AND ignored=0),(SELECT thumbnail FROM visible_faces WHERE person_id=p.id AND ignored=0 ORDER BY id LIMIT 1) FROM selected p ORDER BY p.name='',p.name,p.id")).map_err(error)?;
        let rows = q
            .query_map(
                params![
                    query.trim(),
                    filter.ignored,
                    filter.unnamed_only,
                    filter.single_photo_only,
                    offset as i64
                ],
                |r| {
                    Ok(PersonView {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        photo_count: r.get::<_, i64>(2)? as usize,
                        face_count: r.get::<_, i64>(3)? as usize,
                        thumbnail: r.get::<_, Option<Vec<u8>>>(4)?.map(image),
                    })
                },
            )
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        Ok(Page {
            page,
            total,
            items: rows,
        })
    })
}
// Old manual annotations are visible as review cards only; never counted as current photos.
fn face_where(scope: FaceScope) -> &'static str {
    match scope {
        FaceScope::Pending => "1",
        FaceScope::Person => {
            "f.person_id=?1 AND f.ignored=0 AND (f.valid=1 OR (p.ignored=1 AND f.manual=1 AND (f.active=1 OR f.review=1)))"
        }
        FaceScope::Ignored => "f.ignored=1",
        FaceScope::Photo => {
            "f.file_id=?2 AND (f.valid=1 OR (f.manual=1 AND f.ignored=0 AND ((f.active=0 AND f.review=1) OR f.active=1)))"
        }
    }
}

pub(super) fn faces(
    scan_id: u64,
    scope: FaceScope,
    person_id: Option<i64>,
    file_id: Option<usize>,
    page: usize,
) -> Result<Page<FaceView>, String> {
    let scan = library::scan(scan_id)?;
    with(scan_id, |store| {
        let Some(s) = store else {
            return Ok(Page {
                page: 0,
                total: 0,
                items: Vec::new(),
            });
        };
        let join = if matches!(scope, FaceScope::Pending) {
            "FROM pending_faces pending JOIN face_cards f ON f.id=pending.id LEFT JOIN person_groups p ON p.id=f.person_id"
        } else {
            "FROM face_cards f LEFT JOIN person_groups p ON p.id=f.person_id"
        };
        // All statements bind both placeholders, even scopes which do not use them.
        let condition = format!(
            "({}) AND (?1 IS NULL OR ?1 IS NOT NULL) AND (?2 IS NULL OR ?2 IS NOT NULL)",
            face_where(scope)
        );
        let total = s
            .connection
            .query_row(
                &format!("SELECT COUNT(*) {join} WHERE {condition}"),
                params![person_id, file_id.map(|id| id as i64)],
                |r| r.get::<_, i64>(0).map(|v| v as usize),
            )
            .map_err(error)?;
        let (page, offset) = Page::<FaceView>::bounds(page, total);
        let mut q=s.connection.prepare(&format!("SELECT f.id,f.file_id,f.person_id,p.name,f.thumbnail,f.bbox,f.manual,f.ignored,COALESCE(p.ignored,0),f.valid=0 {join} WHERE {condition} ORDER BY f.review DESC,f.id LIMIT 24 OFFSET ?3")).map_err(error)?;
        let rows = q
            .query_map(
                params![person_id, file_id.map(|id| id as i64), offset as i64],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)? as usize,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, bool>(6)?,
                        r.get::<_, bool>(7)?,
                        r.get::<_, bool>(8)?,
                        r.get::<_, bool>(9)?,
                    ))
                },
            )
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let mut items = Vec::new();
        for (
            id,
            file_id,
            person_id,
            person_name,
            thumb,
            bbox,
            manual,
            ignored,
            person_ignored,
            needs_review,
        ) in rows
        {
            items.push(FaceView {
                id,
                file_id,
                file_name: scan.item(file_id)?.file.path_label(),
                person_id,
                person_name,
                thumbnail: image(thumb),
                bbox: serde_json::from_str(&bbox).map_err(error)?,
                manual,
                ignored,
                person_ignored,
                needs_review,
            });
        }
        Ok(Page { page, total, items })
    })
}
fn conditions(
    s: &Store,
    scan: &library::Scan,
    person_id: i64,
    filter: &Filter,
) -> Result<(), String> {
    filter.validate_location()?;
    s.connection.execute_batch("CREATE TEMP TABLE IF NOT EXISTS location_match(file_id INTEGER PRIMARY KEY); DELETE FROM location_match;").map_err(error)?;
    if filter.location_mode != library::locations::LocationMode::All
        || filter.location_selection.is_some()
    {
        let mut insert = s
            .connection
            .prepare("INSERT INTO location_match VALUES(?1)")
            .map_err(error)?;
        for item in scan.items.iter().filter(|i| filter.matches_location(i)) {
            insert.execute([item.file.id as i64]).map_err(error)?;
        }
    }
    let from = library::parse_date(filter.from.as_deref())?;
    let to = library::parse_date(filter.to.as_deref())?;
    if matches!((from,to),(Some(a),Some(b)) if a>b) {
        return Err("开始日期应早于或等于结束日期。".into());
    }
    let exists: bool = s
        .connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM person_groups WHERE id=?1 AND archived=0 AND ignored=0)",
            [person_id],
            |r| r.get(0),
        )
        .map_err(error)?;
    if !exists {
        return Err("人物已变化，请返回人物列表。".into());
    }
    Ok(())
}
const PHOTO_CONDITION: &str = "person_id=?1 AND ignored=0 AND (?2=0 OR favorite=1) AND ((?3=1 AND captured IS NULL) OR (?3=0 AND (?4 IS NULL OR captured>=?4) AND (?5 IS NULL OR captured<=?5))) AND (?6=0 OR file_id IN (SELECT file_id FROM location_match))";
pub(super) fn photos(
    scan_id: u64,
    person_id: i64,
    filter: &Filter,
    sort: Sort,
    page: usize,
) -> Result<Page<ItemView>, String> {
    let scan = library::scan(scan_id)?;
    let (favs, rotations) = {
        let s = library::session()?;
        let s = library::current(&s, scan_id)?;
        (s.favorites.clone(), s.rotations.clone())
    };
    with(scan_id, |store| {
        let Some(s) = store else {
            return Ok(Page {
                page: 0,
                total: 0,
                items: Vec::new(),
            });
        };
        conditions(s, &scan, person_id, filter)?;
        let bind = params![
            person_id,
            filter.favorites_only,
            filter.unknown_date,
            filter.from,
            filter.to,
            filter.location_mode != library::locations::LocationMode::All
                || filter.location_selection.is_some()
        ];
        let total = s
            .connection
            .query_row(
                &format!("SELECT COUNT(DISTINCT path) FROM visible_faces WHERE {PHOTO_CONDITION}"),
                bind,
                |r| r.get::<_, i64>(0).map(|v| v as usize),
            )
            .map_err(error)?;
        let (page, offset) = Page::<ItemView>::bounds(page, total);
        let direction = if matches!(sort, Sort::DateDesc) {
            "DESC"
        } else {
            "ASC"
        };
        let mut q=s.connection.prepare(&format!("SELECT DISTINCT file_id,captured,path FROM visible_faces WHERE {PHOTO_CONDITION} ORDER BY captured IS NULL,captured {direction},path LIMIT 24 OFFSET ?7")).map_err(error)?;
        let ids = q
            .query_map(
                params![
                    person_id,
                    filter.favorites_only,
                    filter.unknown_date,
                    filter.from,
                    filter.to,
                    filter.location_mode != library::locations::LocationMode::All
                        || filter.location_selection.is_some(),
                    offset as i64
                ],
                |r| r.get::<_, i64>(0).map(|v| v as usize),
            )
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        Ok(Page {
            page,
            total,
            items: ids
                .into_iter()
                .map(|id| {
                    scan.item(id).map(|i| {
                        i.view(favs.contains(&id), rotations.get(&id).copied().unwrap_or(0))
                    })
                })
                .collect::<Result<_, _>>()?,
        })
    })
}
pub(super) fn months(
    scan_id: u64,
    person_id: i64,
    filter: &Filter,
    sort: Sort,
) -> Result<Vec<Month>, String> {
    let scan = library::scan(scan_id)?;
    with(scan_id, |store| {
        let Some(s) = store else {
            return Ok(Vec::new());
        };
        conditions(s, &scan, person_id, filter)?;
        let direction = if matches!(sort, Sort::DateDesc) {
            "DESC"
        } else {
            "ASC"
        };
        let mut q=s.connection.prepare(&format!("SELECT substr(captured,1,7) month,COUNT(DISTINCT path) FROM visible_faces WHERE {PHOTO_CONDITION} GROUP BY month ORDER BY month IS NULL,month {direction}")).map_err(error)?;
        q.query_map(
            params![
                person_id,
                filter.favorites_only,
                filter.unknown_date,
                filter.from,
                filter.to,
                filter.location_mode != library::locations::LocationMode::All
                    || filter.location_selection.is_some()
            ],
            |r| {
                Ok(Month {
                    month: r.get(0)?,
                    count: r.get::<_, i64>(1)? as usize,
                })
            },
        )
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
    })
}
pub(super) fn counts(scan_id: u64) -> Result<(usize, usize, usize, Option<String>), String> {
    with(scan_id, |store| {
        let Some(s) = store else {
            return Ok((0, 0, 0, None));
        };
        let pending = s
            .connection
            .query_row("SELECT COUNT(*) FROM pending_faces", [], |r| {
                r.get::<_, i64>(0).map(|v| v as usize)
            })
            .map_err(error)?;
        let ignored=s.connection.query_row("SELECT COUNT(*) FROM faces f JOIN current_media c ON c.path=f.path WHERE f.ignored=1",[],|r|r.get::<_,i64>(0).map(|v|v as usize)).map_err(error)?;
        let ignored_people = s
            .connection
            .query_row(
                "SELECT COUNT(*) FROM person_groups WHERE archived=0 AND ignored=1",
                [],
                |r| r.get::<_, i64>(0).map(|v| v as usize),
            )
            .map_err(error)?;
        let undo = s
            .connection
            .query_row(
                "SELECT description FROM edits ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        Ok((pending, ignored, ignored_people, undo))
    })
}
pub(super) fn validate_faces(
    scan_id: u64,
    ids: &[i64],
    require_current: bool,
) -> Result<(), String> {
    with(scan_id, |store| {
        let s = store.ok_or("请先建立人物索引。")?;
        for id in ids {
            let exists:bool=s.connection.query_row("SELECT EXISTS(SELECT 1 FROM face_cards f WHERE f.id=?1 AND ((?2=1 AND f.valid=1) OR (?2=0 AND (f.ignored=1 OR f.review=1 OR f.manual=1 OR f.valid=1))))",params![id,require_current],|r|r.get(0)).map_err(error)?;
            if !exists {
                return Err("人脸来源照片已变化，请刷新人物列表。".into());
            }
        }
        Ok(())
    })
}

#[derive(Serialize)]
pub(crate) struct Identity {
    pub id: i64,
    pub name: String,
}
pub(super) fn photo_people(scan_id: u64, file_id: usize) -> Result<Vec<Identity>, String> {
    with(scan_id, |store| {
        let Some(s) = store else {
            return Ok(Vec::new());
        };
        let mut q=s.connection.prepare("SELECT DISTINCT p.id,p.name FROM visible_faces v JOIN person_groups p ON p.id=v.person_id WHERE v.file_id=?1 AND v.ignored=0 AND p.archived=0 AND p.ignored=0 ORDER BY p.name,p.id").map_err(error)?;
        q.query_map([file_id as i64], |r| {
            Ok(Identity {
                id: r.get(0)?,
                name: r.get(1)?,
            })
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
    })
}

pub(super) fn photo_ids(
    scan_id: u64,
    person_id: i64,
    filter: &Filter,
) -> Result<Vec<usize>, String> {
    let scan = library::scan(scan_id)?;
    with(scan_id, |store| {
        let Some(s) = store else {
            return Ok(Vec::new());
        };
        conditions(s, &scan, person_id, filter)?;
        let mut query = s
            .connection
            .prepare(&format!(
                "SELECT DISTINCT file_id FROM visible_faces WHERE {PHOTO_CONDITION}"
            ))
            .map_err(error)?;
        query
            .query_map(
                params![
                    person_id,
                    filter.favorites_only,
                    filter.unknown_date,
                    filter.from,
                    filter.to,
                    filter.location_mode != library::locations::LocationMode::All
                        || filter.location_selection.is_some()
                ],
                |r| r.get::<_, i64>(0).map(|id| id as usize),
            )
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)
    })
}
