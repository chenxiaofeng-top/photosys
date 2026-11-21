//! Offline WGS84 location filtering, administrative lookup and per-library metadata cache.
use super::{Filter, Item, favorites};
use crate::{
    duplicates::{FileStamp, Root},
    media_date::{Coordinates, Metadata},
};
use chrono::NaiveDate;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Read, path::Path, sync::LazyLock, time::Duration};

static CACHE_VERSION: LazyLock<String> = LazyLock::new(|| {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../resources/locations/manifest.json"))
            .expect("bundled location manifest");
    format!(
        "1:{}",
        manifest["sha256"].as_str().expect("location data checksum")
    )
});

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Location {
    #[serde(flatten)]
    pub coordinates: Coordinates,
    pub address: Option<String>,
}
impl Location {
    pub(super) fn resolve(coordinates: Coordinates) -> Self {
        Self {
            coordinates,
            address: ADMIN.address(coordinates),
        }
    }
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LocationMode {
    #[default]
    All,
    Known,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub(crate) enum Selection {
    Point {
        latitude: f64,
        longitude: f64,
    },
    Bounds {
        west: f64,
        south: f64,
        east: f64,
        north: f64,
    },
}
impl Selection {
    pub(super) fn validate(&self) -> Result<(), String> {
        let valid = match *self {
            Self::Point {
                latitude,
                longitude,
            } => Coordinates::new(latitude, longitude).is_some(),
            Self::Bounds {
                west,
                south,
                east,
                north,
            } => {
                Coordinates::new(south, west).is_some()
                    && Coordinates::new(north, east).is_some()
                    && south <= north
            }
        };
        valid.then_some(()).ok_or_else(|| "位置范围无效。".into())
    }
    pub(super) fn contains(&self, c: Coordinates) -> bool {
        match *self {
            Self::Point {
                latitude,
                longitude,
            } => {
                // Tolerate decimal/JSON round-trip noise, far below GPS precision.
                (c.latitude - latitude).abs() < 1e-9 && (c.longitude - longitude).abs() < 1e-9
            }
            Self::Bounds {
                west,
                south,
                east,
                north,
            } => {
                (south..=north).contains(&c.latitude)
                    && if west <= east {
                        (west..=east).contains(&c.longitude)
                    } else {
                        c.longitude >= west || c.longitude <= east
                    }
            }
        }
    }
}
impl Filter {
    pub(super) fn validate_location(&self) -> Result<(), String> {
        if let Some(selection) = &self.location_selection {
            selection.validate()?;
        }
        Ok(())
    }
    pub(super) fn matches_location(&self, item: &Item) -> bool {
        match self.location_mode {
            LocationMode::Known if item.location.is_none() => return false,
            LocationMode::Unknown if item.location.is_some() => return false,
            _ => {}
        }
        self.location_selection.as_ref().is_none_or(|selection| {
            item.location
                .as_ref()
                .is_some_and(|location| selection.contains(location.coordinates))
        })
    }
}

#[derive(Deserialize, Serialize, Clone)]
pub(crate) struct Region {
    pub id: String,
    pub name: String,
    pub level: u8,
    pub bounds: [f64; 4],
    pub polygons: Vec<Vec<Vec<[f64; 2]>>>,
    #[serde(default)]
    pub ancestors: Vec<String>,
}
struct AdminIndex {
    regions: Vec<Region>,
    cells: HashMap<(i16, i16), Vec<usize>>,
}
static ADMIN: LazyLock<AdminIndex> = LazyLock::new(|| {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!("../../resources/locations/china.json.gz")[..])
        .read_to_end(&mut bytes)
        .expect("bundled administrative archive");
    let regions: Vec<Region> =
        serde_json::from_slice(&bytes).expect("validated bundled administrative dataset");
    AdminIndex::new(regions)
});
impl AdminIndex {
    fn new(regions: Vec<Region>) -> Self {
        let mut cells: HashMap<_, Vec<_>> = HashMap::new();
        for (id, r) in regions.iter().enumerate() {
            for x in (r.bounds[0].floor() as i16)..=(r.bounds[2].floor() as i16) {
                for y in (r.bounds[1].floor() as i16)..=(r.bounds[3].floor() as i16) {
                    cells.entry((x, y)).or_default().push(id);
                }
            }
        }
        Self { regions, cells }
    }
    fn address(&self, c: Coordinates) -> Option<String> {
        let ids = self
            .cells
            .get(&(c.longitude.floor() as i16, c.latitude.floor() as i16))?;
        let mut levels = std::collections::BTreeMap::new();
        for &id in ids {
            let r = &self.regions[id];
            if c.longitude < r.bounds[0]
                || c.longitude > r.bounds[2]
                || c.latitude < r.bounds[1]
                || c.latitude > r.bounds[3]
            {
                continue;
            }
            if r.polygons
                .iter()
                .any(|polygon| polygon_contains(polygon, c.longitude, c.latitude))
            {
                // Prefer the smallest enclosing region at each administrative level.
                let area = (r.bounds[2] - r.bounds[0]) * (r.bounds[3] - r.bounds[1]);
                if levels
                    .get(&r.level)
                    .is_none_or(|&(old, _): &(f64, &Region)| area < old)
                {
                    levels.insert(r.level, (area, r));
                }
            }
        }
        if levels.is_empty() {
            return None;
        }
        let mut names = vec!["中国".to_owned()];
        if let Some((_, r)) = levels
            .values()
            .next_back()
            .filter(|(_, r)| !r.ancestors.is_empty())
        {
            for name in r.ancestors.iter().chain(std::iter::once(&r.name)) {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
            return Some(names.join(" · "));
        }
        for (_, r) in levels.values() {
            if !names.contains(&r.name) {
                names.push(r.name.clone());
            }
        }
        Some(names.join(" · "))
    }
}
fn ring_contains(ring: &[[f64; 2]], x: f64, y: f64) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut previous = ring[ring.len() - 1];
    for &current in ring {
        let [ax, ay] = previous;
        let [bx, by] = current;
        let cross = (x - ax) * (by - ay) - (y - ay) * (bx - ax);
        if cross.abs() < 1e-10
            && x >= ax.min(bx)
            && x <= ax.max(bx)
            && y >= ay.min(by)
            && y <= ay.max(by)
        {
            return true;
        }
        if (ay > y) != (by > y) && x < (bx - ax) * (y - ay) / (by - ay) + ax {
            inside = !inside;
        }
        previous = current;
    }
    inside
}
fn polygon_contains(polygon: &[Vec<[f64; 2]>], x: f64, y: f64) -> bool {
    polygon
        .first()
        .is_some_and(|ring| ring_contains(ring, x, y))
        && !polygon.iter().skip(1).any(|ring| ring_contains(ring, x, y))
}

#[derive(Serialize)]
pub(crate) struct RegionView {
    id: String,
    name: String,
    level: u8,
    bounds: [f64; 4],
}
#[tauri::command]
pub(crate) async fn location_regions() -> Result<Vec<RegionView>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        ADMIN
            .regions
            .iter()
            .map(|r| RegionView {
                id: r.id.clone(),
                name: if r.ancestors.is_empty() {
                    r.name.clone()
                } else {
                    r.ancestors
                        .iter()
                        .chain(std::iter::once(&r.name))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" · ")
                },
                level: r.level,
                bounds: r.bounds,
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

pub(super) struct Cache {
    connection: Connection,
    writable: bool,
}
impl Cache {
    pub(super) fn open(root: &Root) -> Option<Self> {
        let directory = favorites::data_directory(root).ok()??;
        let path = directory.join("library.sqlite");
        let m = std::fs::symlink_metadata(&path).ok()?;
        if !m.is_file() || crate::duplicates::is_link(&m) {
            return None;
        }
        let flags = OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let (connection, writable) =
            match Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_READ_WRITE) {
                Ok(c) => (c, true),
                Err(_) => (
                    Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .ok()?,
                    false,
                ),
            };
        connection.busy_timeout(Duration::from_millis(200)).ok()?;
        let writable = writable
            && connection
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS media_locations (
            path BLOB PRIMARY KEY, stamp BLOB NOT NULL, version TEXT NOT NULL,
            captured TEXT, location TEXT
        ) WITHOUT ROWID; BEGIN;",
                )
                .is_ok();
        Some(Self {
            connection,
            writable,
        })
    }
    pub(super) fn read(
        &self,
        relative: &Path,
        stamp: &FileStamp,
    ) -> Option<(Metadata, Option<Location>)> {
        let (date, json): (Option<String>, Option<String>) = self.connection.query_row(
            "SELECT captured,location FROM media_locations WHERE path=?1 AND stamp=?2 AND version=?3",
            params![favorites::path_key(relative),stamp.portable_key(),&*CACHE_VERSION],
            |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional().ok()??;
        let location: Option<Location> = json.map(|s| serde_json::from_str(&s)).transpose().ok()?;
        if location.as_ref().is_some_and(|l| {
            Coordinates::new(l.coordinates.latitude, l.coordinates.longitude).is_none()
        }) {
            return None;
        }
        Some((
            Metadata {
                date: date
                    .map(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d"))
                    .transpose()
                    .ok()?,
                coordinates: location.as_ref().map(|l| l.coordinates),
            },
            location,
        ))
    }
    pub(super) fn write(
        &mut self,
        relative: &Path,
        stamp: &FileStamp,
        metadata: &Metadata,
        location: &Option<Location>,
    ) {
        if !self.writable {
            return;
        }
        let json = location
            .as_ref()
            .and_then(|l| serde_json::to_string(l).ok());
        if self.connection.execute("INSERT INTO media_locations VALUES(?1,?2,?3,?4,?5)
            ON CONFLICT(path) DO UPDATE SET stamp=excluded.stamp,version=excluded.version,captured=excluded.captured,location=excluded.location",
            params![favorites::path_key(relative),stamp.portable_key(),&*CACHE_VERSION,metadata.date.map(|d|d.to_string()),json]).is_err() {
            self.writable = false;
        }
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        let _ = self.connection.execute_batch("COMMIT;");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_include_edges_and_cross_the_antimeridian() {
        let s = Selection::Bounds {
            west: 170.,
            south: -10.,
            east: -170.,
            north: 10.,
        };
        assert!(s.validate().is_ok());
        assert!(s.contains(Coordinates::new(0., 180.).unwrap()));
        assert!(s.contains(Coordinates::new(-10., -170.).unwrap()));
        assert!(!s.contains(Coordinates::new(0., 0.).unwrap()));
        assert!(
            Selection::Point {
                latitude: f64::NAN,
                longitude: 0.
            }
            .validate()
            .is_err()
        );
        assert!(Coordinates::new(0., 0.).is_some());
        assert!(
            Selection::Point {
                latitude: 22.53,
                longitude: 114.06
            }
            .contains(Coordinates::new(22.53 + 1e-12, 114.06 - 1e-12).unwrap())
        );
    }
    #[test]
    fn polygons_respect_holes_and_boundaries() {
        let polygon = vec![
            vec![[0., 0.], [4., 0.], [4., 4.], [0., 4.], [0., 0.]],
            vec![[1., 1.], [2., 1.], [2., 2.], [1., 2.], [1., 1.]],
        ];
        assert!(polygon_contains(&polygon, 0., 0.));
        assert!(polygon_contains(&polygon, 3., 3.));
        assert!(!polygon_contains(&polygon, 1.5, 1.5));
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MapPoints {
    total: usize,
    unknown: usize,
    points: Vec<MapPoint>,
}
#[derive(Serialize)]
pub(crate) struct MapPoint {
    id: usize,
    latitude: f64,
    longitude: f64,
}
#[tauri::command]
pub(crate) async fn library_map_points(
    app: tauri::AppHandle,
    scan_id: u64,
    filter: Filter,
    person_id: Option<i64>,
    content_query: Option<String>,
) -> Result<MapPoints, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _database = crate::database_access::read()?;
        filter.validate_location()?;
        let views = if let Some(query) = content_query {
            if query.trim().is_empty() {
                Vec::new()
            } else {
                super::ai_search::map_items(&app, scan_id, &query, &filter)?
            }
        } else if let Some(person) = person_id {
            let ids = super::people::map_photo_ids(scan_id, person, &filter)?;
            let scan = super::scan(scan_id)?;
            ids.into_iter()
                .map(|id| scan.item(id).map(|i| i.view(false, 0)))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let lock = super::session()?;
            let session = super::current(&lock, scan_id)?;
            session
                .matching_items(&filter)?
                .into_iter()
                .map(|i| i.view(false, 0))
                .collect()
        };
        let total = views.len();
        let points: Vec<_> = views
            .into_iter()
            .filter_map(|i| {
                i.location.map(|l| MapPoint {
                    id: i.id,
                    latitude: l.coordinates.latitude,
                    longitude: l.coordinates.longitude,
                })
            })
            .collect();
        super::scan(scan_id)?;
        Ok(MapPoints {
            total,
            unknown: total - points.len(),
            points,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod integration_tests {
    use super::super::{
        Sort,
        tests::{all, session_for},
    };
    use super::*;
    use std::fs;
    #[test]
    fn real_chinese_addresses_use_county_parents_instead_of_coarse_overlapping_provinces() {
        for (lat, lon, parts) in [
            (39.98, 116.31, vec!["北京市", "海淀区"]),
            (31.2, 121.6, vec!["上海市", "浦东新区"]),
            (22.53, 114.06, vec!["广东省", "深圳市", "福田区"]),
            (18.25, 109.5, vec!["海南省", "三亚市", "天涯区"]),
        ] {
            let address = Location::resolve(Coordinates::new(lat, lon).unwrap())
                .address
                .unwrap();
            for part in parts {
                assert!(address.contains(part), "{lat},{lon}: {address}");
            }
        }
        assert!(
            Location::resolve(Coordinates::new(48.85, 2.35).unwrap())
                .address
                .is_none()
        );
        assert!(ADMIN.regions.len() > 3000);
    }
    #[test]
    fn location_cache_reopens_moves_and_invalidates_stamps_and_versions() {
        let temp = tempfile::tempdir().unwrap();
        let album = temp.path().join("album");
        fs::create_dir(&album).unwrap();
        let path = album.join("photo.jpg");
        fs::write(&path, b"original").unwrap();
        let root = Root::new(&album).unwrap();
        favorites::initialize(&root).unwrap();
        let stamp = FileStamp::read(&root.open_file(Path::new("photo.jpg")).unwrap()).unwrap();
        let c = Coordinates::new(22.53, 114.06).unwrap();
        let location = Some(Location::resolve(c));
        let metadata = Metadata {
            date: NaiveDate::from_ymd_opt(2024, 2, 29),
            coordinates: Some(c),
        };
        {
            let mut cache = Cache::open(&root).unwrap();
            cache.write(Path::new("photo.jpg"), &stamp, &metadata, &location);
        }
        let moved = temp.path().join("moved");
        drop(root);
        fs::rename(&album, &moved).unwrap();
        let root = Root::new(&moved).unwrap();
        {
            let cache = Cache::open(&root).unwrap();
            let (read, stored) = cache.read(Path::new("photo.jpg"), &stamp).unwrap();
            assert_eq!(read.date, metadata.date);
            assert_eq!(stored, location);
        }
        {
            let cache = Cache::open(&root).unwrap();
            cache
                .connection
                .execute("UPDATE media_locations SET version='old'", [])
                .unwrap();
            assert!(cache.read(Path::new("photo.jpg"), &stamp).is_none());
        }
        {
            let mut cache = Cache::open(&root).unwrap();
            cache.write(Path::new("photo.jpg"), &stamp, &metadata, &location);
        }
        fs::write(moved.join("photo.jpg"), b"changed media").unwrap();
        let changed = FileStamp::read(&root.open_file(Path::new("photo.jpg")).unwrap()).unwrap();
        assert!(
            Cache::open(&root)
                .unwrap()
                .read(Path::new("photo.jpg"), &changed)
                .is_none()
        );
        assert_eq!(fs::read(moved.join("photo.jpg")).unwrap(), b"changed media");
    }
    #[test]
    fn geography_combines_with_media_filters_and_pagination() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..60 {
            fs::write(temp.path().join(format!("20240101-{n:03}.jpg")), []).unwrap();
        }
        fs::write(temp.path().join("20240101-video.mp4"), []).unwrap();
        let mut session = session_for(temp.path());
        let scan = std::sync::Arc::get_mut(&mut session.scan).unwrap();
        for (index, item) in scan.items.iter_mut().enumerate().take(50) {
            item.location = Some(Location {
                coordinates: Coordinates::new(20., 100. + index as f64 / 100.).unwrap(),
                address: None,
            });
        }
        let mut filter = all();
        filter.location_mode = LocationMode::Known;
        let page = session.items(filter.clone(), 1, Sort::DateAsc).unwrap();
        assert_eq!(page.total, 50);
        assert_eq!(page.items.len(), 24);
        filter.location_selection = Some(Selection::Bounds {
            west: 100.,
            south: 19.,
            east: 100.24,
            north: 21.,
        });
        assert_eq!(
            session
                .items(filter.clone(), 0, Sort::DateAsc)
                .unwrap()
                .total,
            25
        );
        filter.favorites_only = true;
        session.favorites.insert(10);
        assert_eq!(
            session
                .items(filter.clone(), 0, Sort::DateAsc)
                .unwrap()
                .items[0]
                .id,
            10
        );
        filter.location_selection = Some(Selection::Point {
            latitude: 20.,
            longitude: 100.10,
        });
        assert_eq!(
            session
                .items(filter.clone(), 0, Sort::DateAsc)
                .unwrap()
                .total,
            1
        );
        filter.location_mode = LocationMode::Unknown;
        filter.location_selection = None;
        filter.favorites_only = false;
        assert_eq!(session.items(filter, 0, Sort::DateAsc).unwrap().total, 11);
    }
}
