use super::*;
use std::cell::RefCell;
use tempfile::{TempDir, tempdir};

fn write(root: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    path
}

fn scanned(root: &TempDir) -> (Scan, ScanResult) {
    scan(19, root.path(), |_| {}).unwrap()
}

fn file_id(scan: &Scan, path: &str) -> usize {
    scan.groups
        .iter()
        .flat_map(|group| &group.files)
        .find(|file| file.relative == Path::new(path))
        .unwrap()
        .id
}

fn remove_as_trash(path: &Path) -> Result<(), String> {
    fs::remove_file(path).map_err(|error| error.to_string())
}

#[test]
fn groups_by_contents_and_shallow_paths_with_empty_hidden_and_large_files() {
    let root = tempdir().unwrap();
    let first = vec![3; READ_BUFFER + 17];
    let mut different = first.clone();
    *different.last_mut().unwrap() = 8;
    write(root.path(), "z.data", &first);
    write(root.path(), "a.data", &first);
    write(root.path(), "deep/folder/renamed.jpg", &first);
    write(root.path(), "folder/a.data", &different);
    write(root.path(), "folder/other.bin", &different);
    write(root.path(), "other-size", b"unique");
    write(root.path(), ".hidden", b"");
    write(root.path(), "folder/empty", b"");

    let (_, result) = scanned(&root);
    assert_eq!(result.scanned, 8);
    assert!(result.issues.is_empty());
    assert_eq!(result.groups.len(), 3);
    assert_eq!(result.groups[0].size, first.len() as u64);
    assert_eq!(
        result.groups[0]
            .files
            .iter()
            .map(|file| file.name.as_str())
            .collect::<Vec<_>>(),
        ["a.data", "z.data", "renamed.jpg"],
    );
    assert_eq!(result.groups[1].files[0].name, "a.data");
    assert_eq!(result.groups[2].size, 0);
    assert_eq!(result.groups[2].files[0].name, ".hidden");
}

#[test]
fn reports_disappearing_files_and_keeps_valid_groups() {
    let root = tempdir().unwrap();
    let vanishing = write(root.path(), "vanishing", b"same");
    write(root.path(), "first", b"same");
    write(root.path(), "second", b"same");
    let (_, result) = scan(19, root.path(), |progress| {
        if matches!(
            progress,
            ScanProgress {
                phase: "hashing",
                checked: 0,
                ..
            }
        ) && vanishing.exists()
        {
            fs::remove_file(&vanishing).unwrap();
        }
    })
    .unwrap();

    assert_eq!(result.scanned, 3);
    assert_eq!(result.groups.len(), 1);
    assert_eq!(result.groups[0].files.len(), 2);
    assert_eq!(result.issues.len(), 1);
    assert_eq!(result.issues[0].path, "vanishing");
}

#[cfg(unix)]
#[test]
fn skips_symbolic_links_and_special_files_and_reports_unreadable_paths() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt, symlink};

    let root = tempdir().unwrap();
    let other_root = tempdir().unwrap();
    let a = write(root.path(), "a", b"same");
    write(root.path(), "b", b"same");
    write(other_root.path(), "outside", b"same");
    symlink(&a, root.path().join("link")).unwrap();
    symlink(other_root.path(), root.path().join("directory-link")).unwrap();
    let pipe = CString::new(root.path().join("pipe").as_os_str().as_bytes()).unwrap();
    // The pathname is NUL-terminated and points inside this test's temporary directory.
    assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
    let unreadable = write(root.path(), "unreadable", b"unreadable");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    let (_, result) = scanned(&root);
    assert_eq!(result.scanned, 3);
    assert_eq!(result.groups[0].files.len(), 2);
    if File::open(&unreadable).is_err() {
        assert!(result.issues.iter().any(|issue| issue.path == "unreadable"));
    }
}

#[test]
fn lists_hard_links_but_refuses_to_move_them() {
    let root = tempdir().unwrap();
    let first = write(root.path(), "original", b"same");
    fs::hard_link(&first, root.path().join("link")).unwrap();
    write(root.path(), "independent", b"same");
    let (mut scan, result) = scanned(&root);
    let files = &result.groups[0].files;
    assert_eq!(files.len(), 3);
    assert_eq!(files.iter().filter(|file| file.hard_link).count(), 2);
    let link_id = file_id(&scan, "link");
    assert!(
        scan.recycle(19, vec![link_id], |_| panic!("unexpected move"))
            .is_err()
    );
    let independent_id = file_id(&scan, "independent");
    let result = scan
        .recycle(19, vec![independent_id], remove_as_trash)
        .unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Moved);
    assert!(first.exists());
    assert!(root.path().join("link").exists());
}

