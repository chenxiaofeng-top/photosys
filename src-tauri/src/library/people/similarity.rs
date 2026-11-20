//! Search current cached faces from the exact avatar shown on a person card.
use super::{MODEL_GENERATION, Page, Store, current, error, faces, grouping, query, session};
use crate::ai::{ModelKind, catalog};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, sync::atomic::Ordering};

const CHANGED: &str = "头像、人脸或归属已变化，请重新筛选。";
const NO_AVATAR: &str = "此头像没有当前模型可用的人脸特征。请更新或重建人物索引后再试。";
const INDEXED: &str = "FROM visible_faces f JOIN media m ON m.path=f.path
    LEFT JOIN person_groups p ON p.id=f.person_id
    WHERE m.version=?1 AND m.error IS NULL AND f.ignored=0 AND f.review=0
      AND f.vector IS NOT NULL";
const CANDIDATES: &str = "AND (f.person_id IS NULL OR (p.id<>?2 AND p.ignored=0 AND p.archived=0))
    AND NOT EXISTS(SELECT 1 FROM faces
        WHERE path=f.path AND active=1 AND ignored=0 AND person_id=?2)
    AND NOT EXISTS(SELECT 1 FROM rejections WHERE face_id=f.id AND person_id=?2)";

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Criteria {
    pub(super) target_person_id: i64,
    pub(super) anchor_face_id: i64,
    pub(super) threshold: f64,
}

