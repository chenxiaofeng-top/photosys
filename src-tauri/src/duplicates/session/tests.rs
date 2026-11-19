use super::*;
use tempfile::tempdir;

fn write(root: &Path, name: &str, bytes: &[u8]) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn fixture(root: &Path) -> Session {
    Session::new(run_scan(42, root, &AtomicBool::new(false), |_| {}, true).unwrap())
}
fn apply(s: &mut Session, rule: KeepRule, dir: &str, query: &str) {
    s.selection(SelectionAction::Apply {
        filter: Filter {
            query: query.into(),
            kind: None,
        },
        rule,
        preferred_directory: dir.into(),
    })
    .unwrap();
}
fn id(s: &Session, path: &str) -> usize {
    s.scan
        .groups
        .iter()
        .flat_map(|g| &g.files)
        .find(|f| f.relative == Path::new(path))
        .unwrap()
        .id
}
fn keep_paths(s: &Session) -> Vec<String> {
    s.scan
        .groups
        .iter()
        .flat_map(|g| &g.files)
        .filter(|f| !s.selected.contains(&f.id))
        .map(Candidate::path_label)
        .collect()
}

#[test]
fn filters_match_a_member_and_return_the_whole_group_and_global_selection() {
    let root = tempdir().unwrap();
    write(root.path(), "sub/needle.jpg", b"a");
    write(root.path(), "renamed.bin", b"a");
    write(root.path(), "movie.mp4", b"bb");
    write(root.path(), "movie-copy.mp4", b"bb");
    let mut s = fixture(root.path());
    apply(&mut s, KeepRule::Shallow, "", "");
    let filter = Filter {
        query: "NEEDLE".into(),
        kind: Some(Kind::Photo),
    };
    let page = s.groups(&filter, Sort::ExtraBytes, 0);
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].count, 2);
    assert_eq!(page.summary.selected, 2);
    assert_eq!(page.summary.hidden_selected, 1);
    assert_eq!(s.members(page.items[0].id, 0).unwrap().items.len(), 2);
    assert_eq!(
        s.groups(
            &Filter {
                query: "needle".into(),
                kind: Some(Kind::Video)
            },
            Sort::Count,
            0
        )
        .total,
        0
    );
    // Applying a filtered rule replaces selection, including hidden selections.
    apply(&mut s, KeepRule::Shallow, "", "needle");
    assert_eq!(s.selected.len(), 1);
    assert_eq!(s.summary(&filter).hidden_selected, 0);
}

#[test]
fn retention_prefers_subdirectory_then_timestamp_and_lexical_tie() {
    let root = tempdir().unwrap();
    for name in ["a.jpg", "b.jpg", "chosen/deep/z.jpg", "chosen/deep/a.jpg"] {
        write(root.path(), name, b"same");
    }
    let mut s = fixture(root.path());
    for f in &mut s.scan.groups[0].files {
        f.stamp.modified = SystemTime::UNIX_EPOCH
            + Duration::from_secs(if f.relative == Path::new("b.jpg") {
                20
            } else {
                10
            });
    }
    apply(&mut s, KeepRule::Shallow, "", "");
    assert_eq!(keep_paths(&s), ["a.jpg"]);
    apply(&mut s, KeepRule::Newest, "", "");
    assert_eq!(keep_paths(&s), ["b.jpg"]);
    apply(&mut s, KeepRule::Oldest, "", "");
    assert_eq!(keep_paths(&s), ["a.jpg"]);
    apply(&mut s, KeepRule::Newest, "chosen", "");
    assert_eq!(keep_paths(&s), ["chosen/deep/a.jpg"]);
    let before = s.selected.clone();
    let revision = s.revision;
    assert!(
        s.selection(SelectionAction::Apply {
            filter: Filter::default(),
            rule: KeepRule::Shallow,
            preferred_directory: "../escape".into()
        })
        .is_err()
    );
    assert_eq!(s.selected, before);
    assert_eq!(s.revision, revision);
    assert!(validate_preferred(&s.scan.root, "missing").is_err());
}

#[test]
fn selection_prevents_last_copy_and_rejects_stale_confirmation_before_trash() {
    let root = tempdir().unwrap();
    write(root.path(), "a", b"same");
    write(root.path(), "b", b"same");
    let mut s = fixture(root.path());
    let a = id(&s, "a");
    let b = id(&s, "b");
    s.selection(SelectionAction::Toggle { file_id: b }).unwrap();
    let revision = s.revision;
    assert!(s.selection(SelectionAction::Toggle { file_id: a }).is_err());
    assert!(
        s.confirmation(revision, 0)
            .unwrap()
            .items
            .iter()
            .any(|r| !r.moving)
    );
    s.selection(SelectionAction::Clear).unwrap();
    assert!(s.confirmation(revision, 0).is_err());
    assert!(
        s.recycle(revision, |_| panic!("stale plan must not trash"))
            .is_err()
    );
    assert!(
        s.selection(SelectionAction::Toggle {
            file_id: usize::MAX
        })
        .is_err()
    );
}

