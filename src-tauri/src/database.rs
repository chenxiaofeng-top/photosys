use std::error::Error;
use std::io;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

pub struct Database(Connection);

impl Database {
    pub fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let mut connection = Connection::open(path)?;
        let migration = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: u32 = migration.query_row("PRAGMA user_version", [], |row| row.get(0))?;

        match version {
            0 => migration.execute_batch(
                "CREATE TABLE note (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    body TEXT NOT NULL
                );
                PRAGMA user_version = 1;",
            )?,
            1 => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Unsupported PhotoSys database version: {version}"),
                )
                .into());
            }
        }

        migration.commit()?;
        Ok(Self(connection))
    }

    pub fn load_note(&self) -> rusqlite::Result<String> {
        self.0
            .query_row("SELECT body FROM note WHERE id = 1", [], |row| row.get(0))
            .optional()
            .map(Option::unwrap_or_default)
    }

    pub fn save_note(&self, text: &str) -> rusqlite::Result<()> {
        self.0.execute(
            "INSERT INTO note (id, body) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET body = excluded.body",
            [text],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_database_and_applies_first_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photosys.sqlite3");
        let database = Database::open(&path).unwrap();

        assert!(path.is_file());
        assert_eq!(database.load_note().unwrap(), "");
        let version: u32 = database
            .0
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn reopening_preserves_saved_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photosys.sqlite3");
        let text = "PhotoSys 测试\n'第二行'";
        Database::open(&path).unwrap().save_note(text).unwrap();

        let reopened = Database::open(&path).unwrap();
        assert_eq!(reopened.load_note().unwrap(), text);
    }

    #[test]
    fn empty_text_overwrites_existing_note() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photosys.sqlite3");
        let database = Database::open(&path).unwrap();
        database.save_note("draft").unwrap();
        database.save_note("").unwrap();
        drop(database);

        assert_eq!(Database::open(&path).unwrap().load_note().unwrap(), "");
    }
}
