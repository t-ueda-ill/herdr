//! Runs the real scripts with every POSIX shell on the machine and reads what
//! they print, without the reader thread or its queue in between.

use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;
use crate::offhost::launcher::{LocalShellLauncher, OffhostLauncher};
use crate::upload::test_support::{posix_shells, TempDir};

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index * 31 % 251) as u8).collect()
}

/// `shell` run through `env`, so the script sees `vars` and the PATH a test chose.
fn with_env(shell: &[String], vars: &[(&str, String)]) -> Vec<String> {
    let mut argv = vec!["/usr/bin/env".to_owned()];
    argv.extend(vars.iter().map(|(key, value)| format!("{key}={value}")));
    argv.extend(shell.iter().cloned());
    argv
}

fn launcher(shell: Vec<String>) -> LocalShellLauncher {
    LocalShellLauncher::new(shell, SENDER_SCRIPT, "download_failed")
}

fn script_args<'a>(
    limits: (u64, u64),
    paths: &'a [String],
    store: &'a mut Vec<String>,
) -> Vec<&'a str> {
    store.clear();
    store.push(limits.0.to_string());
    store.push(limits.1.to_string());
    let mut args = vec![EMIT_SCRIPT];
    args.extend(store.iter().map(String::as_str));
    args.extend(paths.iter().map(String::as_str));
    args
}

#[derive(Default)]
struct Outcome {
    estimate: Option<(u64, u64)>,
    frames: Vec<Frame>,
    failure: Option<Failure>,
}

