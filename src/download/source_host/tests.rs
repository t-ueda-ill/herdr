use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;
use crate::api::schema::PaneUploadEntryKind;
use crate::download::frame::{Limits, Packer};
use crate::upload::test_support::TempDir;

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index * 31 % 251) as u8).collect()
}

fn fast() -> HostOptions {
    HostOptions {
        tick: Duration::from_millis(5),
        ..HostOptions::default()
    }
}

fn source(paths: &[&Path], options: HostOptions) -> HostSource {
    HostSource::new(
        paths.iter().map(|path| path.to_path_buf()).collect(),
        options,
    )
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Every frame up to `End`, or the failure that ended the stream.
fn drain(source: &mut HostSource) -> Result<Vec<Frame>, Failure> {
    let control = Control::default();
    let mut frames = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match source.next(Duration::from_millis(50), &control)? {
            Some(Frame::End) => {
                frames.push(Frame::End);
                return Ok(frames);
            }
            Some(frame) => frames.push(frame),
            None => {}
        }
    }
    panic!("the stream did not end");
}

fn begun(paths: &[&Path], options: HostOptions) -> (HostSource, Began) {
    let mut source = source(paths, options);
    let began = source.begin().unwrap();
    (source, began)
}

fn stream(paths: &[&Path]) -> Vec<Frame> {
    let (mut source, _) = begun(paths, fast());
    drain(&mut source).unwrap()
}

fn rels(frames: &[Frame]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            Frame::Entry { rel, .. } => Some(String::from_utf8_lossy(rel).into_owned()),
            _ => None,
        })
        .collect()
}

