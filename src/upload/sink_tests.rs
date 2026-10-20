//! One table of behavior every sink must show, run against the host sink and
//! the remote receiver alike, so the two can never drift apart.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::path::RelPath;
use super::session::{DirectoryAttrs, EntryStart};
use super::sink::{Committed, UploadSink};

pub(crate) fn dir(sink: &mut dyn UploadSink, path: &str) {
    sink.start(&EntryStart::Directory {
        path: RelPath::test_new(path),
    })
    .unwrap();
}

pub(crate) fn file(
    sink: &mut dyn UploadSink,
    path: &str,
    body: &[u8],
    mode: u32,
    mtime: Option<i64>,
) {
    sink.start(&EntryStart::File {
        path: RelPath::test_new(path),
        size: body.len() as u64,
        mode,
        mtime,
    })
    .unwrap();
    for chunk in body.chunks(4096) {
        sink.data(chunk).unwrap();
    }
    sink.end_file().unwrap();
}

pub(crate) fn link(sink: &mut dyn UploadSink, path: &str, target: &str) {
    sink.start(&EntryStart::Symlink {
        path: RelPath::test_new(path),
        target: target.into(),
    })
    .unwrap();
}

pub(crate) fn attrs(path: &str, mode: u32, mtime: Option<i64>) -> DirectoryAttrs {
    DirectoryAttrs {
        path: RelPath::test_new(path),
        mode,
        mtime,
    }
}

pub(crate) fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

pub(crate) fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

pub(crate) fn mtime_of(path: &Path) -> i64 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

pub(crate) fn stages(dest: &Path) -> Vec<String> {
    std::fs::read_dir(dest)
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(super::STAGE_PREFIX))
        .collect()
}

pub(crate) fn published(committed: Committed) -> Vec<(String, String)> {
    match committed {
        Committed::Published { mut paths, .. } => {
            paths.sort();
            paths
        }
        Committed::Conflict(names) => panic!("unexpected conflict on {names:?}"),
    }
}