impl Criteria {
    fn validate(self) -> Result<(), String> {
        if (0.0..1.0).contains(&self.threshold) {
            Ok(())
        } else {
            Err("相似度门槛须大于或等于 0 并小于 1。".into())
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Results {
    pub(super) anchor_token: String,
    pub(super) faces: Page<SimilarFace>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SimilarFace {
    pub(super) face_id: i64,
    pub(super) file_id: usize,
    pub(super) score: f32,
    pub(super) source_person_id: Option<i64>,
    source_person_name: Option<String>,
    thumbnail: String,
    pub(super) token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Selection {
    pub(super) face_id: i64,
    pub(super) token: String,
}

struct IndexedFace {
    file_id: usize,
    person_id: Option<i64>,
    stamp: Vec<u8>,
    rotation: u16,
    encoded_vector: Vec<u8>,
}

impl IndexedFace {
    const COLUMNS: &str = "f.file_id,f.person_id,m.stamp,m.rotation,f.vector";

    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            file_id: row.get::<_, i64>(0)? as usize,
            person_id: row.get(1)?,
            stamp: row.get(2)?,
            rotation: row.get(3)?,
            encoded_vector: row.get(4)?,
        })
    }

    fn token(&self, face_id: i64, context: &[u8]) -> String {
        let mut digest = Sha256::new();
        digest.update(context);
        digest.update(face_id.to_le_bytes());
        digest.update(self.file_id.to_le_bytes());
        digest.update(self.person_id.unwrap_or(0).to_le_bytes());
        digest.update(&self.stamp);
        digest.update(self.rotation.to_le_bytes());
        digest.update(&self.encoded_vector);
        format!("{:x}", digest.finalize())
    }

    fn score(&self, anchor: &[f32]) -> Option<f32> {
        score(&self.encoded_vector, anchor)
    }
}

fn score(encoded: &[u8], anchor: &[f32]) -> Option<f32> {
    grouping::vector(encoded).map(|vector| faces::cosine(&vector, anchor).clamp(-1., 1.))
}

fn anchor(
    connection: &Connection,
    scan_id: u64,
    criteria: Criteria,
    version: &str,
    generation: u64,
) -> Result<(IndexedFace, Vec<f32>, String), String> {
    let face = connection
        .query_row(
            &format!(
                "SELECT {} {INDEXED} AND f.person_id=?2 AND f.id=?3 AND p.ignored=0 AND p.archived=0",
                IndexedFace::COLUMNS
            ),
            params![version, criteria.target_person_id, criteria.anchor_face_id],
            IndexedFace::read,
        )
        .optional()
        .map_err(error)?
        .ok_or(NO_AVATAR)?;
    let vector = grouping::vector(&face.encoded_vector).ok_or(NO_AVATAR)?;
    let mut context = Sha256::new();
    context.update(scan_id.to_le_bytes());
    context.update(version.as_bytes());
    context.update(generation.to_le_bytes());
    context.update(criteria.target_person_id.to_le_bytes());
    context.update(criteria.threshold.to_le_bytes());
    let token = face.token(criteria.anchor_face_id, &context.finalize());
    Ok((face, vector, token))
}

pub(super) fn search(scan_id: u64, criteria: Criteria, page: usize) -> Result<Results, String> {
    criteria.validate()?;
    let generation = MODEL_GENERATION.load(Ordering::Acquire);
    let version = catalog::active(ModelKind::People).feature_version;
    let result = query::with(scan_id, |store| {
        let store = store.ok_or(NO_AVATAR)?;
        let tx = store.connection.unchecked_transaction().map_err(error)?;
        let (_, anchor, anchor_token) = anchor(&tx, scan_id, criteria, version, generation)?;
        let mut ranked = Vec::new();
        {
            let mut statement = tx
                .prepare(&format!("SELECT f.id,f.vector {INDEXED} {CANDIDATES}"))
                .map_err(error)?;
            let mut rows = statement
                .query(params![version, criteria.target_person_id])
                .map_err(error)?;
            while let Some(row) = rows.next().map_err(error)? {
                let encoded = row.get_ref(1).map_err(error)?.as_blob().map_err(error)?;
                if let Some(score) = score(encoded, &anchor)
                    && f64::from(score) >= criteria.threshold
                {
                    ranked.push((row.get::<_, i64>(0).map_err(error)?, score));
                }
            }
        }
        ranked.sort_unstable_by(|(a, a_score), (b, b_score)| {
            b_score.total_cmp(a_score).then_with(|| a.cmp(b))
        });
        let total = ranked.len();
        let (page, offset) = Page::<SimilarFace>::bounds(page, total);
        let mut statement = tx
            .prepare(&format!(
                "SELECT {},p.name,f.thumbnail {INDEXED} {CANDIDATES} AND f.id=?3",
                IndexedFace::COLUMNS
            ))
            .map_err(error)?;
        let items = ranked
            .into_iter()
            .skip(offset)
            .take(super::super::PAGE_SIZE)
            .map(|(face_id, score)| {
                statement
                    .query_row(
                        params![version, criteria.target_person_id, face_id],
                        |row| {
                            let face = IndexedFace::read(row)?;
                            Ok(SimilarFace {
                                face_id,
                                file_id: face.file_id,
                                score,
                                source_person_id: face.person_id,
                                source_person_name: row.get(5)?,
                                thumbnail: query::image(row.get(6)?),
                                token: face.token(face_id, anchor_token.as_bytes()),
                            })
                        },
                    )
                    .map_err(error)
            })
            .collect::<Result<_, _>>()?;
        Ok(Results {
            anchor_token,
            faces: Page { page, total, items },
        })
    })?;
    if MODEL_GENERATION.load(Ordering::Acquire) != generation {
        return Err(CHANGED.into());
    }
    Ok(result)
}

// The caller holds the edit lock, which also serializes model changes, rotations, and index writes.
pub(super) fn confirm(
    scan_id: u64,
    criteria: Criteria,
    anchor_token: &str,
    selected: &[Selection],
) -> Result<(), String> {
    criteria.validate()?;
    let face_ids: Vec<_> = selected.iter().map(|choice| choice.face_id).collect();
    super::validate_suggestion_selection(&face_ids)?;
    let generation = MODEL_GENERATION.load(Ordering::Acquire);
    let version = catalog::active(ModelKind::People).feature_version;

    let scan = super::scan(scan_id)?;
    let mut store = Store::open(&scan, true)?.ok_or("无法打开人物数据库。")?;
    store.edit_checked(
        super::Edit::Assign {
            face_ids,
            person_id: Some(criteria.target_person_id),
            name: None,
        },
        |_| {
            let (anchor_file, candidate_files) = query::with(scan_id, |store| {
                let connection = &store.ok_or(CHANGED)?.connection;
                let (avatar, anchor, current_token) =
                    anchor(connection, scan_id, criteria, version, generation)
                        .map_err(|_| CHANGED)?;
                if current_token != anchor_token {
                    return Err(CHANGED.into());
                }
                let mut files = HashSet::new();
                let mut statement = connection
                    .prepare(&format!(
                        "SELECT {} {INDEXED} {CANDIDATES} AND f.id=?3",
                        IndexedFace::COLUMNS
                    ))
                    .map_err(error)?;
                for choice in selected {
                    let face = statement
                        .query_row(
                            params![version, criteria.target_person_id, choice.face_id],
                            IndexedFace::read,
                        )
                        .optional()
                        .map_err(error)?
                        .ok_or(CHANGED)?;
                    if face.token(choice.face_id, anchor_token.as_bytes()) != choice.token
                        || face
                            .score(&anchor)
                            .is_none_or(|score| f64::from(score) < criteria.threshold)
                    {
                        return Err(CHANGED.into());
                    }
                    if !files.insert(face.file_id) {
                        return Err("请为每张照片只勾选一张人脸。".into());
                    }
                }
                Ok((avatar.file_id, files))
            })?;

            let guard = session()?;
            let scan = &current(&guard, scan_id)?.scan;
            for file_id in std::iter::once(anchor_file).chain(candidate_files) {
                let item = scan.item(file_id)?;
                scan.root.open_matching(&item.file).map_err(|_| CHANGED)?;
            }
            // Keep the album session current all the way through the database commit.
            Ok(guard)
        },
    )
}