#[test]
fn rejects_stale_unknown_and_full_group_requests_before_moving_any_file() {
    let root = tempdir().unwrap();
    write(root.path(), "a", b"same");
    write(root.path(), "b", b"same");
    write(root.path(), "c", b"another");
    write(root.path(), "d", b"another");
    let (mut scan, _) = scanned(&root);
    let a = file_id(&scan, "a");
    let b = file_id(&scan, "b");
    let c = file_id(&scan, "c");

    for (id, selection) in [
        (18, vec![a]),
        (19, vec![]),
        (19, vec![a, usize::MAX]),
        (19, vec![a, b, c]),
    ] {
        assert!(
            scan.recycle(id, selection, |_| panic!("unexpected move"))
                .is_err()
        );
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 4);
}

#[test]
fn skips_replacements_even_when_the_replacement_has_equal_bytes() {
    let root = tempdir().unwrap();
    write(root.path(), "keeping", b"same");
    let target = write(root.path(), "target", b"same");
    let (mut scan, _) = scanned(&root);
    let id = file_id(&scan, "target");
    fs::rename(&target, root.path().join("old-target")).unwrap();
    fs::write(&target, b"same").unwrap();

    let result = scan
        .recycle(19, vec![id], |_| panic!("unexpected move"))
        .unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Skipped);
    assert!(result.groups[0].needs_rescan);
    assert!(target.exists());
    assert!(
        scan.recycle(19, vec![id], |_| panic!("unexpected move"))
            .is_err()
    );
}

#[test]
fn skips_only_the_changed_copy_during_the_current_batch() {
    let root = tempdir().unwrap();
    write(root.path(), "keep", b"same");
    let changed = write(root.path(), "target-changed", b"same");
    let valid = write(root.path(), "target-valid", b"same");
    let (mut scan, _) = scanned(&root);
    let ids = vec![
        file_id(&scan, "target-changed"),
        file_id(&scan, "target-valid"),
    ];
    fs::write(&changed, b"changed").unwrap();

    let result = scan.recycle(19, ids, remove_as_trash).unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Skipped);
    assert_eq!(result.outcomes[1].status, TrashStatus::Moved);
    assert!(result.groups[0].needs_rescan);
    assert!(changed.exists());
    assert!(!valid.exists());
}

#[test]
fn verifies_the_retained_file_again_before_each_move_and_continues_other_groups() {
    let root = tempdir().unwrap();
    let keep = write(root.path(), "keep", b"longer bytes");
    let target_a = write(root.path(), "target-a", b"longer bytes");
    let target_b = write(root.path(), "target-b", b"longer bytes");
    let target_c = write(root.path(), "target-c", b"longer bytes");
    write(root.path(), "other-keep", b"other");
    let other_target = write(root.path(), "other-target", b"other");
    let (mut scan, _) = scanned(&root);
    let ids = ["target-a", "target-b", "target-c", "other-target"].map(|name| file_id(&scan, name));

    let result = scan
        .recycle(19, ids.to_vec(), |path| {
            let result = remove_as_trash(path);
            if path.ends_with("target-a") {
                fs::remove_file(&keep).unwrap();
            }
            result
        })
        .unwrap();

    assert_eq!(
        result
            .outcomes
            .iter()
            .map(|outcome| &outcome.status)
            .collect::<Vec<_>>(),
        [
            &TrashStatus::Moved,
            &TrashStatus::Skipped,
            &TrashStatus::Skipped,
            &TrashStatus::Moved,
        ]
    );
    assert_eq!(result.groups.len(), 1);
    assert!(result.groups[0].needs_rescan);
    assert!(!target_a.exists());
    assert!(target_b.exists());
    assert!(target_c.exists());
    assert!(!other_target.exists());
}

#[test]
fn removes_only_successful_items_and_allows_retrying_failures() {
    let root = tempdir().unwrap();
    let keeper = write(root.path(), "a", b"same");
    let failure = write(root.path(), "b", b"same");
    let success = write(root.path(), "c", b"same");
    let (mut scan, _) = scanned(&root);
    let b = file_id(&scan, "b");
    let c = file_id(&scan, "c");
    let calls = RefCell::new(Vec::new());

    let result = scan
        .recycle(19, vec![b, c], |path| {
            calls.borrow_mut().push(path.to_path_buf());
            if path.ends_with("b") {
                Err("system trash is unavailable".into())
            } else {
                remove_as_trash(path)
            }
        })
        .unwrap();

    assert_eq!(
        calls.into_inner(),
        [failure.canonicalize().unwrap(), scan.root.path.join("c")]
    );
    assert_eq!(result.outcomes[0].status, TrashStatus::Failed);
    assert_eq!(result.outcomes[1].status, TrashStatus::Moved);
    assert_eq!(result.groups[0].files.len(), 2);
    assert!(!result.groups[0].needs_rescan);
    assert!(failure.exists());
    assert!(!success.exists());

    let result = scan.recycle(19, vec![b], remove_as_trash).unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Moved);
    assert!(result.groups.is_empty());
    assert!(keeper.exists());
}