fn warnings(frames: &[Frame]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            Frame::WalkWarning(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The bytes the stream carried for the file named `rel`.
fn body_of(frames: &[Frame], rel: &str) -> Vec<u8> {
    let mut body = Vec::new();
    let mut inside = false;
    for frame in frames {
        match frame {
            Frame::Entry { rel: name, .. } => inside = name == rel.as_bytes(),
            Frame::Data(bytes) if inside => body.extend_from_slice(bytes),
            _ => {}
        }
    }
    body
}

fn entry<'a>(frames: &'a [Frame], rel: &str) -> &'a PaneUploadEntryHeader {
    frames
        .iter()
        .find_map(|frame| match frame {
            Frame::Entry {
                rel: name, header, ..
            } if name == rel.as_bytes() => Some(header),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no entry {rel}"))
}

use crate::api::schema::PaneUploadEntryHeader;

fn mkfifo(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn a_tree_streams_parents_first_with_the_content_of_every_file() {
    let dir = TempDir::new("host-tree");
    let root = dir.join("t");
    std::fs::create_dir_all(root.join("b/c")).unwrap();
    std::fs::write(root.join("a.txt"), b"hello").unwrap();
    std::fs::write(root.join("b/c/d"), pattern(10)).unwrap();

    let frames = stream(&[&root]);

    assert_eq!(rels(&frames), ["t", "t/a.txt", "t/b", "t/b/c", "t/b/c/d"]);
    assert_eq!(body_of(&frames, "t/a.txt"), b"hello");
    assert_eq!(body_of(&frames, "t/b/c/d"), pattern(10));
    assert_eq!(frames.last(), Some(&Frame::End));
}

#[test]
fn file_bodies_of_awkward_sizes_arrive_byte_for_byte_in_bounded_frames() {
    let dir = TempDir::new("host-sizes");
    let root = dir.join("t");
    std::fs::create_dir(&root).unwrap();
    let sizes = [0usize, 1, 65_536, 65_537, 3 * 1024 * 1024];
    for size in sizes {
        std::fs::write(root.join(format!("f{size}")), pattern(size)).unwrap();
    }

    let frames = stream(&[&root]);

    for size in sizes {
        assert_eq!(
            body_of(&frames, &format!("t/f{size}")),
            pattern(size),
            "{size}"
        );
    }
    for frame in &frames {
        if let Frame::Data(bytes) = frame {
            assert!(bytes.len() <= READ_BYTES);
        }
    }
    let ends = frames
        .iter()
        .filter(|frame| matches!(frame, Frame::FileEnd(Ok(()))))
        .count();
    assert_eq!(ends, sizes.len());
}

#[test]
fn names_with_shell_characters_arrive_unchanged() {
    let dir = TempDir::new("host-names");
    let root = dir.join("t");
    std::fs::create_dir(&root).unwrap();
    let names = [
        "a b",
        "q'q",
        "d\"q",
        "$HOME",
        "back\\slash",
        "-dash",
        ".hidden",
        "日本語",
    ];
    for name in names {
        std::fs::write(root.join(name), name.as_bytes()).unwrap();
    }

    let frames = stream(&[&root]);

    for name in names {
        assert_eq!(
            body_of(&frames, &format!("t/{name}")),
            name.as_bytes(),
            "{name}"
        );
    }
}

#[test]
fn names_a_line_cannot_carry_are_left_out_with_a_warning() {
    let dir = TempDir::new("host-badnames");
    let root = dir.join("t");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("new\nline"), b"x").unwrap();
    let raw = std::ffi::OsStr::from_bytes(b"bad\xffname");
    std::fs::write(root.join(raw), b"x").unwrap();
    std::fs::write(root.join("fine"), b"x").unwrap();

    let frames = stream(&[&root]);

    assert_eq!(rels(&frames), ["t", "t/fine"]);
    assert_eq!(warnings(&frames).len(), 2);
}

#[test]
fn symlinks_inside_folders_stay_links_and_are_never_followed() {
    let dir = TempDir::new("host-links");
    let root = dir.join("t");
    std::fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink("../../etc", root.join("up")).unwrap();
    std::os::unix::fs::symlink("/etc", root.join("abs")).unwrap();

    let frames = stream(&[&root]);

    assert_eq!(rels(&frames), ["t", "t/abs", "t/up"]);
    let link = frames
        .iter()
        .find_map(|frame| match frame {
            Frame::Entry {
                rel,
                link_target,
                header,
            } if rel == b"t/up" => Some((header.kind, link_target.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        link,
        (PaneUploadEntryKind::Symlink, Some(b"../../etc".to_vec()))
    );
}

#[test]
fn a_symlink_chosen_directly_is_followed_and_its_target_content_saved_under_the_links_name() {
    let dir = TempDir::new("host-top-link");
    std::fs::write(dir.join("real.txt"), b"target body").unwrap();
    std::os::unix::fs::symlink(dir.join("real.txt"), dir.join("alias.txt")).unwrap();
    std::fs::create_dir_all(dir.join("realdir/sub")).unwrap();
    std::fs::write(dir.join("realdir/sub/x"), b"inner").unwrap();
    std::os::unix::fs::symlink("/etc", dir.join("realdir/inner-link")).unwrap();
    std::os::unix::fs::symlink(dir.join("realdir"), dir.join("aliasdir")).unwrap();

    let file_frames = stream(&[&dir.join("alias.txt")]);
    let dir_frames = stream(&[&dir.join("aliasdir")]);

    assert_eq!(rels(&file_frames), ["alias.txt"]);
    assert_eq!(
        entry(&file_frames, "alias.txt").kind,
        PaneUploadEntryKind::File
    );
    assert_eq!(body_of(&file_frames, "alias.txt"), b"target body");
    assert_eq!(
        rels(&dir_frames),
        [
            "aliasdir",
            "aliasdir/inner-link",
            "aliasdir/sub",
            "aliasdir/sub/x"
        ]
    );
    assert_eq!(
        entry(&dir_frames, "aliasdir/inner-link").kind,
        PaneUploadEntryKind::Symlink
    );
}

#[test]
fn a_selected_symlink_loop_or_a_broken_one_is_not_found_or_invalid() {
    let dir = TempDir::new("host-broken-link");
    std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("broken")).unwrap();
    std::os::unix::fs::symlink(dir.join("b"), dir.join("a")).unwrap();
    std::os::unix::fs::symlink(dir.join("a"), dir.join("b")).unwrap();

    let broken = source(&[&dir.join("broken")], fast()).begin().unwrap_err();
    let looped = source(&[&dir.join("a")], fast()).begin().unwrap_err();

    assert_eq!(broken.0, "not_found");
    assert_eq!(looped.0, "invalid_params");
}

#[test]
fn a_link_whose_target_has_a_newline_is_left_out_with_a_warning() {
    let dir = TempDir::new("host-newline-link");
    let root = dir.join("t");
    std::fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink("a\nb", root.join("odd")).unwrap();

    let frames = stream(&[&root]);

    assert_eq!(rels(&frames), ["t"]);
    assert_eq!(warnings(&frames).len(), 1);
}

#[test]
fn a_fifo_inside_a_folder_is_left_out_and_reported() {
    let dir = TempDir::new("host-fifo-inside");
    let root = dir.join("t");
    std::fs::create_dir(&root).unwrap();
    mkfifo(&root.join("pipe"));
    std::fs::write(root.join("f"), b"x").unwrap();

    let frames = stream(&[&root]);

    assert_eq!(rels(&frames), ["t", "t/f"]);
    assert_eq!(warnings(&frames).len(), 1);
}

#[test]
fn modes_and_modification_times_travel_with_each_entry() {
    let dir = TempDir::new("host-modes");
    let root = dir.join("t");
    std::fs::create_dir(&root).unwrap();
    for (name, mode) in [("run", 0o755), ("secret", 0o600)] {
        let path = root.join(name);
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        crate::platform::set_file_modified_for_test(
            &path,
            std::time::UNIX_EPOCH + Duration::from_secs(1_234_567),
        )
        .unwrap();
    }

    let frames = stream(&[&root]);

    assert_eq!(entry(&frames, "t/run").mode, 0o755);
    assert_eq!(entry(&frames, "t/secret").mode, 0o600);
    assert_eq!(entry(&frames, "t/run").mtime, Some(1_234_567));
    assert_eq!(entry(&frames, "t/run").size, Some(1));
}

#[test]
fn an_unreadable_file_below_the_top_is_skipped_not_fatal() {
    if is_root() {
        eprintln!("skipped: root reads everything");
        return;
    }
    let dir = TempDir::new("host-eacces-file");
    let root = dir.join("t");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("secret"), b"x").unwrap();
    std::fs::set_permissions(root.join("secret"), std::fs::Permissions::from_mode(0o0)).unwrap();
    std::fs::write(root.join("open"), b"y").unwrap();

    let frames = stream(&[&root]);

    assert!(frames.contains(&Frame::Skipped {
        rel: b"t/secret".to_vec(),
        reason: SkipReason::PermissionDenied,
    }));
    assert_eq!(body_of(&frames, "t/open"), b"y");
}

#[test]
fn an_unreadable_folder_below_the_top_is_sent_empty_with_a_warning() {
    if is_root() {
        eprintln!("skipped: root reads everything");
        return;
    }
    let dir = TempDir::new("host-eacces-dir");
    let root = dir.join("t");
    std::fs::create_dir_all(root.join("locked")).unwrap();
    std::fs::write(root.join("locked/inside"), b"x").unwrap();
    std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o0)).unwrap();
    std::fs::write(root.join("z"), b"y").unwrap();

    let frames = stream(&[&root]);

    assert_eq!(rels(&frames), ["t", "t/locked", "t/z"]);
    assert!(warnings(&frames)[0].contains("could not list"));
}

#[test]
fn a_missing_top_level_path_is_not_found() {
    let dir = TempDir::new("host-missing");

    let error = source(&[&dir.join("nope")], fast()).begin().unwrap_err();

    assert_eq!(error.0, "not_found");
    assert!(error.1.contains("nope"));
}

#[test]
fn an_unreadable_top_level_file_or_folder_is_permission_denied() {
    if is_root() {
        eprintln!("skipped: root reads everything");
        return;
    }
    let dir = TempDir::new("host-top-eacces");
    std::fs::write(dir.join("f"), b"x").unwrap();
    std::fs::set_permissions(dir.join("f"), std::fs::Permissions::from_mode(0o0)).unwrap();
    std::fs::create_dir(dir.join("d")).unwrap();
    std::fs::set_permissions(dir.join("d"), std::fs::Permissions::from_mode(0o0)).unwrap();

    let file = source(&[&dir.join("f")], fast()).begin().unwrap_err();
    let folder = source(&[&dir.join("d")], fast()).begin().unwrap_err();

    assert_eq!(file.0, "permission_denied");
    assert_eq!(folder.0, "permission_denied");
}

#[test]
fn a_top_level_fifo_or_non_utf8_name_is_refused_as_invalid() {
    let dir = TempDir::new("host-top-invalid");
    mkfifo(&dir.join("pipe"));
    let raw = dir.path().join(std::ffi::OsStr::from_bytes(b"bad\xffname"));
    std::fs::write(&raw, b"x").unwrap();

    let fifo = source(&[&dir.join("pipe")], fast()).begin().unwrap_err();
    let name = source(&[&raw], fast()).begin().unwrap_err();

    assert_eq!(fifo.0, "invalid_params");
    assert_eq!(name.0, "invalid_params");
}

#[test]
fn a_top_level_name_with_a_control_character_is_found_missing_from_the_walk_and_refused() {
    let dir = TempDir::new("host-top-newline");
    let odd = dir.join("new\nline");
    std::fs::write(&odd, b"x").unwrap();

    let error = source(&[&odd], fast()).begin().unwrap_err();

    assert_eq!(error.0, "invalid_params");
}

#[test]
fn duplicate_or_reserved_top_level_names_are_refused() {
    let dir = TempDir::new("host-top-dup");
    for parent in ["a", "b"] {
        std::fs::create_dir(dir.join(parent)).unwrap();
        std::fs::write(dir.join(parent).join("x"), b"").unwrap();
    }
    std::fs::write(dir.join(".herdr-upload-x"), b"").unwrap();

    let duplicate = source(&[&dir.join("a/x"), &dir.join("b/x")], fast())
        .begin()
        .unwrap_err();
    let reserved = source(&[&dir.join(".herdr-upload-x")], fast())
        .begin()
        .unwrap_err();

    assert_eq!(duplicate.0, "invalid_params");
    assert_eq!(reserved.0, "invalid_params");
}

#[test]
fn a_selection_over_the_byte_or_entry_limit_is_too_large_before_any_byte_moves() {
    let dir = TempDir::new("host-limits");
    let root = dir.join("t");
    std::fs::create_dir(&root).unwrap();
    for name in ["a", "b", "c"] {
        std::fs::write(root.join(name), pattern(100)).unwrap();
    }

    let bytes = source(
        &[&root],
        HostOptions {
            max_bytes: 299,
            ..fast()
        },
    )
    .begin()
    .unwrap_err();
    let entries = source(
        &[&root],
        HostOptions {
            max_entries: 3,
            ..fast()
        },
    )
    .begin()
    .unwrap_err();
    let exact = source(
        &[&root],
        HostOptions {
            max_bytes: 300,
            max_entries: 4,
            ..fast()
        },
    )
    .begin()
    .unwrap();

    assert_eq!(bytes.0, "download_too_large");
    assert_eq!(entries.0, "download_too_large");
    assert!(!bytes.1.to_lowercase().contains("upload"));
    assert_eq!(
        exact,
        Began {
            entries: 4,
            bytes: 300
        }
    );
}

#[test]
fn begin_estimates_are_exact_for_a_host_tree() {
    let dir = TempDir::new("host-estimate");
    let root = dir.join("t");
    std::fs::create_dir_all(root.join("d")).unwrap();
    std::fs::write(root.join("d/a"), pattern(7)).unwrap();
    std::fs::write(root.join("b"), pattern(5)).unwrap();

    let (_, began) = begun(&[&root], fast());

    assert_eq!(
        began,
        Began {
            entries: 4,
            bytes: 12
        }
    );
}

/// A source whose first folder entry parks the thread on the one-frame queue,
/// so a test can change the tree before the thread reaches the file.
fn parked_tree(label: &str, content: &[u8]) -> (TempDir, PathBuf) {
    let dir = TempDir::new(label);
    let file = dir.join("t/a/f");
    std::fs::create_dir_all(dir.join("t/a")).unwrap();
    std::fs::write(&file, content).unwrap();
    (dir, file)
}

fn parked(dir: &TempDir, options: HostOptions) -> HostSource {
    let (source, _) = begun(
        &[&dir.join("t")],
        HostOptions {
            queue: 1,
            ..options
        },
    );
    std::thread::sleep(Duration::from_millis(100));
    source
}

#[test]
fn a_file_swapped_for_a_symlink_after_the_scan_is_not_followed() {
    let (dir, file) = parked_tree("host-swap-link", b"private");
    std::fs::write(dir.join("secret"), b"do not read").unwrap();
    let mut source = parked(&dir, fast());
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(dir.join("secret"), &file).unwrap();

    let error = drain(&mut source).unwrap_err();

    assert_eq!(error.0, "download_source_changed");
}

#[test]
fn a_file_swapped_for_a_fifo_after_the_scan_fails_instead_of_blocking() {
    let (dir, file) = parked_tree("host-swap-fifo", b"x");
    let mut source = parked(&dir, fast());
    std::fs::remove_file(&file).unwrap();
    mkfifo(&file);
    let started = Instant::now();

    let error = drain(&mut source).unwrap_err();

    assert_eq!(error.0, "download_source_changed");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_file_deleted_after_the_scan_fails_the_stream_as_changed() {
    let (dir, file) = parked_tree("host-deleted", b"x");
    let mut source = parked(&dir, fast());
    std::fs::remove_file(&file).unwrap();

    assert_eq!(drain(&mut source).unwrap_err().0, "download_source_changed");
}

#[test]
fn a_file_replaced_by_another_of_the_same_size_and_time_is_caught_by_its_identity() {
    let (dir, file) = parked_tree("host-identity", b"same");
    let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
    let mut source = parked(&dir, fast());
    // Why keep the old file open: a filesystem may hand a freed inode number to
    // the very next file created, which would make the replacement look like
    // the original.
    let old = std::fs::File::open(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    std::fs::write(&file, b"diff").unwrap();
    crate::platform::set_file_modified_for_test(&file, mtime).unwrap();

    assert_eq!(drain(&mut source).unwrap_err().0, "download_source_changed");
    drop(old);
}

#[test]
fn a_file_rewritten_in_place_with_a_new_time_before_the_stream_reaches_it_is_caught() {
    let (dir, file) = parked_tree("host-rewritten", b"same");
    let mut source = parked(&dir, fast());
    std::fs::write(&file, b"diff").unwrap();
    crate::platform::set_file_modified_for_test(
        &file,
        std::time::UNIX_EPOCH + Duration::from_secs(42),
    )
    .unwrap();

    assert_eq!(drain(&mut source).unwrap_err().0, "download_source_changed");
}

/// Streams a 200 KB file `t/f` and a small `t/g` after it, and runs `change`
/// once, before the first read of `t/f`.
fn changed_during_read(label: &str, change: impl Fn(&Path) + Send + Sync + 'static) -> Vec<Frame> {
    let dir = TempDir::new(label);
    let file = dir.join("t/f");
    std::fs::create_dir_all(dir.join("t")).unwrap();
    std::fs::write(&file, pattern(200_000)).unwrap();
    std::fs::write(dir.join("t/g"), b"after").unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let hook: Arc<dyn Fn(&Path) + Send + Sync> = Arc::new(move |path| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            change(path);
        }
    });
    let options = HostOptions {
        before_read: Some(hook),
        ..fast()
    };
    let (mut source, _) = begun(&[&dir.join("t")], options);
    drain(&mut source).unwrap()
}

fn file_end(frames: &[Frame]) -> &Result<(), Changed> {
    frames
        .iter()
        .find_map(|frame| match frame {
            Frame::FileEnd(result) => Some(result),
            _ => None,
        })
        .expect("no file end")
}

#[test]
fn a_file_that_shrinks_mid_stream_ends_with_a_changed_trailer() {
    let frames = changed_during_read("host-shrink", |path| {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(1_000)
            .unwrap();
    });

    assert!(file_end(&frames).is_err());
}

#[test]
fn a_file_that_grows_mid_stream_ends_with_a_changed_trailer() {
    let frames = changed_during_read("host-grow", |path| {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"more")
            .unwrap();
    });

    assert!(file_end(&frames).is_err());
}

