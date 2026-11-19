use super::*;
use rusqlite::params;
use std::io::Write;

const BACKUP: &str = "restore-backup-12345678-1234-1234-1234-123456789abc";
fn album(path: &Path, all: bool) -> Root {
    fs::create_dir_all(path.join(".photosys")).unwrap();
    let c = Connection::open(path.join(".photosys/library.sqlite")).unwrap();
    c.execute_batch("CREATE TABLE favorites(path BLOB PRIMARY KEY);CREATE TABLE rotations(path BLOB PRIMARY KEY,rotation INTEGER);CREATE TABLE media_locations(path BLOB PRIMARY KEY,stamp BLOB,version TEXT,captured TEXT,location TEXT);
        INSERT INTO favorites VALUES(x'612e6a7067');INSERT INTO rotations VALUES(x'612e6a7067',90);
        INSERT INTO media_locations VALUES(x'612e6a7067',x'01','v1','2020-01-01','{}');").unwrap();
    fs::write(path.join("照片.jpg"), b"original-photo-bytes").unwrap();
    if all {
        let c = Connection::open(path.join(".photosys/ai.sqlite")).unwrap();
        c.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value INTEGER);CREATE TABLE embeddings(path BLOB PRIMARY KEY,stamp BLOB,rotation INTEGER,version TEXT,vector BLOB,error TEXT);
            INSERT INTO settings VALUES('enabled',1);INSERT INTO embeddings VALUES(x'612e6a7067',x'01',90,'test',x'01020304',NULL);").unwrap();
        let c = Connection::open(path.join(".photosys/people.sqlite")).unwrap();
        c.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT);CREATE TABLE people(id INTEGER PRIMARY KEY,name TEXT,archived INTEGER);
            CREATE TABLE media(path BLOB PRIMARY KEY,stamp BLOB,rotation INTEGER,version TEXT,error TEXT);
            CREATE TABLE faces(id INTEGER PRIMARY KEY,path BLOB,bbox TEXT,confidence REAL,vector BLOB,thumbnail BLOB,person_id INTEGER,manual INTEGER,ignored INTEGER,active INTEGER,review INTEGER);
            CREATE TABLE rejections(face_id INTEGER,person_id INTEGER);CREATE TABLE edits(id INTEGER PRIMARY KEY,description TEXT,payload TEXT);
            INSERT INTO settings VALUES('enabled','1');INSERT INTO people VALUES(1,'妈妈',0);
            INSERT INTO media VALUES(x'612e6a7067',x'01',90,'test',NULL);
            INSERT INTO faces VALUES(1,x'612e6a7067','[0,0,1,1]',0.99,x'01020304',x'0102',1,1,1,1,0);
            INSERT INTO rejections VALUES(1,2);INSERT INTO edits VALUES(1,'修改分组','{}');PRAGMA user_version=1;").unwrap();
    }
    Root::new(path).unwrap()
}
fn inspect_file(path: &Path) -> Inspection {
    let work = tempfile::tempdir().unwrap();
    inspect(path, work.path(), &|_| {}).unwrap()
}
fn raw_files(path: &Path) -> Vec<(String, Vec<u8>)> {
    database_files()
        .into_iter()
        .filter_map(|n| fs::read(path.join(&n)).ok().map(|b| (n, b)))
        .collect()
}
fn same_sql(path: &Path, sql: &str) -> String {
    Connection::open(path)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}