impl Outcome {
    fn rels(&self) -> Vec<Vec<u8>> {
        self.frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Entry { rel, .. } => Some(rel.clone()),
                _ => None,
            })
            .collect()
    }

    fn entry(&self, rel: &str) -> (&PaneUploadEntryHeader, Option<&Vec<u8>>) {
        self.frames
            .iter()
            .find_map(|frame| match frame {
                Frame::Entry {
                    rel: found,
                    header,
                    link_target,
                } if found == rel.as_bytes() => Some((header, link_target.as_ref())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no entry {rel}: {:?}", self.rels()))
    }

    fn skipped(&self) -> Vec<(String, SkipReason)> {
        self.frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Skipped { rel, reason } => {
                    Some((String::from_utf8_lossy(rel).into_owned(), *reason))
                }
                _ => None,
            })
            .collect()
    }

    fn warnings(&self) -> Vec<String> {
        self.frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::WalkWarning(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// The bytes and trailer of the file `rel`, in stream order.
    fn file(&self, rel: &str) -> (Vec<u8>, Result<(), Changed>) {
        let mut at = None;
        let mut body = Vec::new();
        for (index, frame) in self.frames.iter().enumerate() {
            match frame {
                Frame::Entry { rel: found, .. } if found == rel.as_bytes() => at = Some(index),
                Frame::Data(bytes) if at.is_some() => body.extend_from_slice(bytes),
                Frame::FileEnd(result) if at.is_some() => return (body, result.clone()),
                _ => {}
            }
        }
        panic!("no file {rel}: {:?}", self.rels());
    }
}

struct Spawned {
    child: crate::noninteractive_process::StreamingChild,
    parser: SenderParser,
}

fn spawn(shell: &[String], limits: (u64, u64), paths: &[&Path]) -> Spawned {
    let paths: Vec<String> = paths
        .iter()
        .map(|path| path.to_str().unwrap().to_owned())
        .collect();
    let mut store = Vec::new();
    let args = script_args(limits, &paths, &mut store);
    let child = launcher(shell.to_vec()).launch(&args).unwrap();
    Spawned {
        child,
        parser: SenderParser::default(),
    }
}

impl Spawned {
    /// Reads until the output ends, a failure, or the deadline.
    fn read_to_end(&mut self, outcome: &mut Outcome) {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut buffer = vec![0u8; 64 * 1024];
        while Instant::now() < deadline {
            let read = match self.child.stdout.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            if self.take(&buffer[..read], outcome) {
                return;
            }
        }
        panic!("the sender did not finish");
    }

    /// `true`: stop reading (a failure).
    fn take(&mut self, bytes: &[u8], outcome: &mut Outcome) -> bool {
        match self.parser.feed(bytes) {
            Ok(events) => {
                for event in events {
                    match event {
                        Event::Estimate { entries, bytes } => {
                            outcome.estimate = Some((entries, bytes));
                        }
                        Event::Frame(frame) => outcome.frames.push(frame),
                    }
                }
                false
            }
            Err(failure) => {
                outcome.failure = Some(failure);
                true
            }
        }
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.child.child.kill();
        let _ = self.child.child.wait();
    }
}

const BIG: (u64, u64) = (100_000, 16 * 1024 * 1024 * 1024);

fn download_with(shell: &[String], limits: (u64, u64), paths: &[&Path]) -> Outcome {
    let mut spawned = spawn(shell, limits, paths);
    // Why ignore a failed write: a helper that refused the walk has already exited.
    let _ = spawned.child.stdin.write_all(b"G\n");
    let mut outcome = Outcome::default();
    spawned.read_to_end(&mut outcome);
    outcome
}

fn download(shell: &[String], paths: &[&Path]) -> Outcome {
    download_with(shell, BIG, paths)
}

fn each_shell(check: impl Fn(&[String])) {
    for shell in posix_shells() {
        eprintln!("shell: {shell:?}");
        check(&shell);
    }
}

/// Like [`each_shell`], without the shell from `HERDR_TEST_EXTRA_SHELL`: a
/// test that sets the helper's environment or PATH through `env` cannot reach
/// into a container that shell starts.
fn each_local_shell(check: impl Fn(&[String])) {
    let extra: Vec<String> = std::env::var("HERDR_TEST_EXTRA_SHELL")
        .map(|text| text.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    for shell in posix_shells() {
        if shell == extra {
            eprintln!("skipping {shell:?}: this test changes the helper's environment");
            continue;
        }
        eprintln!("shell: {shell:?}");
        check(&shell);
    }
}

fn tree(label: &str) -> TempDir {
    TempDir::new(label)
}

#[test]
fn both_scripts_are_valid_posix_sh_syntax() {
    let dir = tree("syntax");
    for (name, script) in [("sender", SENDER_SCRIPT), ("emit", EMIT_SCRIPT)] {
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        for shell in posix_shells() {
            let mut argv = shell[1..].to_vec();
            argv.extend(["-n".to_owned(), path.to_str().unwrap().to_owned()]);
            let output = std::process::Command::new(&shell[0])
                .args(&argv)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{shell:?} rejects the {name} script: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn file_bodies_of_every_awkward_size_arrive_byte_for_byte() {
    each_shell(|shell| {
        let dir = tree("sizes");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        let sizes = [0usize, 1, 65_535, 65_536, 65_537, 3 * 1024 * 1024];
        for size in sizes {
            std::fs::write(root.join(format!("f{size}")), pattern(size)).unwrap();
        }

        let outcome = download(shell, &[&root]);

        assert_eq!(outcome.failure, None);
        for size in sizes {
            let (body, trailer) = outcome.file(&format!("t/f{size}"));
            assert!(body == pattern(size), "{size} bytes differ");
            assert_eq!(trailer, Ok(()), "{size}");
        }
        assert_eq!(
            outcome.estimate,
            Some((7, sizes.iter().sum::<usize>() as u64))
        );
        assert!(matches!(outcome.frames.last(), Some(Frame::End)));
    });
}

#[test]
fn names_with_newlines_quotes_spaces_dollars_and_leading_dashes_survive() {
    each_shell(|shell| {
        let dir = tree("names");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        let names: Vec<Vec<u8>> = vec![
            b"two\nlines".to_vec(),
            b"with space".to_vec(),
            b"it's".to_vec(),
            b"say \"hi\"".to_vec(),
            b"$HOME".to_vec(),
            b"back\\slash".to_vec(),
            b"-dash".to_vec(),
            b".hidden".to_vec(),
            b"bad-\xff-utf8".to_vec(),
            b"*".to_vec(),
        ];
        for name in &names {
            std::fs::write(root.join(std::ffi::OsStr::from_bytes(name)), name).unwrap();
        }

        let outcome = download(shell, &[&root]);

        assert_eq!(outcome.failure, None);
        for name in &names {
            let mut rel = b"t/".to_vec();
            rel.extend_from_slice(name);
            let found = outcome.frames.iter().position(
                |frame| matches!(frame, Frame::Entry { rel: found, .. } if *found == rel),
            );
            let at =
                found.unwrap_or_else(|| panic!("{:?} is missing", String::from_utf8_lossy(name)));
            let Frame::Data(body) = &outcome.frames[at + 1] else {
                panic!("no data after {:?}", String::from_utf8_lossy(name));
            };
            assert_eq!(body, name);
        }
    });
}

#[test]
fn nested_folders_and_links_arrive_in_parent_first_order_and_links_are_not_followed() {
    each_shell(|shell| {
        let dir = tree("nested");
        let root = dir.join("t");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/f"), b"x").unwrap();
        std::os::unix::fs::symlink("../../etc", root.join("a/escape")).unwrap();
        std::os::unix::fs::symlink("b", root.join("a/inner")).unwrap();

        let outcome = download(shell, &[&root]);

        assert_eq!(outcome.failure, None);
        let rels: Vec<String> = outcome
            .rels()
            .into_iter()
            .map(|rel| String::from_utf8(rel).unwrap())
            .collect();
        let position = |rel: &str| rels.iter().position(|found| found == rel).unwrap();
        assert!(position("t") < position("t/a"));
        assert!(position("t/a") < position("t/a/b"));
        assert!(position("t/a/b") < position("t/a/b/f"));
        let (header, target) = outcome.entry("t/a/escape");
        assert_eq!(header.kind, PaneUploadEntryKind::Symlink);
        assert_eq!(target.map(Vec::as_slice), Some(b"../../etc".as_slice()));
        assert!(!rels.iter().any(|rel| rel.starts_with("t/a/escape/")));
        assert!(!rels.iter().any(|rel| rel.starts_with("t/a/inner/")));
    });
}

#[test]
fn a_selected_link_is_followed_and_keeps_its_own_name() {
    each_shell(|shell| {
        let dir = tree("top-link");
        std::fs::write(dir.join("real.txt"), b"content").unwrap();
        std::os::unix::fs::symlink(dir.join("real.txt"), dir.join("alias")).unwrap();
        std::fs::create_dir_all(dir.join("realdir/sub")).unwrap();
        std::fs::write(dir.join("realdir/sub/g"), b"deep").unwrap();
        std::os::unix::fs::symlink("sub", dir.join("realdir/inner")).unwrap();
        std::os::unix::fs::symlink(dir.join("realdir"), dir.join("dirlink")).unwrap();

        let file = download(shell, &[&dir.join("alias")]);
        let folder = download(shell, &[&dir.join("dirlink")]);

        assert_eq!(file.failure, None);
        let (header, _) = file.entry("alias");
        assert_eq!(header.kind, PaneUploadEntryKind::File);
        assert_eq!(header.size, Some(7));
        assert_eq!(file.file("alias"), (b"content".to_vec(), Ok(())));
        assert_eq!(folder.failure, None);
        assert_eq!(
            folder.entry("dirlink").0.kind,
            PaneUploadEntryKind::Directory
        );
        assert_eq!(folder.file("dirlink/sub/g").0, b"deep");
        let (inner, target) = folder.entry("dirlink/inner");
        assert_eq!(inner.kind, PaneUploadEntryKind::Symlink);
        assert_eq!(target.map(Vec::as_slice), Some(b"sub".as_slice()));
    });
}

#[test]
fn a_selected_link_to_nothing_is_missing() {
    each_shell(|shell| {
        let dir = tree("broken-link");
        std::os::unix::fs::symlink(dir.join("gone"), dir.join("alias")).unwrap();

        let outcome = download(shell, &[&dir.join("alias")]);

        assert_eq!(outcome.failure.map(|failure| failure.0), Some("not_found"));
    });
}

#[test]
fn a_link_whose_target_ends_in_a_newline_keeps_the_newline_so_the_server_can_skip_it() {
    each_shell(|shell| {
        let dir = tree("nl-link");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink("x\n", root.join("ln")).unwrap();

        let outcome = download(shell, &[&root]);

        let (_, target) = outcome.entry("t/ln");
        assert_eq!(target.map(Vec::as_slice), Some(b"x\n".as_slice()));
    });
}

#[test]
fn a_fifo_is_left_out_and_a_selected_fifo_is_refused() {
    each_shell(|shell| {
        let dir = tree("fifo");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(root.join("pipe"))
            .status()
            .unwrap();
        assert!(made.success());

        let inside = download(shell, &[&root]);
        let selected = download(shell, &[&root.join("pipe")]);

        assert_eq!(inside.failure, None);
        assert_eq!(
            inside.skipped(),
            [("t/pipe".to_owned(), SkipReason::Special)]
        );
        assert_eq!(
            selected.failure.map(|failure| failure.0),
            Some("invalid_params")
        );
    });
}

#[test]
fn an_unreadable_file_is_skipped_and_an_unreadable_folder_is_a_warning() {
    if is_root() {
        eprintln!("skipping: root reads everything");
        return;
    }
    each_shell(|shell| {
        let dir = tree("unreadable");
        let root = dir.join("t");
        std::fs::create_dir_all(root.join("locked")).unwrap();
        std::fs::write(root.join("locked/inside"), b"x").unwrap();
        std::fs::write(root.join("secret"), b"x").unwrap();
        std::fs::write(root.join("ok"), b"fine").unwrap();
        std::fs::set_permissions(root.join("secret"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();

        let outcome = download(shell, &[&root]);

        assert_eq!(outcome.failure, None);
        assert_eq!(
            outcome.skipped(),
            [("t/secret".to_owned(), SkipReason::PermissionDenied)]
        );
        assert_eq!(outcome.warnings().len(), 1, "{:?}", outcome.warnings());
        assert!(outcome.rels().contains(&b"t/locked".to_vec()));
        assert!(!outcome.rels().contains(&b"t/locked/inside".to_vec()));
        assert_eq!(outcome.file("t/ok").0, b"fine");
    });
}

#[test]
fn modes_come_from_the_listing() {
    each_shell(|shell| {
        let dir = tree("modes");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("exec"), b"x").unwrap();
        std::fs::write(root.join("private"), b"x").unwrap();
        std::fs::write(root.join("sticky"), b"x").unwrap();
        std::fs::set_permissions(root.join("exec"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::set_permissions(root.join("private"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        std::fs::set_permissions(root.join("sticky"), std::fs::Permissions::from_mode(0o4750))
            .unwrap();

        let outcome = download(shell, &[&root]);

        assert_eq!(outcome.entry("t/exec").0.mode, 0o755);
        assert_eq!(outcome.entry("t/private").0.mode, 0o600);
        assert_eq!(outcome.entry("t/sticky").0.mode, 0o750);
    });
}

#[test]
fn the_modification_time_is_the_files_and_a_missing_stat_leaves_it_unknown() {
    each_local_shell(|shell| {
        let dir = tree("mtime");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        let file = root.join("f");
        std::fs::write(&file, b"x").unwrap();
        let stamp = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        // A PATH with the same tools but no stat.
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        for tool in [
            "ls", "find", "awk", "cat", "head", "readlink", "sh", "env", "dash", "bash",
        ] {
            let found = std::process::Command::new("sh")
                .args(["-c", &format!("command -v {tool}")])
                .output()
                .unwrap();
            let path = String::from_utf8_lossy(&found.stdout).trim().to_owned();
            if !path.is_empty() {
                let _ = std::os::unix::fs::symlink(path, bin.join(tool));
            }
        }

        let with_stat = download(shell, &[&root]);
        let shell_without_stat = with_env(shell, &[("PATH", bin.to_str().unwrap().to_owned())]);
        let without_stat = download(&shell_without_stat, &[&root]);

        assert_eq!(with_stat.entry("t/f").0.mtime, Some(1_600_000_000));
        assert_eq!(without_stat.failure, None);
        assert_eq!(without_stat.entry("t/f").0.mtime, None);
        assert_eq!(without_stat.file("t/f"), (b"x".to_vec(), Ok(())));
    });
}

#[test]
fn listing_style_variables_do_not_change_the_reported_sizes() {
    each_local_shell(|shell| {
        let dir = tree("blocksize");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("f"), pattern(1500)).unwrap();
        let shell = with_env(
            shell,
            &[
                ("BLOCK_SIZE", "1K".to_owned()),
                ("LS_BLOCK_SIZE", "human-readable".to_owned()),
                ("TIME_STYLE", "long-iso".to_owned()),
            ],
        );

        let outcome = download(&shell, &[&root]);

        assert_eq!(outcome.failure, None);
        assert_eq!(outcome.entry("t/f").0.size, Some(1500));
        assert_eq!(outcome.file("t/f").0, pattern(1500));
        assert_eq!(outcome.estimate, Some((2, 1500)));
    });
}

#[test]
fn a_missing_selection_and_one_over_the_limits_are_refused_before_any_byte_moves() {
    each_shell(|shell| {
        let dir = tree("limits");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a"), pattern(100)).unwrap();
        std::fs::write(root.join("b"), pattern(100)).unwrap();

        let missing = download(shell, &[&dir.join("nope")]);
        let too_many = download_with(shell, (2, BIG.1), &[&root]);
        let too_big = download_with(shell, (BIG.0, 150), &[&root]);
        let exact = download_with(shell, (3, 200), &[&root]);

        assert_eq!(missing.failure.map(|failure| failure.0), Some("not_found"));
        assert_eq!(
            too_many.failure.map(|failure| failure.0),
            Some("download_too_large")
        );
        assert_eq!(
            too_big.failure.map(|failure| failure.0),
            Some("download_too_large")
        );
        assert_eq!(exact.failure, None);
        assert_eq!(exact.estimate, Some((3, 200)));
        assert!(too_big.frames.is_empty() && too_big.estimate.is_none());
    });
}

#[test]
fn several_selections_are_streamed_in_the_order_given() {
    each_shell(|shell| {
        let dir = tree("several");
        std::fs::write(dir.join("one"), b"1").unwrap();
        std::fs::create_dir(dir.join("two")).unwrap();
        std::fs::write(dir.join("two/inner"), b"2").unwrap();

        let outcome = download(shell, &[&dir.join("two"), &dir.join("one")]);

        assert_eq!(outcome.failure, None);
        assert_eq!(
            outcome.rels(),
            [b"two".to_vec(), b"two/inner".to_vec(), b"one".to_vec()]
        );
    });
}

#[test]
fn a_selection_that_vanishes_before_the_walk_is_reported_as_changed() {
    each_local_shell(|shell| {
        let dir = tree("vanish");
        let victim = dir.join("t");
        std::fs::create_dir(&victim).unwrap();
        std::fs::write(victim.join("f"), b"x").unwrap();
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let real_find = String::from_utf8(
            std::process::Command::new("sh")
                .args(["-c", "command -v find"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        write_tool(
            &bin,
            "find",
            &format!("rm -rf \"$VICTIM\"\nexec {} \"$@\"", real_find.trim()),
        );
        let shell = with_env(
            shell,
            &[
                (
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                ),
                ("VICTIM", victim.to_str().unwrap().to_owned()),
            ],
        );

        let outcome = download(&shell, &[&victim]);

        assert_eq!(
            outcome.failure.map(|failure| failure.0),
            Some("download_source_changed")
        );
    });
}

fn write_tool(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn closing_stdin_before_go_ends_the_helper_without_streaming() {
    each_shell(|shell| {
        let dir = tree("no-go");
        std::fs::write(dir.join("f"), b"x").unwrap();
        let mut spawned = spawn(shell, BIG, &[&dir.join("f")]);
        let mut outcome = Outcome::default();

        drop(std::mem::replace(
            &mut spawned.child.stdin,
            // A throwaway pipe: the real stdin must close.
            std::process::Command::new("cat")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
                .stdin
                .take()
                .unwrap(),
        ));
        spawned.read_to_end(&mut outcome);

        assert_eq!(outcome.estimate, Some((1, 1)));
        assert!(outcome.frames.is_empty());
        assert_eq!(outcome.failure, None);
    });
}

#[test]
fn go_without_its_newline_does_not_start_the_stream() {
    each_shell(|shell| {
        let dir = tree("no-newline");
        std::fs::write(dir.join("f"), b"x").unwrap();
        let paths = vec![dir.join("f").to_str().unwrap().to_owned()];
        let mut store = Vec::new();
        let args = script_args(BIG, &paths, &mut store);
        let mut child = launcher(shell.to_vec()).launch(&args).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut stdout = child.stdout;
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = stdout.read(&mut buffer) {
                if read == 0 || sender.send(buffer[..read].to_vec()).is_err() {
                    return;
                }
            }
        });
        child.stdin.write_all(b"G").unwrap();
        child.stdin.flush().unwrap();
        let mut parser = SenderParser::default();
        let mut events = Vec::new();
        while !events
            .iter()
            .any(|event| matches!(event, Event::Estimate { .. }))
        {
            let bytes = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
            events.extend(parser.feed(&bytes).unwrap());
        }

        assert!(receiver.recv_timeout(Duration::from_millis(500)).is_err());

        child.stdin.write_all(b"\n").unwrap();
        let bytes = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        let more = parser.feed(&bytes).unwrap();
        assert!(!more.is_empty());
        let _ = child.child.kill();
        let _ = child.child.wait();
    });
}

/// Streams `rel` up to its header and returns how many body bytes the helper
/// has already put in the pipe, so a test can change the file at that point.
fn changing_file(shell: &[String], size: usize, change: impl FnOnce(&Path)) -> Outcome {
    let dir = tree("changing");
    let file = dir.join("big");
    std::fs::write(&file, pattern(size)).unwrap();
    let mut spawned = spawn(shell, BIG, &[&file]);
    spawned.child.stdin.write_all(b"G\n").unwrap();
    let mut outcome = Outcome::default();
    let mut buffer = vec![0u8; 4096];
    // Why stop at the header: with nobody reading, the pipe fills after 64 KiB
    // and `cat` waits, so the rest of the file has not been read yet.
    while !outcome
        .frames
        .iter()
        .any(|frame| matches!(frame, Frame::Entry { .. }))
    {
        let read = spawned.child.stdout.read(&mut buffer).unwrap();
        assert!(read > 0, "the helper ended early: {:?}", outcome.failure);
        assert!(!spawned.take(&buffer[..read], &mut outcome));
    }
    change(&file);
    spawned.read_to_end(&mut outcome);
    outcome
}

#[test]
fn a_file_that_shrinks_mid_stream_still_delivers_its_declared_size_and_a_changed_trailer() {
    each_shell(|shell| {
        let size = 16 * 1024 * 1024;
        let outcome = changing_file(shell, size, |file| {
            std::fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_len(1024 * 1024)
                .unwrap();
        });

        let (body, trailer) = outcome.file("big");
        assert_eq!(body.len(), size);
        assert!(body[..1024 * 1024] == pattern(size)[..1024 * 1024]);
        assert!(body[1024 * 1024..].iter().all(|byte| *byte == 0));
        assert!(trailer.is_err());
    });
}

#[test]
fn a_file_that_grows_mid_stream_still_delivers_its_declared_size_and_a_changed_trailer() {
    each_shell(|shell| {
        let size = 16 * 1024 * 1024;
        let outcome = changing_file(shell, size, |file| {
            let mut open = std::fs::File::options().append(true).open(file).unwrap();
            open.write_all(&vec![7u8; 1024 * 1024]).unwrap();
        });

        let (body, trailer) = outcome.file("big");
        assert!(body == pattern(size));
        assert!(trailer.is_err());
    });
}

#[test]
fn a_read_that_fails_halfway_is_a_changed_trailer_even_when_size_and_time_are_unchanged() {
    each_local_shell(|shell| {
        let dir = tree("failing-cat");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("f"), pattern(100_000)).unwrap();
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let real_cat = String::from_utf8(
            std::process::Command::new("sh")
                .args(["-c", "command -v cat"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        // Writes half of the file named after `--` and fails; anything else is a real cat.
        write_tool(
            &bin,
            "cat",
            &format!(
                "if [ \"$1\" = -- ]; then {real} -- \"$2\" | head -c 50000; exit 1; fi\nexec {real} \"$@\"",
                real = real_cat.trim()
            ),
        );
        let shell = with_env(
            shell,
            &[(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )],
        );

        let outcome = download(&shell, &[&root]);

        assert_eq!(outcome.failure, None);
        let (body, trailer) = outcome.file("t/f");
        assert_eq!(body.len(), 100_000);
        assert!(body[..50_000] == pattern(100_000)[..50_000]);
        assert!(trailer.is_err());
    });
}