#[test]
fn a_rewrite_that_keeps_the_size_but_changes_the_time_ends_with_a_changed_trailer() {
    let frames = changed_during_read("host-rewrite", |path| {
        std::fs::write(path, pattern(200_000)).unwrap();
        crate::platform::set_file_modified_for_test(
            path,
            std::time::UNIX_EPOCH + Duration::from_secs(99),
        )
        .unwrap();
    });

    assert!(file_end(&frames).is_err());
}

#[test]
fn a_changed_trailer_leaves_the_verdict_to_the_packer_and_the_stream_goes_on_to_its_end() {
    let frames = changed_during_read("host-changed-goes-on", |path| {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(1_000)
            .unwrap();
    });

    assert!(file_end(&frames).is_err());
    assert_eq!(body_of(&frames, "t/g"), b"after");
    assert_eq!(frames.last(), Some(&Frame::End));
}

#[test]
fn an_untouched_file_ends_with_an_unchanged_trailer() {
    let frames = changed_during_read("host-untouched", |_| {});

    assert_eq!(file_end(&frames), &Ok(()));
}

#[test]
fn a_filesystem_that_stops_answering_fails_the_stream_as_stalled() {
    let dir = TempDir::new("host-stall");
    std::fs::create_dir(dir.join("t")).unwrap();
    std::fs::write(dir.join("t/f"), b"x").unwrap();
    let hook: Arc<dyn Fn(&Path) + Send + Sync> =
        Arc::new(|_| std::thread::sleep(Duration::from_millis(600)));
    let options = HostOptions {
        stall: Duration::from_millis(100),
        before_read: Some(hook),
        ..fast()
    };
    let (mut source, _) = begun(&[&dir.join("t")], options);
    let started = Instant::now();

    let error = drain(&mut source).unwrap_err();

    assert_eq!(
        error,
        ("download_failed", "the source stopped sending".into())
    );
    assert!(started.elapsed() < Duration::from_millis(550));
}