#[test]
fn all_databases_round_trip_and_media_and_unknown_files_are_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let source = album(&temp.path().join("原相册"), true);
    let zip = temp.path().join("数据库备份.zip");
    export(&source, &zip, |_| {}).unwrap();
    let info = inspect_file(&zip);
    assert_eq!(info.manifest.databases.len(), 3);
    let target = album(&temp.path().join("搬迁后的相册"), true);
    let data = target.path.join(".photosys");
    fs::write(data.join("other.txt"), b"untouched").unwrap();
    Connection::open(data.join("library.sqlite"))
        .unwrap()
        .execute("DELETE FROM favorites", [])
        .unwrap();
    let originals = raw_files(&data);
    let (backup, warning) =
        restore(&target, &zip, &info.fingerprint, BACKUP, |_| {}, |_| Ok(())).unwrap();
    assert!(warning.is_none());
    assert_eq!(raw_files(&backup), originals);
    assert_eq!(
        same_sql(
            &data.join("library.sqlite"),
            "SELECT hex(path) FROM favorites"
        ),
        "612E6A7067"
    );
    assert_eq!(
        same_sql(
            &data.join("library.sqlite"),
            "SELECT CAST(rotation AS TEXT) FROM rotations"
        ),
        "90"
    );
    assert_eq!(
        same_sql(
            &data.join("ai.sqlite"),
            "SELECT hex(vector) FROM embeddings"
        ),
        "01020304"
    );
    assert_eq!(
        same_sql(&data.join("people.sqlite"), "SELECT name FROM people"),
        "妈妈"
    );
    assert_eq!(
        same_sql(
            &data.join("people.sqlite"),
            "SELECT CAST(manual + ignored AS TEXT) FROM faces"
        ),
        "2"
    );
    assert_eq!(
        same_sql(&data.join("people.sqlite"), "SELECT description FROM edits"),
        "修改分组"
    );
    assert_eq!(
        fs::read(target.path.join("照片.jpg")).unwrap(),
        b"original-photo-bytes"
    );
    assert_eq!(fs::read(data.join("other.txt")).unwrap(), b"untouched");
    assert!(!data.join(JOURNAL).exists());
}
#[test]
fn missing_optional_databases_clear_old_indexes_and_corrupt_databases_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let source = album(&temp.path().join("source"), false);
    let zip = temp.path().join("backup.zip");
    export(&source, &zip, |_| {}).unwrap();
    let info = inspect_file(&zip);
    let target = album(&temp.path().join("target"), true);
    let data = target.path.join(".photosys");
    fs::write(data.join("library.sqlite"), b"corrupt").unwrap();
    fs::write(data.join("library.sqlite-wal"), b"preserved-wal").unwrap();
    let (backup, _) =
        restore(&target, &zip, &info.fingerprint, BACKUP, |_| {}, |_| Ok(())).unwrap();
    assert_eq!(fs::read(backup.join("library.sqlite")).unwrap(), b"corrupt");
    assert_eq!(
        fs::read(backup.join("library.sqlite-wal")).unwrap(),
        b"preserved-wal"
    );
    assert!(!data.join("ai.sqlite").exists());
    assert!(!data.join("people.sqlite").exists());
    assert!(!data.join("library.sqlite-wal").exists());
    validate_database(&data.join("library.sqlite"), "library.sqlite").unwrap();
    let empty = temp.path().join("empty");
    fs::create_dir(&empty).unwrap();
    restore(
        &Root::new(&empty).unwrap(),
        &zip,
        &info.fingerprint,
        BACKUP,
        |_| {},
        |_| Ok(()),
    )
    .unwrap();
    validate_database(&empty.join(".photosys/library.sqlite"), "library.sqlite").unwrap();
}
#[test]
fn sqlite_snapshot_contains_committed_wal_pages() {
    let temp = tempfile::tempdir().unwrap();
    let root = album(&temp.path().join("source"), false);
    let c = Connection::open(root.path.join(".photosys/library.sqlite")).unwrap();
    c.execute_batch("PRAGMA journal_mode=WAL;PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    c.execute(
        "INSERT INTO favorites VALUES(?1)",
        params![b"wal.jpg".as_slice()],
    )
    .unwrap();
    assert!(root.path.join(".photosys/library.sqlite-wal").exists());
    let zip = temp.path().join("backup.zip");
    export(&root, &zip, |_| {}).unwrap();
    let work = tempfile::tempdir().unwrap();
    inspect(&zip, work.path(), &|_| {}).unwrap();
    assert_eq!(
        same_sql(
            &work.path().join("library.sqlite"),
            "SELECT CAST(COUNT(*) AS TEXT) FROM favorites"
        ),
        "2"
    );
}
fn rewrite_zip(
    source: &Path,
    destination: &Path,
    change: impl Fn(&str, &mut Vec<u8>),
    extra: Option<&str>,
) {
    let mut input = ZipArchive::new(File::open(source).unwrap()).unwrap();
    let mut output = ZipWriter::new(File::create(destination).unwrap());
    for i in 0..input.len() {
        let mut entry = input.by_index(i).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        change(entry.name(), &mut bytes);
        output
            .start_file(entry.name(), SimpleFileOptions::default())
            .unwrap();
        output.write_all(&bytes).unwrap();
    }
    if let Some(name) = extra {
        output
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        output.write_all(b"unexpected").unwrap();
    }
    output.finish().unwrap();
}
#[test]
fn invalid_packages_and_changed_input_fail_before_overwriting() {
    let temp = tempfile::tempdir().unwrap();
    let root = album(&temp.path().join("source"), false);
    let zip = temp.path().join("good.zip");
    export(&root, &zip, |_| {}).unwrap();
    let good = inspect_file(&zip);
    let originals = raw_files(&root.path.join(".photosys"));
    let bad = temp.path().join("bad.zip");
    for mode in 0..5 {
        rewrite_zip(
            &zip,
            &bad,
            |name, bytes| {
                if mode == 0 && name == "library.sqlite" {
                    bytes[0] ^= 1;
                }
                if name == MANIFEST && (mode == 1 || mode == 2) {
                    let mut m: serde_json::Value = serde_json::from_slice(bytes).unwrap();
                    if mode == 1 {
                        m["formatVersion"] = 999.into();
                    } else {
                        m["databases"][0]["sha256"] = "0".repeat(64).into();
                    }
                    *bytes = serde_json::to_vec(&m).unwrap();
                }
            },
            if mode == 3 {
                Some("../outside")
            } else if mode == 4 {
                Some("extra")
            } else {
                None
            },
        );
        assert!(restore(&root, &bad, &good.fingerprint, BACKUP, |_| {}, |_| Ok(())).is_err());
        assert_eq!(raw_files(&root.path.join(".photosys")), originals);
        assert!(!root.path.join(".photosys").join(BACKUP).exists());
    }
    assert!(restore(&root, &zip, "wrong fingerprint", BACKUP, |_| {}, |_| Ok(())).is_err());
    fs::write(&bad, b"not a zip").unwrap();
    let work = tempfile::tempdir().unwrap();
    assert!(inspect(&bad, work.path(), &|_| {}).is_err());
}
#[test]
fn every_replacement_failure_rolls_back_all_original_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let source = album(&temp.path().join("source"), true);
    let zip = temp.path().join("backup.zip");
    export(&source, &zip, |_| {}).unwrap();
    let info = inspect_file(&zip);
    for fail_at in 0..database_files().len() {
        let target = album(&temp.path().join(format!("target-{fail_at}")), false);
        let data = target.path.join(".photosys");
        fs::write(data.join("ai.sqlite-journal"), b"old journal").unwrap();
        let originals = raw_files(&data);
        let mut count = 0;
        let result = restore(
            &target,
            &zip,
            &info.fingerprint,
            BACKUP,
            |_| {},
            |_| {
                let fail = count == fail_at;
                count += 1;
                if fail {
                    Err("injected write failure".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.unwrap_err().contains("已还原"));
        assert_eq!(raw_files(&data), originals);
        assert!(!data.join(JOURNAL).exists());
    }
}
#[test]
fn interrupted_restore_and_interrupted_rollback_recover_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let root = album(&temp.path().join("target"), true);
    let data = root.path.join(".photosys");
    let backup = data.join(BACKUP);
    fs::create_dir(&backup).unwrap();
    let originals = raw_files(&data);
    let mut entries = Vec::new();
    for (name, _) in &originals {
        copy_synced(&data.join(name), &backup.join(name)).unwrap();
        entries.push(descriptor(&backup.join(name), name).unwrap());
    }
    let journal = Journal {
        format_version: 1,
        backup_directory: BACKUP.into(),
        staging_directory: None,
        original_files: entries,
        committed: false,
    };
    write_json(&data.join(JOURNAL), &journal).unwrap();
    fs::write(data.join("library.sqlite"), b"partial replacement").unwrap();
    fs::remove_file(data.join("people.sqlite")).unwrap();
    let mut n = 0;
    assert!(
        rollback(&data, &journal, |_| {
            n += 1;
            if n == 4 {
                Err("injected rollback failure".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert!(data.join(JOURNAL).exists());
    recover(&root).unwrap();
    recover(&root).unwrap();
    assert_eq!(raw_files(&data), originals);
    assert_eq!(raw_files(&backup), originals);
}
#[test]
fn committed_journal_cleanup_does_not_undo_successful_restore() {
    let temp = tempfile::tempdir().unwrap();
    let root = album(&temp.path().join("album"), false);
    let data = root.path.join(".photosys");
    write_json(
        &data.join(JOURNAL),
        &Journal {
            format_version: 1,
            backup_directory: BACKUP.into(),
            staging_directory: None,
            original_files: vec![],
            committed: true,
        },
    )
    .unwrap();
    recover(&root).unwrap();
    assert!(data.join("library.sqlite").exists());
    assert!(!data.join(JOURNAL).exists());
}
#[cfg(unix)]
#[test]
fn symlinked_data_and_databases_are_rejected() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let source = album(&temp.path().join("source"), false);
    let zip = temp.path().join("backup.zip");
    export(&source, &zip, |_| {}).unwrap();
    let info = inspect_file(&zip);
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    symlink(source.path.join(".photosys"), target.join(".photosys")).unwrap();
    assert!(
        restore(
            &Root::new(&target).unwrap(),
            &zip,
            &info.fingerprint,
            BACKUP,
            |_| {},
            |_| Ok(())
        )
        .is_err()
    );
    let data = source.path.join(".photosys");
    fs::remove_file(data.join("library.sqlite")).unwrap();
    symlink(&zip, data.join("library.sqlite")).unwrap();
    assert!(export(&source, &temp.path().join("rejected.zip"), |_| {}).is_err());
    assert!(!temp.path().join("rejected.zip").exists());
}

#[test]
fn duplicated_zip_names_are_rejected_even_when_contents_and_hashes_match() {
    let temp = tempfile::tempdir().unwrap();
    let root = album(&temp.path().join("source"), false);
    let good = temp.path().join("good.zip");
    export(&root, &good, |_| {}).unwrap();
    let mut input = ZipArchive::new(File::open(&good).unwrap()).unwrap();
    let bad = temp.path().join("duplicate.zip");
    let mut output = ZipWriter::new(File::create(&bad).unwrap());
    for name in [MANIFEST, "library.sqlite", "xibrary.sqlite"] {
        let mut bytes = Vec::new();
        input
            .by_name(if name == "xibrary.sqlite" {
                "library.sqlite"
            } else {
                name
            })
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        output
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        output.write_all(&bytes).unwrap();
    }
    output.finish().unwrap();
    let mut bytes = fs::read(&bad).unwrap();
    for i in 0..bytes.len() - 14 {
        if &bytes[i..i + 14] == b"xibrary.sqlite" {
            bytes[i] = b'l';
        }
    }
    fs::write(&bad, bytes).unwrap();
    let work = tempfile::tempdir().unwrap();
    assert!(
        inspect(&bad, work.path(), &|_| {})
            .err()
            .unwrap()
            .contains("重复")
    );
}

#[test]
fn interrupted_replacement_at_every_file_is_recovered_before_library_opens() {
    let temp = tempfile::tempdir().unwrap();
    let source = album(&temp.path().join("source"), true);
    let zip = temp.path().join("backup.zip");
    export(&source, &zip, |_| {}).unwrap();
    let info = inspect_file(&zip);
    for fail_at in 0..database_files().len() {
        let root = album(&temp.path().join(format!("interrupted-{fail_at}")), false);
        let data = root.path.join(".photosys");
        let original = raw_files(&data);
        let mut count = 0;
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = restore(
                &root,
                &zip,
                &info.fingerprint,
                BACKUP,
                |_| {},
                |_| {
                    assert_ne!(count, fail_at, "simulated process termination");
                    count += 1;
                    Ok(())
                },
            );
        }));
        assert!(crashed.is_err());
        assert!(data.join(JOURNAL).exists());
        recover(&root).unwrap();
        assert_eq!(raw_files(&data), original);
        assert!(!data.join(JOURNAL).exists());
    }
}

#[test]
fn failed_export_keeps_existing_destination_and_newer_database_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = album(&temp.path().join("source"), true);
    let zip = temp.path().join("existing.zip");
    fs::write(&zip, b"previous backup").unwrap();
    let db = root.path.join(".photosys/people.sqlite");
    Connection::open(&db)
        .unwrap()
        .execute_batch("PRAGMA user_version=999")
        .unwrap();
    assert!(
        export(&root, &zip, |_| {})
            .unwrap_err()
            .contains("较新版本")
    );
    assert_eq!(fs::read(&zip).unwrap(), b"previous backup");
    fs::write(&db, b"broken sqlite").unwrap();
    assert!(export(&root, &zip, |_| {}).is_err());
    assert_eq!(fs::read(&zip).unwrap(), b"previous backup");
}

#[cfg(unix)]
#[test]
fn read_only_album_can_be_exported_but_cannot_be_overwritten() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = album(&temp.path().join("source"), false);
    let data = root.path.join(".photosys");
    let zip = temp.path().join("backup.zip");
    let original = raw_files(&data);
    fs::set_permissions(&data, fs::Permissions::from_mode(0o555)).unwrap();
    let exported = export(&root, &zip, |_| {});
    let restored = exported.as_ref().ok().map(|_| {
        let info = inspect_file(&zip);
        restore(&root, &zip, &info.fingerprint, BACKUP, |_| {}, |_| Ok(()))
    });
    fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
    exported.unwrap();
    assert!(restored.unwrap().is_err());
    assert_eq!(raw_files(&data), original);
    assert!(!data.join(JOURNAL).exists());
}
