use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Result, params};

use super::imaging::MAX_PIXELS;
use super::matching::{CaptureTime, Features, RGB_LEN};
use crate::duplicates::Candidate;

const VERSION: i64 = 1;

pub(super) struct Cache {
    connection: Connection,
    root: Vec<u8>,
}

impl Cache {
    pub fn open(database: &Path, root: &Path) -> std::result::Result<Self, String> {
        let parent = database.parent().ok_or("找不到索引目录。")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let connection = Connection::open(database).map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS photo_features (
                root BLOB NOT NULL,
                path BLOB NOT NULL,
                version INTEGER NOT NULL,
                stamp BLOB NOT NULL,
                width INTEGER NOT NULL,
                height INTEGER NOT NULL,
                hash INTEGER NOT NULL,
                rgb BLOB NOT NULL,
                captured_label TEXT,
                captured_millis INTEGER,
                captured_offset INTEGER,
                PRIMARY KEY (root, path)
            ) WITHOUT ROWID;",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            connection,
            root: key(root).to_vec(),
        })
    }

    pub fn get(&self, file: &Candidate) -> Result<Option<Features>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT width, height, hash, rgb, captured_label, captured_millis, captured_offset
             FROM photo_features WHERE root = ?1 AND path = ?2 AND version = ?3 AND stamp = ?4",
        )?;
        statement
            .query_row(
                params![
                    self.root,
                    key(&file.relative),
                    VERSION,
                    file.stamp.cache_key()
                ],
                |row| {
                    let width: u32 = row.get(0)?;
                    let height: u32 = row.get(1)?;
                    let rgb: Vec<u8> = row.get(3)?;
                    let rgb: Box<[u8; RGB_LEN]> = rgb
                        .into_boxed_slice()
                        .try_into()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?;
                    if width == 0
                        || height == 0
                        || u64::from(width) * u64::from(height) > MAX_PIXELS
                    {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    let captured = row
                        .get::<_, Option<String>>(4)?
                        .map(|label| {
                            Ok::<_, rusqlite::Error>(CaptureTime {
                                label,
                                millis: row.get(5)?,
                                has_offset: row.get(6)?,
                            })
                        })
                        .transpose()?;
                    Ok(Features {
                        width,
                        height,
                        hash: row.get::<_, i64>(2)? as u64,
                        rgb,
                        captured,
                    })
                },
            )
            .optional()
    }

    pub fn save<'a>(
        &mut self,
        entries: impl Iterator<Item = (&'a Candidate, &'a Features)>,
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT OR REPLACE INTO photo_features VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for (file, features) in entries {
                let captured = features.captured.as_ref();
                statement.execute(params![
                    self.root,
                    key(&file.relative),
                    VERSION,
                    file.stamp.cache_key(),
                    features.width,
                    features.height,
                    features.hash as i64,
                    &features.rgb[..],
                    captured.map(|time| &time.label),
                    captured.map(|time| time.millis),
                    captured.map(|time| time.has_offset),
                ])?;
            }
        }
        transaction.commit()
    }

    pub fn prune(&mut self, paths: &HashSet<&[u8]>) -> Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut select =
                transaction.prepare("SELECT path FROM photo_features WHERE root = ?1")?;
            let old = select
                .query_map([&self.root], |row| row.get::<_, Vec<u8>>(0))?
                .filter_map(|row| match row {
                    Ok(bytes) if paths.contains(bytes.as_slice()) => None,
                    row => Some(row),
                })
                .collect::<Result<Vec<_>>>()?;
            let mut delete =
                transaction.prepare("DELETE FROM photo_features WHERE root = ?1 AND path = ?2")?;
            for path in old {
                delete.execute(params![self.root, path])?;
            }
        }
        transaction.commit()
    }
}

pub(super) fn key(path: &Path) -> &[u8] {
    path.as_os_str().as_encoded_bytes()
}