#[test]
fn a_stuck_read_does_not_stop_the_caller_from_cancelling() {
    let dir = TempDir::new("host-stuck-cancel");
    std::fs::create_dir(dir.join("t")).unwrap();
    std::fs::write(dir.join("t/f"), b"x").unwrap();
    let hook: Arc<dyn Fn(&Path) + Send + Sync> =
        Arc::new(|_| std::thread::sleep(Duration::from_millis(800)));
    let (mut source, _) = begun(
        &[&dir.join("t")],
        HostOptions {
            before_read: Some(hook),
            ..fast()
        },
    );
    let control = Arc::new(Control::default());
    let flag = Arc::clone(&control);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        flag.cancel();
    });
    let started = Instant::now();

    let result = loop {
        match source.next(Duration::from_secs(10), &control) {
            Ok(Some(_)) => {}
            other => break other,
        }
    };

    assert_eq!(result, Err(cancelled()));
    assert!(started.elapsed() < Duration::from_millis(600));
}

#[test]
fn aborting_ends_the_reading_thread_and_later_reads_report_a_cancel() {
    let dir = TempDir::new("host-abort");
    std::fs::create_dir(dir.join("t")).unwrap();
    std::fs::write(dir.join("t/big"), pattern(3 * 1024 * 1024)).unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let hook: Arc<dyn Fn(&Path) + Send + Sync> = Arc::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let (mut source, _) = begun(
        &[&dir.join("t")],
        HostOptions {
            queue: 1,
            before_read: Some(hook),
            ..fast()
        },
    );
    std::thread::sleep(Duration::from_millis(100));

    source.abort();
    std::thread::sleep(Duration::from_millis(100));
    let after_abort = reads.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));

    assert_eq!(reads.load(Ordering::SeqCst), after_abort);
    assert!(
        after_abort < 48,
        "a 3 MiB file is 48 reads; the thread must stop early"
    );
    assert_eq!(
        source.next(Duration::ZERO, &Control::default()),
        Err(cancelled())
    );
}

