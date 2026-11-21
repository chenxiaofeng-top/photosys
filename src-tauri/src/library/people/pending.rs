//! Derived groups for the pending queue. Group IDs never reference the people table.
use super::{Page, error, grouping, query, store::Store};
use crate::ai::faces;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashSet},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

pub(super) const CHANGED: &str = "待整理分组已变化，请刷新后重新选择。";
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

struct Cache {
    scan_id: u64,
    data_version: i64,
    feature_version: String,
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct View {
    pub(super) token: String,
    pub(super) group_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Selection {
    Selected { face_ids: Vec<i64> },
    All { excluded_face_ids: Vec<i64> },
}

pub(super) struct GroupSelection {
    pub(super) chosen: Vec<(i64, usize)>,
    pub(super) remaining: Vec<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResumeGroup {
    pub(super) group_id: i64,
    pub(super) face_ids: Vec<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Group {
    id: i64,
    count: usize,
    thumbnail: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Overview {
    token: String,
    resumed_group_id: Option<i64>,
    ungrouped_count: usize,
    groups: Page<Group>,
}

pub(super) fn invalidate() -> Result<(), String> {
    *CACHE.lock().map_err(error)? = None;
    Ok(())
}

struct Representative {
    vector: Vec<f32>,
    photos: HashSet<usize>,
    members: Vec<i64>,
}

fn cluster(
    rows: impl Iterator<Item = Result<(i64, usize, Vec<u8>), rusqlite::Error>>,
    policy: &grouping::Policy,
) -> Result<BTreeMap<i64, Representative>, String> {
    let index = grouping::representative_index()?;
    let mut groups: BTreeMap<i64, Representative> = BTreeMap::new();
    for row in rows {
        let (id, file, bytes) = row.map_err(error)?;
        let Some(vector) = grouping::vector(&bytes) else {
            continue;
        };
        let mut ranked = Vec::new();
        if !groups.is_empty() {
            for key in index.search(&vector, 32).map_err(error)?.keys {
                let group = &groups[&(key as i64)];
                ranked.push((key as i64, faces::cosine(&vector, &group.vector)));
            }
            ranked.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        }
        if let Some(&(group_id, score)) = ranked
            .first()
            .filter(|(_, score)| *score >= policy.threshold)
        {
            let group = groups.get_mut(&group_id).expect("indexed representative");
            let ambiguous = ranked.get(1).is_some_and(|&(_, other)| {
                let gap = score - other;
                gap <= 0. || gap < policy.margin
            });
            if !ambiguous && group.photos.insert(file) {
                group.members.push(id);
            }
        } else {
            if index.size() == index.capacity() {
                index.reserve((index.size() * 2).max(64)).map_err(error)?;
            }
            index.add(id as u64, &vector).map_err(error)?;
            groups.insert(
                id,
                Representative {
                    vector,
                    photos: HashSet::from([file]),
                    members: vec![id],
                },
            );
        }
    }
    groups.retain(|_, group| group.members.len() > 1);
    Ok(groups)
}

// Called from within query::with, which serializes access to the reader and its temp tables.
fn ensure(store: &Store, scan_id: u64) -> Result<String, String> {
    let c = &store.connection;
    let mut cache = CACHE.lock().map_err(error)?;
    let data_version = c
        .pragma_query_value(None, "data_version", |r| r.get(0))
        .map_err(error)?;
    let spec = crate::ai::catalog::active(crate::ai::ModelKind::People);
    if let Some(cache) = cache.as_ref()
        && cache.scan_id == scan_id
        && cache.data_version == data_version
        && cache.feature_version == spec.feature_version
    {
        return Ok(cache.token.clone());
    }

    let tx = c.unchecked_transaction().map_err(error)?;
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS pending_group_members(
        face_id INTEGER PRIMARY KEY,group_id INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS temp.pending_group_members_by_group
        ON pending_group_members(group_id,face_id);
        DELETE FROM temp.pending_group_members;",
    )
    .map_err(error)?;
    let policy = grouping::Policy::effective(Some(store))?;
    if policy.allowed() {
        let mut select = tx
            .prepare(&format!(
                "SELECT f.id,f.file_id,f.vector
            FROM visible_faces f JOIN media m ON m.path=f.path
            WHERE m.version=?1 AND {} ORDER BY f.id",
                grouping::PENDING_FACE
            ))
            .map_err(error)?;
        let rows = select
            .query_map([&policy.version], |r| {
                Ok((r.get(0)?, r.get::<_, i64>(1)? as usize, r.get(2)?))
            })
            .map_err(error)?;
        let groups = cluster(rows, &policy)?;
        let mut insert = tx
            .prepare("INSERT INTO temp.pending_group_members VALUES(?1,?2)")
            .map_err(error)?;
        for (id, group) in groups {
            for member in group.members {
                insert.execute(params![member, id]).map_err(error)?;
            }
        }
    }
    tx.commit().map_err(error)?;
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed).to_string();
    *cache = Some(Cache {
        scan_id,
        data_version,
        feature_version: spec.feature_version.into(),
        token: token.clone(),
    });
    Ok(token)
}

pub(super) fn check_view(store: &Store, scan_id: u64, view: &View) -> Result<(), String> {
    if ensure(store, scan_id)? != view.token {
        return Err(CHANGED.into());
    }
    Ok(())
}

fn resumed_group_id(store: &Store, resume: &ResumeGroup) -> Result<Option<i64>, String> {
    let mut lookup = store
        .connection
        .prepare("SELECT group_id FROM temp.pending_group_members WHERE face_id=?1")
        .map_err(error)?;
    let mut counts = BTreeMap::<i64, usize>::new();
    for face_id in resume.face_ids.iter().copied().collect::<HashSet<_>>() {
        let mut rows = lookup.query([face_id]).map_err(error)?;
        if let Some(row) = rows.next().map_err(error)? {
            *counts.entry(row.get(0).map_err(error)?).or_default() += 1;
        }
    }
    Ok(counts
        .into_iter()
        .max_by_key(|&(id, count)| (count, id == resume.group_id, Reverse(id)))
        .map(|(id, _)| id))
}

pub(super) fn overview(
    scan_id: u64,
    page: usize,
    resume: Option<&ResumeGroup>,
) -> Result<Overview, String> {
    query::with(scan_id, |store| {
        let Some(store) = store else {
            return Ok(Overview {
                token: String::new(),
                resumed_group_id: None,
                ungrouped_count: 0,
                groups: Page {
                    items: Vec::new(),
                    page: 0,
                    total: 0,
                },
            });
        };
        let token = ensure(store, scan_id)?;
        let resumed_group_id = match resume {
            Some(resume) => resumed_group_id(store, resume)?,
            None => None,
        };
        let c = &store.connection;
        let ungrouped_count = c
            .query_row(
                "SELECT COUNT(*) FROM pending_faces p
            WHERE NOT EXISTS(SELECT 1 FROM temp.pending_group_members g WHERE g.face_id=p.id)",
                [],
                |r| r.get::<_, i64>(0).map(|n| n as usize),
            )
            .map_err(error)?;
        let total = c
            .query_row(
                "SELECT COUNT(DISTINCT group_id) FROM temp.pending_group_members",
                [],
                |r| r.get::<_, i64>(0).map(|n| n as usize),
            )
            .map_err(error)?;
        let (page, offset) = Page::<Group>::bounds(page, total);
        let mut rows = c
            .prepare(
                "WITH groups AS (
            SELECT group_id,COUNT(*) count FROM temp.pending_group_members
            GROUP BY group_id ORDER BY count DESC,group_id LIMIT ?1 OFFSET ?2)
            SELECT g.group_id,g.count,f.thumbnail FROM groups g JOIN faces f ON f.id=g.group_id
            ORDER BY g.count DESC,g.group_id",
            )
            .map_err(error)?;
        let items = rows
            .query_map(
                params![super::super::PAGE_SIZE as i64, offset as i64],
                |r| {
                    Ok(Group {
                        id: r.get(0)?,
                        count: r.get::<_, i64>(1)? as usize,
                        thumbnail: query::image(r.get(2)?),
                    })
                },
            )
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        Ok(Overview {
            token,
            resumed_group_id,
            ungrouped_count,
            groups: Page { items, page, total },
        })
    })
}

pub(super) fn selected(
    scan_id: u64,
    view: &View,
    selection: Selection,
) -> Result<GroupSelection, String> {
    let group_id = view.group_id.ok_or(CHANGED)?;
    let (all, ids) = match selection {
        Selection::All { excluded_face_ids } => (true, excluded_face_ids),
        Selection::Selected { face_ids } => (false, face_ids),
    };
    let len = ids.len();
    let mut ids: HashSet<_> = ids.into_iter().collect();
    if ids.len() != len {
        return Err(CHANGED.into());
    }
    query::with(scan_id, |store| {
        let store = store.ok_or(CHANGED)?;
        check_view(store, scan_id, view)?;
        let mut query = store
            .connection
            .prepare(
                "SELECT f.id,f.file_id
            FROM temp.pending_group_members g JOIN visible_faces f ON f.id=g.face_id
            WHERE g.group_id=?1 AND f.person_id IS NULL AND f.ignored=0 AND f.review=0
            ORDER BY f.id",
            )
            .map_err(error)?;
        let rows = query
            .query_map([group_id], |r| {
                Ok((r.get(0)?, r.get::<_, i64>(1)? as usize))
            })
            .map_err(error)?;
        let mut chosen = Vec::new();
        let mut remaining = Vec::new();
        for row in rows {
            let (id, file) = row.map_err(error)?;
            if ids.remove(&id) != all {
                chosen.push((id, file));
            } else {
                remaining.push(id);
            }
        }
        if !ids.is_empty() {
            return Err(CHANGED.into());
        }
        if chosen.is_empty() {
            return Err("请选择至少一张人脸。".into());
        }
        Ok(GroupSelection { chosen, remaining })
    })
}
