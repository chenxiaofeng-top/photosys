use super::*;
use crate::library::{OpenMode, TEST_SESSION, open_directory};

pub(super) fn start_legacy_database(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch("PRAGMA page_size=4096; VACUUM")
        .unwrap();
}

type StoredEmbedding = (
    Vec<u8>,
    Vec<u8>,
    i64,
    String,
    Option<Vec<u8>>,
    Option<String>,
);

fn stored_embeddings(path: &Path) -> Vec<StoredEmbedding> {
    let connection = open_index_connection(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT path, stamp, rotation, version, vector, error FROM embeddings ORDER BY path",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn new_databases_use_larger_pages_without_migrating_existing_layouts() {
    let directory = tempfile::tempdir().unwrap();
    let new = directory.path().join("new.sqlite");
    let old = directory.path().join("old.sqlite");
    start_legacy_database(&old);
    for (path, expected) in [(&new, INDEX_PAGE_SIZE), (&old, 4096)] {
        let cache = Cache::at(path, true).unwrap().unwrap();
        assert_eq!(index_page_size(&cache.connection).unwrap(), expected);
        drop(cache);
        let cache = Cache::at(path, false).unwrap().unwrap();
        assert_eq!(index_page_size(&cache.connection).unwrap(), expected);
    }
}

#[test]
fn read_only_legacy_index_is_still_searchable() {
    let _session = TEST_SESSION.lock().unwrap();
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let album = tempfile::tempdir().unwrap();
    image::RgbImage::new(2, 2)
        .save(album.path().join("photo.png"))
        .unwrap();
    let scan_id = open_directory(album.path().to_str().unwrap(), OpenMode::Select, |_| {})
        .unwrap()
        .scan_id;
    let scan = scan(scan_id).unwrap();
    let path = index_path(&scan).unwrap();
    start_legacy_database(&path);
    let mut cache = Cache::open(&scan, true).unwrap().unwrap();
    cache
        .save(&[(
            b"photo.png".to_vec(),
            scan.items[0].file.stamp.portable_key(),
            0,
            Ok(vec![1.0; DIM]),
        )])
        .unwrap();
    drop(cache);
    let original = fs::metadata(&path).unwrap().permissions();
    let mut read_only = original.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&path, read_only).unwrap();
    let state = status(scan_id);
    let maintenance = optimize_index_storage(scan_id, |_| Ok(u64::MAX));
    let filter: Filter = serde_json::from_value(serde_json::json!({
        "unknownDate": false, "favoritesOnly": false
    }))
    .unwrap();
    let search =
        SearchReader::default().search(scan_id, "照片", filter, 0, 24, |_| Ok(vec![1.; DIM]));
    fs::set_permissions(&path, original).unwrap();
    let state = state.unwrap();
    assert!(!state.can_optimize_storage);
    assert!(maintenance.is_err());
    assert_eq!(search.unwrap().total, 1);
    *session().unwrap() = None;
}