#[cfg(unix)]
#[test]
fn rejects_a_parent_directory_replaced_by_a_link() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let elsewhere = tempdir().unwrap();
    write(root.path(), "keep", b"same");
    write(root.path(), "folder/target", b"same");
    let (mut scan, _) = scanned(&root);
    let id = file_id(&scan, "folder/target");
    let parent = root.path().join("folder");
    let moved_parent = elsewhere.path().join("folder");
    fs::rename(&parent, &moved_parent).unwrap();
    symlink(&moved_parent, &parent).unwrap();

    let result = scan
        .recycle(19, vec![id], |_| panic!("unexpected move"))
        .unwrap();
    assert_eq!(result.outcomes[0].status, TrashStatus::Skipped);
    assert!(moved_parent.join("target").exists());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a GIO desktop session with an isolated XDG_DATA_HOME"]
fn moves_and_restores_a_file_with_the_linux_system_trash() {
    let data_home = PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap());
    let disposable = tempfile::tempdir_in(&data_home).unwrap();
    let keeper = write(disposable.path(), "a", b"photosys trash smoke test");
    let filename = format!(
        "photosys-trash-{}",
        disposable.path().file_name().unwrap().display()
    );
    let target = write(disposable.path(), &filename, b"photosys trash smoke test");
    let (mut scan, _) = scanned(&disposable);
    let id = file_id(&scan, &filename);
    let result = scan.recycle(19, vec![id], trash).unwrap();
    assert_eq!(
        result.outcomes[0].status,
        TrashStatus::Moved,
        "{:?}",
        result.outcomes[0].reason
    );
    assert!(!target.exists());
    assert!(keeper.exists());

    let trashed = data_home.join("Trash/files").join(&filename);
    let info = data_home
        .join("Trash/info")
        .join(format!("{filename}.trashinfo"));
    assert_eq!(fs::read(&trashed).unwrap(), fs::read(&keeper).unwrap());
    let info_text = fs::read_to_string(info).unwrap();
    assert!(info_text.contains(&format!("Path={}", target.display())));

    let restore = std::process::Command::new("gio")
        .args(["trash", "--restore", &format!("trash:///{filename}")])
        .output()
        .unwrap();
    assert!(
        restore.status.success(),
        "{}",
        String::from_utf8_lossy(&restore.stderr)
    );
    assert_eq!(fs::read(target).unwrap(), fs::read(keeper).unwrap());
    assert!(!trashed.exists());
}

#[test]
fn sampling_is_only_a_prefilter_and_does_not_miss_unsampled_differences() {
    let root = tempdir().unwrap();
    let a = vec![7; READ_BUFFER * 8];
    let mut b = a.clone();
    b[READ_BUFFER + 17] = 9;
    write(root.path(), "a", &a);
    write(root.path(), "a-copy", &a);
    write(root.path(), "b", &b);
    write(root.path(), "b-copy", &b);
    let (scan, result) = scanned(&root);
    assert_eq!(result.groups.len(), 2);
    assert!(result.groups.iter().all(|g| g.files.len() == 2));
    let first = &scan.groups[0].files[0];
    let second = &scan.groups[1].files[0];
    assert_eq!(
        scan.root.sample(first, &mut |_| Ok(())).unwrap(),
        scan.root.sample(second, &mut |_| Ok(())).unwrap()
    );
    assert_ne!(
        scan.root.checksum(first, &mut |_| Ok(())).unwrap(),
        scan.root.checksum(second, &mut |_| Ok(())).unwrap()
    );
}

#[test]
fn cancellation_is_observed_in_every_phase_and_at_read_boundaries() {
    let root = tempdir().unwrap();
    let data = vec![1; READ_BUFFER * 8];
    write(root.path(), "a", &data);
    write(root.path(), "b", &data);
    for phase in ["scanning", "sampling", "hashing", "comparing"] {
        let flag = AtomicBool::new(false);
        let result = run_scan(
            1,
            root.path(),
            &flag,
            |p| {
                if p.phase == phase {
                    flag.store(true, Ordering::Relaxed);
                }
            },
            true,
        );
        assert!(result.is_err(), "phase {phase}");
        let scan = run_scan(2, root.path(), &AtomicBool::new(false), |_| {}, true).unwrap();
        assert_eq!(scan.groups.len(), 1);
    }
    let (scan, _) = scanned(&root);
    let candidate = &scan.groups[0].files[0];
    let mut bytes = 0;
    assert!(
        scan.root
            .checksum(candidate, &mut |n| {
                bytes += n;
                if bytes >= READ_BUFFER {
                    Err(io::ErrorKind::Interrupted.into())
                } else {
                    Ok(())
                }
            })
            .is_err()
    );
    assert_eq!(bytes, READ_BUFFER);
    let mut bytes = 0;
    assert!(
        scan.root
            .compare_with(candidate, &scan.groups[0].files[1], None, &mut |n| {
                bytes += n;
                if bytes > 0 {
                    Err(io::ErrorKind::Interrupted.into())
                } else {
                    Ok(())
                }
            })
            .is_err()
    );
    assert_eq!(bytes, READ_BUFFER * 2);
    let mut bytes = 0;
    assert!(
        scan.root
            .sample(candidate, &mut |n| {
                bytes += n;
                if bytes > 0 {
                    Err(io::ErrorKind::Interrupted.into())
                } else {
                    Ok(())
                }
            })
            .is_err()
    );
    assert_eq!(bytes, READ_BUFFER);
}