#[test]
fn partial_cleanup_keeps_valid_failures_selected_and_pages_outcomes() {
    let root = tempdir().unwrap();
    for i in 0..32 {
        write(root.path(), &format!("{i:03}"), b"same");
    }
    let mut s = fixture(root.path());
    apply(&mut s, KeepRule::Shallow, "", "");
    let revision = s.revision;
    let counts = s
        .recycle(revision, |p| {
            if p.file_name().unwrap() == "001" {
                Err("test failure".into())
            } else {
                fs::remove_file(p).map_err(|e| e.to_string())
            }
        })
        .unwrap();
    assert_eq!((counts.moved, counts.failed), (30, 1));
    assert_eq!(s.selected.len(), 1);
    assert_eq!(s.summary(&Filter::default()).selected_bytes, 4);
    assert_eq!(s.outcomes.iter().skip(offset(1)).take(PAGE_SIZE).count(), 6);
    assert!(s.recycle(revision, |_| panic!("stale retry")).is_err());
    let counts = s
        .recycle(s.revision, |p| {
            fs::remove_file(p).map_err(|e| e.to_string())
        })
        .unwrap();
    assert_eq!(counts.moved, 1);
    assert!(s.scan.groups.is_empty());
    assert!(s.selected.is_empty());
}

#[test]
fn changed_group_is_locked_and_selection_is_cleared_after_cleanup() {
    let root = tempdir().unwrap();
    write(root.path(), "a", b"same");
    write(root.path(), "b", b"same");
    let mut s = fixture(root.path());
    apply(&mut s, KeepRule::Shallow, "", "");
    write(root.path(), "b", b"changed");
    let counts = s
        .recycle(s.revision, |_| panic!("changed file must not be trashed"))
        .unwrap();
    assert_eq!(counts.skipped, 1);
    assert!(s.selected.is_empty());
    assert!(s.scan.groups[0].needs_rescan);
    assert_eq!(s.summary(&Filter::default()).extras, 0);
    assert!(
        s.members(s.scan.groups[0].id, 0)
            .unwrap()
            .items
            .iter()
            .all(|f| f.disabled)
    );
}

#[test]
fn hardlinks_are_all_retained_but_regular_copies_can_be_selected() {
    let root = tempdir().unwrap();
    write(root.path(), "a", b"same");
    fs::hard_link(root.path().join("a"), root.path().join("b")).unwrap();
    write(root.path(), "c", b"same");
    write(root.path(), "d", b"same");
    let mut s = fixture(root.path());
    apply(&mut s, KeepRule::Shallow, "", "");
    assert_eq!(s.selected.len(), 2);
    assert_eq!(keep_paths(&s), ["a", "b"]);
    assert_eq!(s.summary(&Filter::default()).extras, 2);
    assert!(
        s.selection(SelectionAction::Toggle {
            file_id: id(&s, "a")
        })
        .is_err()
    );
}

#[test]
fn all_views_are_bounded_and_cross_page_selection_survives_with_100000_members() {
    let root = tempdir().unwrap();
    write(root.path(), "source", b"same");
    let mut s = fixture(root.path());
    let path = root.path().join("source");
    let stamp = FileStamp::read(&File::open(path).unwrap()).unwrap();
    s.scan.groups = vec![Group {
        id: 1,
        checksum: [0; 32],
        needs_rescan: false,
        files: (1..=100_000)
            .map(|id| Candidate {
                id,
                relative: PathBuf::from(format!("{id:06}.bin")),
                stamp: stamp.clone(),
            })
            .collect(),
    }];
    apply(&mut s, KeepRule::Shallow, "", "");
    assert_eq!(s.selected.len(), 99_999);
    let page = s.groups(&Filter::default(), Sort::ExtraBytes, 0);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].count, 100_000);
    assert!(serde_json::to_string(&page).unwrap().len() < 2000);
    assert_eq!(s.members(1, 0).unwrap().items.len(), 25);
    assert_eq!(s.members(1, 3999).unwrap().items.len(), 25);
    assert_eq!(s.members(1, usize::MAX).unwrap().items.len(), 0);
    s.selection(SelectionAction::Toggle { file_id: 100_000 })
        .unwrap();
    assert!(!s.members(1, 3999).unwrap().items[24].selected);
    assert!(s.members(1, 1).unwrap().items.iter().all(|f| f.selected));
    let confirmation = s.confirmation(s.revision, 3999).unwrap();
    assert_eq!(confirmation.total, 100_000);
    assert_eq!(confirmation.items.len(), 25);
    assert!(serde_json::to_string(&confirmation).unwrap().len() < 16000);
    // Many groups also produce a bounded page, regardless of the total count.
    let files = std::mem::take(&mut s.scan.groups[0].files);
    let mut iter = files.into_iter();
    s.scan.groups.clear();
    while let (Some(a), Some(b)) = (iter.next(), iter.next()) {
        s.scan.groups.push(Group {
            id: a.id,
            files: vec![a, b],
            checksum: [0; 32],
            needs_rescan: false,
        });
    }
    assert_eq!(s.groups(&Filter::default(), Sort::Count, 0).items.len(), 25);
    assert_eq!(
        s.groups(&Filter::default(), Sort::Count, 1999).items.len(),
        25
    );
}