#[test]
fn optimization_preserves_index_contents_through_backup_and_restore() {
    let _session = TEST_SESSION.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let album = temp.path().join("source");
    fs::create_dir(&album).unwrap();
    image::RgbImage::new(2, 2)
        .save(album.join("photo.png"))
        .unwrap();
    let scan_id = open_directory(album.to_str().unwrap(), OpenMode::Select, |_| {})
        .unwrap()
        .scan_id;
    let scan = scan(scan_id).unwrap();
    let path = index_path(&scan).unwrap();
    start_legacy_database(&path);
    let mut cache = Cache::open(&scan, true).unwrap().unwrap();
    cache
        .save(&[
            (
                b"photo.png".to_vec(),
                scan.items[0].file.stamp.portable_key(),
                0,
                Ok(vec![1.0; DIM]),
            ),
            (
                b"broken.png".to_vec(),
                vec![2],
                90,
                Err("decode failed".into()),
            ),
        ])
        .unwrap();
    cache
        .connection
        .execute("INSERT INTO settings VALUES ('enabled', 1)", [])
        .unwrap();
    drop(cache);
    let before = stored_embeddings(&path);
    assert!(status(scan_id).unwrap().can_optimize_storage);
    let mut reader = SEARCH_READER.lock().unwrap();
    let filter: Filter = serde_json::from_value(serde_json::json!({
        "unknownDate": false, "favoritesOnly": false
    }))
    .unwrap();
    let result = reader
        .search(scan_id, "照片", filter.clone(), 0, 24, |_| {
            Ok(vec![1.; DIM])
        })
        .unwrap();
    assert_eq!(
        (result.total, result.indexed, result.pending, result.failed),
        (1, 1, 0, 0)
    );
    drop(reader);
    assert!(
        optimize_index_storage(scan_id, |_| Ok(0))
            .unwrap_err()
            .contains("磁盘空间不足")
    );
    assert_eq!(stored_embeddings(&path), before);

    let other_task = crate::file_task().unwrap();
    assert!(
        optimize_index_storage(scan_id, |_| Ok(u64::MAX))
            .unwrap_err()
            .contains("已有文件任务")
    );
    drop(other_task);
    optimize_index_storage(scan_id, |_| Ok(u64::MAX)).unwrap();
    optimize_index_storage(scan_id, |_| panic!("already optimized")).unwrap();
    assert!(SEARCH_READER.lock().unwrap().connection.is_none());
    assert_eq!(stored_embeddings(&path), before);
    let state = status(scan_id).unwrap();
    assert!(state.enabled);
    assert!(!state.can_optimize_storage);
    let result = SEARCH_READER
        .lock()
        .unwrap()
        .search(scan_id, "照片", filter, 0, 24, |_| Ok(vec![1.; DIM]))
        .unwrap();
    assert_eq!(result.items[0].id, 0);
    close_reader().unwrap();

    let zip = temp.path().join("backup.zip");
    crate::database_backup::export(&scan.root, &zip, |_| {}).unwrap();
    let work = tempfile::tempdir().unwrap();
    let inspection = crate::database_backup::inspect(&zip, work.path(), &|_| {}).unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    let target = crate::duplicates::Root::new(&target).unwrap();
    crate::database_backup::restore(
        &target,
        &zip,
        &inspection.fingerprint,
        "restore-backup-12345678-1234-1234-1234-123456789abc",
        |_| {},
        |_| Ok(()),
    )
    .unwrap();
    let restored = target.path.join(".photosys/ai.sqlite");
    let connection = open_index_connection(&restored, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(index_page_size(&connection).unwrap(), INDEX_PAGE_SIZE);
    assert_eq!(stored_embeddings(&restored), before);
    *session().unwrap() = None;
}

#[test]
fn wal_index_keeps_its_layout_and_stays_searchable() {
    let _session = TEST_SESSION.lock().unwrap();
    let album = tempfile::tempdir().unwrap();
    image::RgbImage::new(2, 2)
        .save(album.path().join("photo.png"))
        .unwrap();
    let scan_id = open_directory(album.path().to_str().unwrap(), OpenMode::Select, |_| {})
        .unwrap()
        .scan_id;
    let scan = scan(scan_id).unwrap();
    let path = index_path(&scan).unwrap();
    assert!(optimize_index_storage(scan_id, |_| Ok(u64::MAX)).is_err());
    assert!(!path.exists());
    start_legacy_database(&path);
    let mut cache = Cache::open(&scan, true).unwrap().unwrap();
    cache
        .connection
        .execute_batch("PRAGMA journal_mode=WAL")
        .unwrap();
    cache
        .save(&[(
            b"photo.png".to_vec(),
            scan.items[0].file.stamp.portable_key(),
            0,
            Ok(vec![1.0; DIM]),
        )])
        .unwrap();
    let before = stored_embeddings(&path);
    assert!(!status(scan_id).unwrap().can_optimize_storage);
    assert!(
        optimize_index_storage(scan_id, |_| Ok(u64::MAX))
            .unwrap_err()
            .contains("WAL")
    );
    assert!(optimize_index_storage(scan_id + 1, |_| Ok(u64::MAX)).is_err());
    assert_eq!(index_page_size(&cache.connection).unwrap(), 4096);
    assert!(index_uses_wal(&cache.connection).unwrap());
    assert_eq!(stored_embeddings(&path), before);
    let filter: Filter = serde_json::from_value(serde_json::json!({
        "unknownDate": false, "favoritesOnly": false
    }))
    .unwrap();
    let result = SearchReader::default()
        .search(scan_id, "照片", filter, 0, 24, |_| Ok(vec![1.; DIM]))
        .unwrap();
    assert_eq!(result.total, 1);
    *session().unwrap() = None;
}