pub(crate) fn canonical(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

/// Generates the shared sink tests for `$factory: fn(&str) -> Box<dyn UploadSink>`.
macro_rules! sink_contract {
    ($factory:path) => {
        mod contract {
            use super::*;
            use crate::api::schema::PaneUploadCollision;
            use crate::upload::sink::Committed;
            use crate::upload::sink_tests::*;
            use crate::upload::test_support::TempDir;
            use std::os::unix::fs::PermissionsExt as _;

            #[test]
            fn begin_resolves_the_destination_and_reports_existing_names() {
                let dest = TempDir::new("contract-begin");
                std::fs::write(dest.join("here"), b"").unwrap();
                let mut sink = $factory(dest.str());

                let began = sink.begin(&names(&["here", "new"])).unwrap();

                assert_eq!(began.destination, canonical(dest.path()));
                assert_eq!(began.conflicts, ["here"]);
                assert_eq!(stages(dest.path()).len(), 1, "a stage exists while staging");
                sink.abort();
            }

            #[test]
            fn a_tree_publishes_with_modes_times_and_unfollowed_symlinks() {
                let dest = TempDir::new("contract-tree");
                let mut sink = $factory(dest.str());
                sink.begin(&names(&["t", "top.txt"])).unwrap();
                dir(sink.as_mut(), "t");
                dir(sink.as_mut(), "t/sub");
                file(
                    sink.as_mut(),
                    "t/sub/run.sh",
                    b"#!/bin/sh\n",
                    0o755,
                    Some(1_600_000_000),
                );
                link(sink.as_mut(), "t/up", "../../etc");
                file(sink.as_mut(), "top.txt", &vec![7u8; 70_000], 0o600, None);

                let committed = sink
                    .commit(
                        PaneUploadCollision::Fail,
                        &[attrs("t/sub", 0o750, Some(1_500_000_000))],
                        &[attrs("t", 0o555, Some(1_400_000_000))],
                    )
                    .unwrap();

                let root = canonical(dest.path());
                assert_eq!(
                    published(committed),
                    [
                        ("t".to_owned(), format!("{root}/t")),
                        ("top.txt".to_owned(), format!("{root}/top.txt"))
                    ]
                );
                let tree = dest.join("t");
                assert_eq!(mode_of(&tree.join("sub/run.sh")), 0o755);
                assert_eq!(mtime_of(&tree.join("sub/run.sh")), 1_600_000_000);
                assert_eq!(mode_of(&tree.join("sub")), 0o750);
                assert_eq!(mtime_of(&tree.join("sub")), 1_500_000_000);
                assert_eq!(mode_of(&tree), 0o555);
                assert_eq!(mtime_of(&tree), 1_400_000_000);
                assert_eq!(
                    std::fs::read_link(tree.join("up")).unwrap(),
                    std::path::Path::new("../../etc")
                );
                assert_eq!(
                    std::fs::read(dest.join("top.txt")).unwrap(),
                    vec![7u8; 70_000]
                );
                assert_eq!(mode_of(&dest.join("top.txt")), 0o600);
                assert!(stages(dest.path()).is_empty());
            }

            #[test]
            fn rename_numbers_conflicting_names_keeping_file_extensions() {
                let dest = TempDir::new("contract-rename");
                std::fs::write(dest.join("a.tar.gz"), b"old").unwrap();
                std::fs::create_dir(dest.join("d.d")).unwrap();
                let mut sink = $factory(dest.str());
                sink.begin(&names(&["a.tar.gz", "d.d"])).unwrap();
                file(sink.as_mut(), "a.tar.gz", b"new", 0o644, None);
                dir(sink.as_mut(), "d.d");

                let committed = sink.commit(PaneUploadCollision::Rename, &[], &[]).unwrap();

                let root = canonical(dest.path());
                assert_eq!(
                    published(committed),
                    [
                        ("a.tar.gz".to_owned(), format!("{root}/a.tar (1).gz")),
                        ("d.d".to_owned(), format!("{root}/d.d (1)"))
                    ]
                );
                assert_eq!(std::fs::read(dest.join("a.tar.gz")).unwrap(), b"old");
            }

            #[test]
            fn replace_swaps_whole_entries_without_merging_directories() {
                let dest = TempDir::new("contract-replace");
                std::fs::write(dest.join("f"), b"old").unwrap();
                std::fs::create_dir(dest.join("d")).unwrap();
                std::fs::write(dest.join("d/stale"), b"").unwrap();
                let mut sink = $factory(dest.str());
                sink.begin(&names(&["f", "d"])).unwrap();
                file(sink.as_mut(), "f", b"new", 0o644, None);
                dir(sink.as_mut(), "d");
                file(sink.as_mut(), "d/fresh", b"", 0o644, None);

                sink.commit(PaneUploadCollision::Replace, &[], &[]).unwrap();

                assert_eq!(std::fs::read(dest.join("f")).unwrap(), b"new");
                assert!(dest.join("d/fresh").exists());
                assert!(!dest.join("d/stale").exists());
                assert!(stages(dest.path()).is_empty());
            }

            #[test]
            fn fail_keeps_the_stage_on_conflict_and_a_second_commit_can_rename() {
                let dest = TempDir::new("contract-fail");
                std::fs::write(dest.join("a"), b"old").unwrap();
                let mut sink = $factory(dest.str());
                sink.begin(&names(&["a", "b"])).unwrap();
                file(sink.as_mut(), "a", b"new", 0o644, None);
                file(sink.as_mut(), "b", b"new", 0o644, None);

                let first = sink.commit(PaneUploadCollision::Fail, &[], &[]).unwrap();

                assert_eq!(first, Committed::Conflict(names(&["a"])));
                assert!(!dest.join("b").exists(), "nothing is published on conflict");
                assert_eq!(stages(dest.path()).len(), 1);
                let second = sink.commit(PaneUploadCollision::Rename, &[], &[]).unwrap();
                assert_eq!(published(second).len(), 2);
                assert_eq!(std::fs::read(dest.join("a (1)")).unwrap(), b"new");
            }

            #[test]
            fn abort_removes_the_stage_even_with_read_only_directories() {
                let dest = TempDir::new("contract-abort");
                let mut sink = $factory(dest.str());
                sink.begin(&names(&["ro"])).unwrap();
                dir(sink.as_mut(), "ro");
                dir(sink.as_mut(), "ro/inner");
                file(sink.as_mut(), "ro/inner/f", b"x", 0o444, None);
                let stage = dest.join(&stages(dest.path())[0]);
                // A remote receiver writes asynchronously.
                let written = stage.join("i/ro/inner/f");
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while !written.exists() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                std::fs::set_permissions(
                    stage.join("i/ro/inner"),
                    std::fs::Permissions::from_mode(0o555),
                )
                .unwrap();

                sink.abort();

                assert!(stages(dest.path()).is_empty());
                assert!(!dest.join("ro").exists());
            }

            #[test]
            fn a_destination_that_resolves_to_a_name_with_control_characters_is_refused() {
                let dir = TempDir::new("contract-control");
                let real = dir.join("x\u{1b}[201~\nrm -rf ~");
                std::fs::create_dir(&real).unwrap();
                std::os::unix::fs::symlink(&real, dir.join("plain")).unwrap();

                let error = $factory(dir.join("plain").to_str().unwrap())
                    .begin(&names(&["a"]))
                    .unwrap_err();

                assert_eq!(error.0, "invalid_params", "{}", error.1);
                assert!(stages(&real).is_empty());
            }

            #[test]
            fn missing_and_non_directory_destinations_have_their_own_codes() {
                let dir = TempDir::new("contract-dest");
                std::fs::write(dir.join("file"), b"").unwrap();

                let missing = $factory(dir.join("missing").to_str().unwrap())
                    .begin(&names(&["a"]))
                    .unwrap_err();
                let file = $factory(dir.join("file").to_str().unwrap())
                    .begin(&names(&["a"]))
                    .unwrap_err();

                assert_eq!(missing.0, "not_found");
                assert_eq!(file.0, "not_a_directory");
            }
        }
    };
}

pub(crate) use sink_contract;