#[test]
fn sorting_uses_removable_bytes_then_size_and_count_as_requested() {
    let root = tempdir().unwrap();
    for i in 0..4 {
        write(root.path(), &format!("small{i}"), b"1234");
    }
    for i in 0..2 {
        write(root.path(), &format!("large{i}"), b"12345678");
    }
    let s = fixture(root.path());
    assert_eq!(
        s.groups(&Filter::default(), Sort::ExtraBytes, 0).items[0].count,
        4
    );
    assert_eq!(s.groups(&Filter::default(), Sort::Size, 0).items[0].size, 8);
    assert_eq!(
        s.groups(&Filter::default(), Sort::Count, 0).items[0].count,
        4
    );
}

#[test]
fn validated_file_paths_do_not_use_library_ids_and_reject_replacements() {
    let root = tempdir().unwrap();
    write(root.path(), "a.jpg", b"same");
    write(root.path(), "b.jpg", b"same");
    let s = fixture(root.path());
    let b = id(&s, "b.jpg");
    assert_eq!(s.path(b).unwrap(), root.path().join("b.jpg"));
    assert!(s.path(usize::MAX).is_err());
    fs::remove_file(root.path().join("b.jpg")).unwrap();
    write(root.path(), "b.jpg", b"same");
    assert!(s.path(b).is_err());
}

#[cfg(unix)]
#[test]
fn preferred_directory_cannot_be_a_symlink() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
    let root = Root::new(root.path()).unwrap();
    assert!(validate_preferred(&root, "link").is_err());
}

#[test]
fn commands_cancel_publish_and_render_only_the_current_scan() {
    let root = tempdir().unwrap();
    image::RgbImage::new(32, 24)
        .save(root.path().join("a.png"))
        .unwrap();
    fs::copy(root.path().join("a.png"), root.path().join("b.png")).unwrap();
    let directory = root.path().to_str().unwrap().to_owned();
    // A cancel delivered before the async command is first polled is retained.
    cancel_duplicates(700).unwrap();
    let early = tauri::async_runtime::block_on(scan_duplicates(
        directory.clone(),
        700,
        Channel::new(|_| Ok(())),
    ))
    .unwrap();
    assert!(matches!(early, Outcome::Cancelled));
    let cancelled = tauri::async_runtime::block_on(scan_duplicates(
        directory.clone(),
        701,
        Channel::new(|_| {
            // Cancel while the worker is emitting its first update. This must not
            // contend on the long-lived result lock or miss startup registration.
            cancel_duplicates(701).unwrap();
            Ok(())
        }),
    ))
    .unwrap();
    assert!(matches!(cancelled, Outcome::Cancelled));
    assert!(session().unwrap().is_none());
    assert!(ACTIVE.lock().unwrap().is_none());
    let finished = tauri::async_runtime::block_on(scan_duplicates(
        directory.clone(),
        702,
        Channel::new(|_| {
            cancel_duplicates(701).unwrap(); // a late cancellation must not hit 702
            Ok(())
        }),
    ))
    .unwrap();
    assert!(matches!(finished, Outcome::Finished { .. }));
    assert!(duplicate_groups(701, Filter::default(), Sort::Count, 0).is_err());
    let groups = duplicate_groups(702, Filter::default(), Sort::Count, 0).unwrap();
    let file_id = groups.items[0].representative.file.id;
    let preview = tauri::async_runtime::block_on(duplicate_preview(702, file_id, false)).unwrap();
    assert!(preview.starts_with("data:image/jpeg;base64,"));
    assert_eq!(
        tauri::async_runtime::block_on(duplicate_preview(702, file_id, false)).unwrap(),
        preview
    );
    fs::write(
        root.path()
            .join(&groups.items[0].representative.file.relative_path),
        b"changed",
    )
    .unwrap();
    assert!(tauri::async_runtime::block_on(duplicate_preview(702, file_id, false)).is_err());
    assert!(
        tauri::async_runtime::block_on(scan_duplicates(
            directory + "/missing",
            703,
            Channel::new(|_| Ok(()))
        ))
        .is_err()
    );
    assert!(ACTIVE.lock().unwrap().is_none());
    assert!(session().unwrap().is_none());
    // An old worker's cleanup must not clear a newly registered job.
    let old = Arc::new(AtomicBool::new(false));
    let next = Arc::new(AtomicBool::new(false));
    let guard = ActiveGuard(old);
    *ACTIVE.lock().unwrap() = Some((704, next.clone()));
    drop(guard);
    assert!(
        ACTIVE
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(_, flag)| Arc::ptr_eq(flag, &next))
    );
    *ACTIVE.lock().unwrap() = None;
}