#[test]
fn a_source_cannot_be_read_before_begin() {
    let mut source = source(&[Path::new("/tmp")], fast());

    assert_eq!(
        source
            .next(Duration::ZERO, &Control::default())
            .unwrap_err()
            .0,
        "download_failed"
    );
}

#[test]
fn the_host_stream_packs_into_records_a_client_could_publish() {
    let dir = TempDir::new("host-packer");
    let root = dir.join("t");
    std::fs::create_dir_all(root.join("d")).unwrap();
    std::fs::write(root.join("d/x"), pattern(150_000)).unwrap();
    std::fs::write(root.join("empty"), b"").unwrap();
    std::os::unix::fs::symlink("d/x", root.join("link")).unwrap();
    mkfifo(&root.join("pipe"));
    let (mut source, _) = begun(&[&root], fast());
    let control = Control::default();
    let mut packer = Packer::new(vec!["t".into()], Limits::new(64 * 1024));

    let mut records = Vec::new();
    let mut seq = 0;
    let (done, warnings) = loop {
        let mut next = || source.next(Duration::from_millis(200), &control);
        let chunk = packer.fill(seq, &mut next).unwrap();
        seq += 1;
        records.extend(chunk.records);
        if chunk.done {
            break (true, chunk.warnings);
        }
    };

    assert!(done);
    assert_eq!(warnings.len(), 1);
    let paths: Vec<_> = records
        .iter()
        .filter(|record| record.entry.is_some())
        .map(|record| record.path.as_str())
        .collect();
    assert_eq!(paths, ["t", "t/d", "t/d/x", "t/empty", "t/link"]);
    let total: usize = records
        .iter()
        .filter(|record| record.path == "t/d/x")
        .map(|record| record.data.len())
        .sum();
    assert!(total > 0);
}
