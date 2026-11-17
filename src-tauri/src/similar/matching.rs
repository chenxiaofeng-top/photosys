use std::collections::BTreeMap;
use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};

pub(super) const RGB_LEN: usize = 16 * 16 * 3;
const HASH_PARTS: usize = 4;
const PART_VALUES: usize = 1 << 16;

pub(super) struct Features {
    pub width: u32,
    pub height: u32,
    pub hash: u64,
    pub rgb: Box<[u8; RGB_LEN]>,
    pub captured: Option<CaptureTime>,
}

#[derive(Clone)]
pub(super) struct CaptureTime {
    pub label: String,
    pub millis: i64,
    pub has_offset: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum MatchKind {
    Variant,
    Burst,
}

pub(super) struct Group {
    pub anchor: usize,
    pub members: Vec<(usize, MatchKind)>,
    pub has_variant: bool,
    pub has_burst: bool,
}

impl Group {
    pub fn matches(&self, kind: Option<MatchKind>) -> bool {
        match kind {
            None => true,
            Some(MatchKind::Variant) => self.has_variant,
            Some(MatchKind::Burst) => self.has_burst,
        }
    }
}

fn distance(left: &Features, right: &Features, kind: MatchKind) -> Option<(u32, u32)> {
    let (max_hash, max_ratio, max_rgb) = match kind {
        MatchKind::Variant => (8, 5, 12),
        MatchKind::Burst => (16, 10, 18),
    };
    let hash = (left.hash ^ right.hash).count_ones();
    if hash > max_hash {
        return None;
    }

    let lhs = u64::from(left.width) * u64::from(right.height);
    let rhs = u64::from(right.width) * u64::from(left.height);
    if 100 * lhs.abs_diff(rhs) > max_ratio * lhs.min(rhs) {
        return None;
    }

    let limit = max_rgb * (RGB_LEN as u32 * 255) / 100;
    let mut sum = 0;
    for (&a, &b) in left.rgb.iter().zip(right.rgb.iter()) {
        sum += u32::from(a.abs_diff(b));
        if sum > limit {
            return None;
        }
    }
    Some((hash, sum))
}

fn masks() -> Vec<usize> {
    let mut masks = vec![0];
    for first in 0..16 {
        masks.push(1 << first);
        for second in 0..first {
            masks.push((1 << first) | (1 << second));
        }
    }
    masks
}

fn hash_part(hash: u64, part: usize) -> usize {
    ((hash >> (16 * part)) as u16) as usize
}

type Best = Option<(u32, u32, usize)>;

fn consider(
    best: &mut Best,
    current: &Features,
    features: &[&Features],
    anchor: usize,
    kind: MatchKind,
) {
    if let Some((hash, rgb)) = distance(current, features[anchor], kind)
        && best.is_none_or(|(h, c, id)| (hash, rgb, anchor) < (h, c, id))
    {
        *best = Some((hash, rgb, anchor));
    }
}

pub(super) fn group(
    features: &[&Features],
    mut tick: impl FnMut(usize) -> ControlFlow<()>,
) -> ControlFlow<(), Vec<Group>> {
    // The caller supplies features in path order. This index is also the path rank.
    let mut order: Vec<_> = (0..features.len()).collect();
    order.sort_unstable_by_key(|&id| {
        let item = features[id];
        (
            std::cmp::Reverse(u64::from(item.width) * u64::from(item.height)),
            id,
        )
    });
    let masks = masks();
    let mut buckets = vec![Vec::<usize>::new(); HASH_PARTS * PART_VALUES];
    let mut times = BTreeMap::<(bool, i64), Vec<usize>>::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut anchor_groups = vec![0; features.len()];
    let mut visited = vec![0; features.len()];
    for (processed, id) in order.into_iter().enumerate() {
        tick(processed)?;
        let current = features[id];
        let mut best = None;

        // At distance <= 8, one of the four 16-bit parts differs by at most two bits.
        for part in 0..HASH_PARTS {
            let value = hash_part(current.hash, part);
            for mask in &masks {
                for &anchor in &buckets[part * PART_VALUES + (value ^ mask)] {
                    if visited[anchor] != processed + 1 {
                        visited[anchor] = processed + 1;
                        consider(&mut best, current, features, anchor, MatchKind::Variant);
                    }
                }
            }
        }

        let mut kind = MatchKind::Variant;
        if best.is_none()
            && let Some(time) = &current.captured
        {
            kind = MatchKind::Burst;
            for (_, anchors) in times.range(
                (time.has_offset, time.millis.saturating_sub(5_000))
                    ..=(time.has_offset, time.millis.saturating_add(5_000)),
            ) {
                for &anchor in anchors {
                    consider(&mut best, current, features, anchor, kind);
                }
            }
        }

        if let Some((_, _, anchor)) = best {
            let group = &mut groups[anchor_groups[anchor]];
            group.members.push((id, kind));
            group.has_variant |= kind == MatchKind::Variant;
            group.has_burst |= kind == MatchKind::Burst;
        } else {
            anchor_groups[id] = groups.len();
            groups.push(Group {
                anchor: id,
                members: Vec::new(),
                has_variant: false,
                has_burst: false,
            });
            for part in 0..HASH_PARTS {
                buckets[part * PART_VALUES + hash_part(current.hash, part)].push(id);
            }
            if let Some(time) = &current.captured {
                times
                    .entry((time.has_offset, time.millis))
                    .or_default()
                    .push(id);
            }
        }
    }

    tick(features.len())?;
    groups.retain(|group| !group.members.is_empty());
    ControlFlow::Continue(groups)
}