#[test]
fn byte_updates_continue_inside_a_single_file_and_sample_detects_mutation() {
    let root = tempdir().unwrap();
    let data = vec![1; READ_BUFFER * 8];
    let path = write(root.path(), "a", &data);
    write(root.path(), "b", &data);
    let (scan, _) = scanned(&root);
    let candidate = scan.groups[0]
        .files
        .iter()
        .find(|f| f.relative == Path::new("a"))
        .unwrap();
    let flag = AtomicBool::new(false);
    let mut updates = Vec::new();
    let mut callback = |p: ScanProgress| updates.push(p);
    let mut reporter = Reporter {
        update: ScanProgress {
            phase: "hashing",
            found: 2,
            checked: 0,
            total: 2,
            bytes_read: 0,
            current_file: "a".into(),
            elapsed_ms: 0,
        },
        start: Instant::now(),
        last: Instant::now(),
        cancelled: &flag,
        send: &mut callback,
    };
    scan.root
        .checksum(candidate, &mut |n| {
            reporter.last = Instant::now() - PROGRESS_INTERVAL;
            reporter.tick(n, false)
        })
        .unwrap();
    assert!(
        updates
            .iter()
            .filter(|p| p.bytes_read > 0 && p.checked == 0)
            .count()
            > 1
    );
    let mut mutated = false;
    assert!(
        scan.root
            .sample(candidate, &mut |n| {
                if n > 0 && !mutated {
                    mutated = true;
                    fs::write(&path, b"changed").unwrap();
                }
                Ok(())
            })
            .is_err()
    );
}

fn signature(scan: &Scan) -> Vec<Vec<String>> {
    let mut groups: Vec<_> = scan
        .groups
        .iter()
        .map(|g| {
            let mut files: Vec<_> = g.files.iter().map(Candidate::path_label).collect();
            files.sort();
            files
        })
        .collect();
    groups.sort();
    groups
}

#[test]
#[ignore = "disk benchmark; run explicitly with --ignored --nocapture"]
fn benchmark_sampling_against_full_hash_pipeline() {
    for (label, count, size, distinct) in [
        ("small", 200, 1024, true),
        ("same-size-distinct", 16, 2 * 1024 * 1024, true),
        ("true-copies", 16, 1024 * 1024, false),
    ] {
        let root = tempdir().unwrap();
        for i in 0..count {
            let data = vec![if distinct { (i % 100) as u8 } else { 7 }; size];
            write(root.path(), &format!("{i:05}.bin"), &data);
        }
        let baseline = run_scan(1, root.path(), &AtomicBool::new(false), |_| {}, false).unwrap();
        let optimized = run_scan(2, root.path(), &AtomicBool::new(false), |_| {}, true).unwrap();
        assert_eq!(signature(&baseline), signature(&optimized));
        println!(
            "{label}: full-hash {} bytes / {} ms; sampled {} bytes / {} ms; groups {}",
            baseline.bytes_read,
            baseline.elapsed_ms,
            optimized.bytes_read,
            optimized.elapsed_ms,
            optimized.groups.len()
        );
        if label == "same-size-distinct" {
            assert!(optimized.bytes_read < baseline.bytes_read / 4);
        }
    }
}

#[test]
#[ignore = "creates and scans 100000 real files; run explicitly"]
fn scans_100000_real_files_with_one_large_duplicate_group() {
    let root = tempdir().unwrap();
    for i in 0..100_000 {
        write(root.path(), &format!("{i:06}.bin"), b"");
    }
    let scan = run_scan(1, root.path(), &AtomicBool::new(false), |_| {}, true).unwrap();
    assert_eq!(scan.scanned, 100_000);
    assert_eq!(scan.groups.len(), 1);
    assert_eq!(scan.groups[0].files.len(), 100_000);
    println!(
        "100000 empty files: {} ms, {} content bytes read",
        scan.elapsed_ms, scan.bytes_read
    );
}
