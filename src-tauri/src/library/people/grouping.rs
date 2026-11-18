use super::{query, store::Store};
use crate::ai::{
    error,
    faces::{self},
};
use rusqlite::params;
use serde::Deserialize;
use std::collections::HashMap;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};
#[derive(Clone, Deserialize)]
pub(super) struct Policy {
    pub enabled: bool,
    pub version: String,
    pub threshold: f32,
    pub margin: f32,
    pub validation: Option<Validation>,
    pub reason: String,
}
#[derive(Clone, Deserialize)]
pub(super) struct Validation {
    pub pipeline: String,
    pub correct_links: usize,
    pub accepted_links: usize,
    pub coverage: f32,
    pub strata: HashMap<String, usize>,
    pub strata_correct: HashMap<String, usize>,
    pub dataset_sha256: String,
}
impl Policy {
    pub fn shipped() -> Self {
        serde_json::from_str(include_str!(
            "../../../../scripts/people-grouping-policy.json"
        ))
        .expect("bundled grouping policy")
    }
    pub fn allowed(&self) -> bool {
        self.enabled
            && self.version
                == crate::ai::catalog::active(crate::ai::ModelKind::People).feature_version
            && self.threshold.is_finite()
            && (0.0..1.0).contains(&self.threshold)
            && self.margin.is_finite()
            && (0.0..1.0).contains(&self.margin)
            && self.validation.as_ref().is_some_and(|v| {
                v.pipeline == "fixed-anchors-hnsw-exact-v1"
                    && v.accepted_links >= 1000
                    && v.correct_links <= v.accepted_links
                    && v.correct_links as f64 / v.accepted_links as f64 >= 0.99
                    && v.coverage.is_finite()
                    && v.coverage > 0.
                    && v.coverage <= 1.
                    && v.dataset_sha256.len() == 64
                    && v.dataset_sha256.bytes().all(|c| c.is_ascii_hexdigit())
                    && ["adult", "child", "profile", "occluded", "years"]
                        .iter()
                        .all(|k| {
                            v.strata.get(*k).is_some_and(|n| {
                                *n >= 20
                                    && v.strata_correct
                                        .get(*k)
                                        .is_some_and(|c| *c <= *n && *c as f64 / *n as f64 >= 0.99)
                            })
                        })
            })
    }
}
struct Anchor {
    person: i64,
    vector: Vec<f32>,
}
pub(super) struct Groups {
    index: Index,
    anchors: HashMap<u64, Anchor>,
    policy: Policy,
    enabled: bool,
}
fn vector(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() != crate::ai::catalog::active(crate::ai::ModelKind::People).dimension * 4 {
        return None;
    }
    faces::normalize_dim(
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect(),
        crate::ai::catalog::active(crate::ai::ModelKind::People).dimension,
    )
    .ok()
}
impl Groups {
    pub fn load(scan_id: u64, policy: Policy) -> Result<Self, String> {
        let enabled = policy.allowed();
        Self::create(scan_id, policy, enabled)
    }
    #[cfg(test)]
    pub fn for_evaluation(scan_id: u64, policy: Policy) -> Result<Self, String> {
        Self::create(scan_id, policy, true)
    }
    fn create(scan_id: u64, policy: Policy, enabled: bool) -> Result<Self, String> {
        let index = Index::new(&IndexOptions {
            dimensions: crate::ai::catalog::active(crate::ai::ModelKind::People).dimension,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            connectivity: 16,
            expansion_add: 128,
            expansion_search: 128,
            ..Default::default()
        })
        .map_err(error)?;
        let mut g = Self {
            index,
            anchors: HashMap::new(),
            policy,
            enabled,
        };
        if !g.enabled {
            return Ok(g);
        }
        let rows = query::with(scan_id, |s| {
            let Some(s) = s else {
                return Ok(Vec::new());
            };
            let mut q=s.connection.prepare("WITH candidates AS(SELECT v.id,v.person_id,v.vector,v.manual,ROW_NUMBER() OVER(PARTITION BY v.person_id ORDER BY v.manual DESC,v.id) rank FROM visible_faces v JOIN people p ON p.id=v.person_id JOIN media m ON m.path=v.path WHERE m.version=?1 AND m.error IS NULL AND v.ignored=0 AND v.vector IS NOT NULL AND p.archived=0) SELECT id,person_id,vector FROM candidates WHERE rank=1 OR (manual=1 AND rank<=5) ORDER BY id").map_err(error)?;
            q.query_map(
                [crate::ai::catalog::active(crate::ai::ModelKind::People).feature_version],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)
        })?;
        g.index.reserve(rows.len().max(64)).map_err(error)?;
        for (id, person, bytes) in rows {
            if let Some(v) = vector(&bytes) {
                g.insert(id, person, v)?;
            }
        }
        Ok(g)
    }
    fn insert(&mut self, id: i64, person: i64, vector: Vec<f32>) -> Result<(), String> {
        if self.index.size() == self.index.capacity() {
            self.index
                .reserve((self.index.size() * 2).max(64))
                .map_err(error)?;
        }
        self.index.add(id as u64, &vector).map_err(error)?;
        self.anchors.insert(id as u64, Anchor { person, vector });
        Ok(())
    }
    pub fn assign(&mut self, s: &mut Store, ids: &[i64]) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let tx = s.connection.transaction().map_err(error)?;
        // A manual edit can occur while inference is running. Recheck all mutable state here.
        for id in ids {
            let row:Option<(Vec<u8>,Vec<u8>)>=tx.query_row("SELECT f.path,f.vector FROM faces f JOIN media m ON m.path=f.path WHERE f.id=?1 AND f.active=1 AND f.ignored=0 AND f.manual=0 AND f.person_id IS NULL AND f.vector IS NOT NULL AND m.version=?2 AND m.error IS NULL",params![id,crate::ai::catalog::active(crate::ai::ModelKind::People).feature_version],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(error)?;
            let Some((path, bytes)) = row else {
                continue;
            };
            let Some(v) = vector(&bytes) else {
                continue;
            };
            let mut scores: HashMap<i64, f32> = HashMap::new();
            if self.index.size() > 0 {
                let hits = self.index.search(&v, 32).map_err(error)?;
                for key in hits.keys {
                    if let Some(a) = self.anchors.get(&key) {
                        let score = faces::cosine(&v, &a.vector);
                        scores
                            .entry(a.person)
                            .and_modify(|s| *s = s.max(score))
                            .or_insert(score);
                    }
                }
            }
            let mut ranked: Vec<_> = scores.into_iter().collect();
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            let target = if let Some(&(p, score)) = ranked.first() {
                if score >= self.policy.threshold {
                    let ambiguous = ranked
                        .get(1)
                        .is_some_and(|(_, other)| score - other < self.policy.margin);
                    let blocked:bool=tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM people WHERE id=?1 AND archived=0) OR EXISTS(SELECT 1 FROM faces WHERE path=?2 AND active=1 AND ignored=0 AND person_id=?1) OR EXISTS(SELECT 1 FROM rejections WHERE face_id=?3 AND person_id=?1)",params![p,path,id],|r|r.get(0)).map_err(error)?;
                    if ambiguous || blocked {
                        continue;
                    }
                    Some(p)
                } else {
                    None
                }
            } else {
                None
            };
            let person = if let Some(p) = target {
                p
            } else {
                tx.execute("INSERT INTO people DEFAULT VALUES", [])
                    .map_err(error)?;
                let p = tx.last_insert_rowid();
                self.insert(*id, p, v.clone())?;
                p
            };
            tx.execute(
                "UPDATE faces SET person_id=?2 WHERE id=?1 AND manual=0 AND person_id IS NULL",
                params![id, person],
            )
            .map_err(error)?;
        }
        tx.commit().map_err(error)?;
        Ok(())
    }
    pub fn pending_ids(scan_id: u64) -> Result<Vec<i64>, String> {
        query::with(scan_id, |s| {
            let Some(s) = s else {
                return Ok(Vec::new());
            };
            let mut q=s.connection.prepare("SELECT v.id FROM visible_faces v JOIN media m ON m.path=v.path WHERE v.person_id IS NULL AND v.manual=0 AND v.ignored=0 AND v.vector IS NOT NULL AND m.version=?1 AND m.error IS NULL ORDER BY v.id").map_err(error)?;
            q.query_map(
                [crate::ai::catalog::active(crate::ai::ModelKind::People).feature_version],
                |r| r.get(0),
            )
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)
        })
    }
}
use rusqlite::OptionalExtension;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unvalidated_policy_never_groups() {
        let mut p = Policy::shipped();
        p.enabled = true;
        assert!(!p.allowed());
        assert!(!Policy::shipped().allowed());
    }
}

#[cfg(test)]
mod release_gate_tests {
    use super::*;
    #[test]
    fn release_gate_requires_accuracy_each_stratum_and_matching_pipeline() {
        let mut policy = Policy::shipped();
        policy.enabled = true;
        let strata = ["adult", "child", "profile", "occluded", "years"]
            .into_iter()
            .map(|k| (k.to_owned(), 100))
            .collect::<HashMap<_, _>>();
        policy.validation = Some(Validation {
            pipeline: "fixed-anchors-hnsw-exact-v1".into(),
            correct_links: 995,
            accepted_links: 1000,
            coverage: 0.5,
            strata: strata.clone(),
            strata_correct: strata,
            dataset_sha256: "a".repeat(64),
        });
        assert!(policy.allowed());
        policy.validation.as_mut().unwrap().correct_links = 989;
        assert!(!policy.allowed());
        policy.validation.as_mut().unwrap().correct_links = 995;
        policy
            .validation
            .as_mut()
            .unwrap()
            .strata_correct
            .insert("child".into(), 98);
        assert!(!policy.allowed());
        policy
            .validation
            .as_mut()
            .unwrap()
            .strata_correct
            .insert("child".into(), 100);
        policy.version = "old".into();
        assert!(!policy.allowed());
    }
}
