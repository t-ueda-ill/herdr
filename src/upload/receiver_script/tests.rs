//! Runs the receiver with every local POSIX shell against scratch directories.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::upload::test_support::{posix_shells, TempDir};

const STAGE_TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// Header lines and bodies, built the way the remote sink writes them.
#[derive(Default)]
pub(crate) struct Input(pub(crate) Vec<u8>);

impl Input {
    pub(crate) fn text(mut self, line: &str) -> Self {
        self.0.extend_from_slice(line.as_bytes());
        self
    }

    pub(crate) fn query(self, names: &[&str]) -> Self {
        let mut input = self;
        for name in names {
            input = input.text(&query_line(name));
        }
        input.text(QUERY_END)
    }

    pub(crate) fn dir(self, path: &str) -> Self {
        self.text(&directory_line(path))
    }

    pub(crate) fn link(self, path: &str, target: &str) -> Self {
        self.text(&symlink_lines(path, target))
    }

    pub(crate) fn file(mut self, path: &str, body: &[u8], mode: u32, mtime: Option<i64>) -> Self {
        self = self.text(&file_line(path, body.len() as u64, mode, mtime));
        self.0.extend_from_slice(body);
        self
    }

    pub(crate) fn commit(self, collision: PaneUploadCollision) -> Self {
        let line = commit_line(collision).unwrap();
        self.text(line)
    }
}

pub(crate) struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    chunks: mpsc::Receiver<Vec<u8>>,
    reader: TokenReader,
    tokens: Vec<Token>,
}

pub(crate) fn spawn(shell: &[String], dest: &str, fake_bin: Option<&Path>) -> Running {
    let mut command = Command::new(&shell[0]);
    command
        .args(&shell[1..])
        .args(["-c", RECEIVER_SCRIPT, "sh", dest, STAGE_TOKEN])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(bin) = fake_bin {
        let path = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{}:{path}", bin.display()));
    }
    let mut child = command.spawn().unwrap();
    let stdin = child.stdin.take();
    let mut out = child.stdout.take().unwrap();
    let (tx, chunks) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(read) = out.read(&mut buf) {
            if read == 0 || tx.send(buf[..read].to_vec()).is_err() {
                break;
            }
        }
    });
    Running {
        child,
        stdin,
        chunks,
        reader: TokenReader::default(),
        tokens: Vec::new(),
    }
}

impl Running {
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.stdin.as_mut().unwrap().write_all(bytes).unwrap();
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Reads output until a token equal to `want` arrives.
    pub(crate) fn wait_for(&mut self, want: &Token) {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !self.tokens.contains(want) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let chunk = self
                .chunks
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no {want:?} in {:?}", self.tokens));
            self.tokens.extend(self.reader.feed(&chunk).unwrap());
        }
    }

    /// Closes stdin and returns every token once the receiver exits.
    pub(crate) fn finish(mut self) -> Vec<Token> {
        drop(self.stdin.take());
        while let Ok(chunk) = self.chunks.recv_timeout(Duration::from_secs(60)) {
            self.tokens.extend(self.reader.feed(&chunk).unwrap());
        }
        let _ = self.child.wait();
        self.tokens
    }
}

fn run(shell: &[String], dest: &str, input: Input) -> Vec<Token> {
    run_with(shell, dest, input, None)
}

fn run_with(shell: &[String], dest: &str, input: Input, fake_bin: Option<&Path>) -> Vec<Token> {
    let mut running = spawn(shell, dest, fake_bin);
    running.write(&input.0);
    running.finish()
}

fn stage_path(dest: &Path) -> std::path::PathBuf {
    dest.join(format!(".herdr-upload-{STAGE_TOKEN}"))
}

fn published(tokens: &[Token]) -> Vec<(String, String)> {
    tokens
        .iter()
        .filter_map(|token| match token {
            Token::Published(name, path) => Some((name.clone(), path.clone())),
            _ => None,
        })
        .collect()
}

fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn mtime_of(path: &Path) -> i64 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A directory of fake tools placed first on PATH.
fn fake_bin(dir: &TempDir, name: &str, script: &str) -> std::path::PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = bin.join(name);
    std::fs::write(&tool, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[test]
fn the_script_parses_in_every_local_shell() {
    for shell in posix_shells() {
        let status = Command::new(&shell[0])
            .args(&shell[1..])
            .args(["-n", "-c", RECEIVER_SCRIPT])
            .status()
            .unwrap();
        assert!(status.success(), "{shell:?}");
    }
}

#[test]
fn the_script_never_needs_mktemp_or_tar() {
    assert!(!RECEIVER_SCRIPT.contains("mktemp"));
    assert!(!RECEIVER_SCRIPT.contains("tar "));
}

#[test]
fn bodies_of_every_size_arrive_intact_even_when_they_contain_header_lines() {
    let tricky = b"F 1 0644 - x\nD y\n\0C replace\n".to_vec();
    let sizes = [0usize, 1, 65_536, 65_537, 3 * 1024 * 1024];
    for shell in posix_shells() {
        let dest = TempDir::new("sizes");
        let mut input = Input::default();
        let mut bodies = Vec::new();
        for (index, size) in sizes.iter().enumerate() {
            let mut body: Vec<u8> = (0..*size).map(|i| (i * 7 + index) as u8).collect();
            if *size >= tricky.len() {
                body[..tricky.len()].copy_from_slice(&tricky);
            }
            input = input.file(&format!("f{index}"), &body, 0o644, None);
            bodies.push(body);
        }
        input = input.file("tricky", &tricky, 0o644, None);
        bodies.push(tricky.clone());

        let tokens = run(&shell, dest.str(), input.commit(PaneUploadCollision::Fail));

        assert!(tokens.contains(&Token::Committed), "{shell:?}: {tokens:?}");
        for (index, body) in bodies.iter().enumerate() {
            let name = if index < sizes.len() {
                format!("f{index}")
            } else {
                "tricky".into()
            };
            assert_eq!(
                &std::fs::read(dest.join(&name)).unwrap(),
                body,
                "{shell:?} {name}"
            );
        }
        assert!(!stage_path(dest.path()).exists());
    }
}

#[test]
fn a_body_trickling_in_one_byte_at_a_time_is_read_without_overrun() {
    for shell in posix_shells() {
        let dest = TempDir::new("trickle");
        let body: Vec<u8> = (0..1_500u32).map(|i| (i % 251) as u8).collect();
        let input = Input::default().file("slow", &body, 0o600, None);
        let mut running = spawn(&shell, dest.str(), None);
        running.write(&input.0[..input.0.len() - body.len()]);
        for (index, byte) in body.iter().enumerate() {
            running.write(&[*byte]);
            if index % 200 == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        running.write(commit_line(PaneUploadCollision::Fail).unwrap().as_bytes());

        let tokens = running.finish();

        assert!(tokens.contains(&Token::Committed), "{shell:?}: {tokens:?}");
        assert_eq!(std::fs::read(dest.join("slow")).unwrap(), body);
    }
}

#[test]
fn names_with_spaces_quotes_dollars_backslashes_and_leading_dashes_or_dots_publish_as_is() {
    let names = [
        "a b",
        "it's",
        "q\"x",
        "$HOME",
        "back\\slash",
        "-rf",
        ".hidden",
        "..dots",
    ];
    for shell in posix_shells() {
        let dest = TempDir::new("names");
        let mut input = Input::default();
        for name in names {
            input = input.file(name, name.as_bytes(), 0o644, None);
        }

        let tokens = run(&shell, dest.str(), input.commit(PaneUploadCollision::Fail));

        let mut got = published(&tokens);
        got.sort();
        let mut want: Vec<(String, String)> = names
            .iter()
            .map(|name| ((*name).to_owned(), format!("{}/{name}", dest.str())))
            .collect();
        want.sort();
        assert_eq!(got, want, "{shell:?}: {tokens:?}");
        for name in names {
            assert_eq!(std::fs::read(dest.join(name)).unwrap(), name.as_bytes());
        }
    }
}

#[test]
fn trees_keep_modes_times_and_symlinks_without_following_them() {
    for shell in posix_shells() {
        let dest = TempDir::new("tree");
        let input = Input::default()
            .dir("t")
            .dir("t/sub")
            .file("t/sub/run.sh", b"#!/bin/sh\n", 0o755, Some(1_600_000_000))
            .file("t/secret", b"s", 0o600, None)
            .link("t/up", "../../etc")
            .text(&nested_attrs_line("t/sub", 0o750, Some(1_500_000_000)))
            .text(&top_attrs_line("t", 0o755, Some(1_400_000_000)));

        let tokens = run(&shell, dest.str(), input.commit(PaneUploadCollision::Fail));

        assert!(tokens.contains(&Token::Committed), "{shell:?}: {tokens:?}");
        let tree = dest.join("t");
        assert_eq!(mode_of(&tree.join("sub/run.sh")), 0o755);
        assert_eq!(mtime_of(&tree.join("sub/run.sh")), 1_600_000_000);
        assert_eq!(mode_of(&tree.join("secret")), 0o600);
        assert_eq!(mode_of(&tree.join("sub")), 0o750);
        assert_eq!(mtime_of(&tree.join("sub")), 1_500_000_000);
        assert_eq!(mode_of(&tree), 0o755);
        assert_eq!(mtime_of(&tree), 1_400_000_000);
        assert_eq!(
            std::fs::read_link(tree.join("up")).unwrap(),
            Path::new("../../etc")
        );
        assert_eq!(
            tokens.iter().filter(|t| matches!(t, Token::Ok(_))).count(),
            5
        );
    }
}

#[test]
fn read_only_directories_publish_and_abort_without_leaking_the_stage() {
    for shell in posix_shells() {
        let dest = TempDir::new("readonly");
        let tree = Input::default()
            .dir("ro")
            .dir("ro/inner")
            .file("ro/inner/f", b"x", 0o444, None)
            .text(&nested_attrs_line("ro/inner", 0o555, None))
            .text(&top_attrs_line("ro", 0o555, None));

        let tokens = run(&shell, dest.str(), tree.commit(PaneUploadCollision::Fail));

        assert!(tokens.contains(&Token::Committed), "{shell:?}: {tokens:?}");
        assert_eq!(mode_of(&dest.join("ro")), 0o555);
        assert!(!stage_path(dest.path()).exists());

        let aborted = TempDir::new("readonly-abort");
        let partial = Input::default()
            .dir("ro")
            .dir("ro/inner")
            .file("ro/inner/f", b"x", 0o444, None)
            .text(&nested_attrs_line("ro/inner", 0o555, None));
        let tokens = run(&shell, aborted.str(), partial);

        assert_eq!(tokens.last(), Some(&Token::Aborted), "{shell:?}");
        assert!(!stage_path(aborted.path()).exists(), "{shell:?}");
    }
}

#[test]
fn rename_picks_the_first_free_numbered_name_keeping_file_extensions() {
    for shell in posix_shells() {
        let dest = TempDir::new("rename");
        for existing in ["a.txt", "a.tar.gz", ".bashrc", "d", "noext", "noext (1)"] {
            std::fs::write(dest.join(existing), b"old").unwrap();
        }
        let input = Input::default()
            .file("a.txt", b"new", 0o644, None)
            .file("a.tar.gz", b"new", 0o644, None)
            .file(".bashrc", b"new", 0o644, None)
            .dir("d")
            .file("noext", b"new", 0o644, None);

        let tokens = run(
            &shell,
            dest.str(),
            input.commit(PaneUploadCollision::Rename),
        );

        let mut got: Vec<String> = published(&tokens)
            .into_iter()
            .map(|(_, path)| path.rsplit('/').next().unwrap().to_owned())
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                ".bashrc (1)",
                "a (1).txt",
                "a.tar (1).gz",
                "d (1)",
                "noext (2)"
            ],
            "{shell:?}: {tokens:?}"
        );
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old");
        assert_eq!(std::fs::read(dest.join("a (1).txt")).unwrap(), b"new");
    }
}

#[test]
fn rename_never_collides_with_another_name_of_the_same_upload() {
    for shell in posix_shells() {
        let dest = TempDir::new("rename-self");
        std::fs::write(dest.join("x"), b"old").unwrap();
        let input = Input::default()
            .file("x", b"one", 0o644, None)
            .file("x (1)", b"two", 0o644, None);

        let tokens = run(
            &shell,
            dest.str(),
            input.commit(PaneUploadCollision::Rename),
        );

        assert!(tokens.contains(&Token::Committed), "{shell:?}: {tokens:?}");
        assert_eq!(std::fs::read(dest.join("x (1)")).unwrap(), b"two");
        assert_eq!(std::fs::read(dest.join("x (2)")).unwrap(), b"one");
    }
}

#[test]
fn replace_swaps_files_and_whole_directories_and_drops_the_originals() {
    for shell in posix_shells() {
        let dest = TempDir::new("replace");
        std::fs::write(dest.join("f"), b"old").unwrap();
        std::fs::create_dir(dest.join("d")).unwrap();
        std::fs::write(dest.join("d/stale"), b"old").unwrap();
        let input = Input::default()
            .file("f", b"new", 0o644, None)
            .dir("d")
            .file("d/fresh", b"new", 0o644, None);

        let tokens = run(
            &shell,
            dest.str(),
            input.commit(PaneUploadCollision::Replace),
        );

        assert!(tokens.contains(&Token::Committed), "{shell:?}: {tokens:?}");
        assert_eq!(std::fs::read(dest.join("f")).unwrap(), b"new");
        assert!(dest.join("d/fresh").exists());
        assert!(!dest.join("d/stale").exists(), "directories are not merged");
        assert!(!stage_path(dest.path()).exists());
    }
}

#[test]
fn fail_reports_every_conflict_keeps_the_stage_and_accepts_another_commit() {
    for shell in posix_shells() {
        let dest = TempDir::new("fail");
        std::fs::write(dest.join("a"), b"old").unwrap();
        std::fs::write(dest.join("b"), b"old").unwrap();
        let mut running = spawn(&shell, dest.str(), None);
        let input = Input::default()
            .file("a", b"new", 0o644, None)
            .file("b", b"new", 0o644, None)
            .file("c", b"new", 0o644, None)
            .commit(PaneUploadCollision::Fail);
        running.write(&input.0);
        running.wait_for(&Token::Conflict);
        // The stage must survive the refused commit for the second one to work.
        assert!(stage_path(dest.path()).exists(), "{shell:?}");
        assert!(!dest.join("c").exists(), "nothing is published on conflict");
        running.write(commit_line(PaneUploadCollision::Rename).unwrap().as_bytes());

        let tokens = running.finish();

        let conflict = tokens.iter().position(|t| *t == Token::Conflict).unwrap();
        assert!(tokens[..conflict].contains(&Token::Exists("a".into())));
        assert!(tokens[..conflict].contains(&Token::Exists("b".into())));
        assert!(tokens.contains(&Token::Committed), "{shell:?}: {tokens:?}");
        assert_eq!(std::fs::read(dest.join("a")).unwrap(), b"old");
        assert_eq!(std::fs::read(dest.join("a (1)")).unwrap(), b"new");
        assert!(dest.join("c").exists());
    }
}

/// Shells whose `PATH` lookup reaches a fake tool. BusyBox's `sh` runs applets
/// such as `mv` and `tee` in-process and never consults `PATH` for them, so a
/// fake cannot be injected there; skipping beats a test that injects nothing.
fn shells_honouring_path() -> Vec<Vec<String>> {
    let probe = TempDir::new("probe");
    let bin = fake_bin(&probe, "mv", "echo faked");
    posix_shells()
        .into_iter()
        .filter(|shell| {
            let mut command = Command::new(&shell[0]);
            command.args(&shell[1..]).args(["-c", "mv"]);
            let path = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{path}", bin.display()));
            let faked = command
                .output()
                .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("faked"));
            if !faked {
                eprintln!("{shell:?}: runs applets in-process, skipping fault injection");
            }
            faked
        })
        .collect()
}

#[test]
fn a_failed_publish_rolls_back_everything_including_replaced_originals() {
    for shell in shells_honouring_path() {
        let dest = TempDir::new("rollback");
        std::fs::write(dest.join("a"), b"old-a").unwrap();
        // `mv` refuses to publish `z`, after `a` was already swapped in.
        let bin = fake_bin(
            &dest,
            "mv",
            r#"for last; do :; done; case $last in */z) exit 1 ;; esac; exec /bin/mv "$@""#,
        );
        let target = dest.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("a"), b"old-a").unwrap();
        let input = Input::default()
            .file("a", b"new", 0o644, None)
            .file("z", b"new", 0o644, None)
            .commit(PaneUploadCollision::Replace);

        let tokens = run_with(&shell, target.to_str().unwrap(), input, Some(&bin));

        assert_eq!(
            tokens.last(),
            Some(&Token::Error("commit".into(), "publish".into())),
            "{shell:?}: {tokens:?}"
        );
        assert_eq!(std::fs::read(target.join("a")).unwrap(), b"old-a");
        assert!(!target.join("z").exists());
        assert!(!stage_path(&target).exists());
    }
}

#[test]
fn a_rollback_that_cannot_restore_keeps_the_stage_and_reports_its_path() {
    for shell in shells_honouring_path() {
        let dir = TempDir::new("rbfail");
        let target = dir.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("a"), b"old-a").unwrap();
        // Publishing `z` fails, and so does moving the original `a` back.
        let bin = fake_bin(
            &dir,
            "mv",
            r#"for last; do :; done; case $last in */z) exit 1 ;; esac
case $2 in */o/*) exit 1 ;; esac; exec /bin/mv "$@""#,
        );
        let input = Input::default()
            .file("a", b"new", 0o644, None)
            .file("z", b"new", 0o644, None)
            .commit(PaneUploadCollision::Replace);

        let tokens = run_with(&shell, target.to_str().unwrap(), input, Some(&bin));

        let stage = stage_path(&target);
        assert_eq!(
            tokens.last(),
            Some(&Token::RollbackFailed(stage.to_str().unwrap().into())),
            "{shell:?}: {tokens:?}"
        );
        assert_eq!(std::fs::read(stage.join("o/1")).unwrap(), b"old-a");
    }
}

#[test]
fn a_short_write_is_an_error_and_never_reported_ok() {
    for shell in shells_honouring_path() {
        let dir = TempDir::new("short");
        // A `tee` that forwards everything but keeps only one byte, as on ENOSPC.
        let bin = fake_bin(
            &dir,
            "tee",
            r#"t=$(mktemp); cat > "$t"; head -c 1 "$t" >> "$2"; cat "$t"; rm -f "$t""#,
        );
        let target = dir.join("target");
        std::fs::create_dir(&target).unwrap();
        let input = Input::default().file("f", b"hello", 0o644, None);

        let tokens = run_with(&shell, target.to_str().unwrap(), input, Some(&bin));

        assert_eq!(
            tokens.last(),
            Some(&Token::Error("1".into(), "short".into())),
            "{shell:?}: {tokens:?}"
        );
        assert!(!tokens.contains(&Token::Ok(1)));
        assert!(!stage_path(&target).exists());
    }
}

#[test]
fn end_of_input_or_terminate_while_staging_removes_the_stage() {
    for shell in posix_shells() {
        let dest = TempDir::new("eof");
        let tokens = run(
            &shell,
            dest.str(),
            Input::default().dir("d").file("d/f", b"x", 0o644, None),
        );
        assert_eq!(tokens.last(), Some(&Token::Aborted), "{shell:?}");
        assert!(!stage_path(dest.path()).exists());

        let dest = TempDir::new("term");
        let mut running = spawn(&shell, dest.str(), None);
        // A file header whose body never comes leaves the receiver waiting in dd.
        running.write(file_line("big", 10, 0o644, None).as_bytes());
        running.wait_for(&Token::Ready(
            std::fs::canonicalize(dest.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
        ));
        std::thread::sleep(Duration::from_millis(100));
        assert!(stage_path(dest.path()).exists());
        crate::platform::signal_processes(&[running.pid()], crate::platform::Signal::Terminate);
        let _ = running.finish();
        assert!(!stage_path(dest.path()).exists(), "{shell:?}");
    }
}

#[test]
fn terminate_during_commit_still_ends_published_or_rolled_back_with_originals_intact() {
    for shell in posix_shells() {
        let dest = TempDir::new("commit-term");
        let names: Vec<String> = (0..300).map(|i| format!("n{i:03}")).collect();
        for name in &names {
            std::fs::write(dest.join(name), b"old").unwrap();
        }
        let mut input = Input::default();
        for name in &names {
            input = input.file(name, b"new", 0o644, None);
        }
        let mut running = spawn(&shell, dest.str(), None);
        running.write(&input.0);
        running.wait_for(&Token::Ok(names.len() as u64));
        running.write(
            commit_line(PaneUploadCollision::Replace)
                .unwrap()
                .as_bytes(),
        );
        std::thread::sleep(Duration::from_millis(20));
        crate::platform::signal_processes(&[running.pid()], crate::platform::Signal::Terminate);

        let tokens = running.finish();

        let committed = tokens.contains(&Token::Committed);
        for name in &names {
            let content = std::fs::read(dest.join(name)).unwrap();
            if committed {
                assert_eq!(content, b"new", "{shell:?} {name}");
            } else {
                assert_eq!(content, b"old", "{shell:?} {name}: {tokens:?}");
            }
        }
    }
}

#[test]
fn queries_report_existing_names_then_the_end_marker() {
    for shell in posix_shells() {
        let dest = TempDir::new("query");
        std::fs::write(dest.join("here"), b"").unwrap();
        std::os::unix::fs::symlink("/nonexistent", dest.join("dangling")).unwrap();

        let tokens = run(
            &shell,
            dest.str(),
            Input::default().query(&["here", "gone", "dangling"]),
        );

        assert_eq!(
            tokens,
            [
                Token::Ready(
                    std::fs::canonicalize(dest.path())
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .into()
                ),
                Token::Exists("here".into()),
                Token::Exists("dangling".into()),
                Token::QueryEnd,
                Token::Aborted
            ],
            "{shell:?}"
        );
    }
}

#[test]
fn a_missing_or_non_directory_destination_is_reported_before_any_stage_exists() {
    for shell in posix_shells() {
        let dir = TempDir::new("dest");
        std::fs::write(dir.join("file"), b"").unwrap();

        let missing = run(&shell, dir.join("nope").to_str().unwrap(), Input::default());
        let file = run(&shell, dir.join("file").to_str().unwrap(), Input::default());

        assert_eq!(missing, [Token::NotFound], "{shell:?}");
        assert_eq!(file, [Token::NotDirectory], "{shell:?}");
    }
}

#[test]
fn an_empty_destination_means_the_login_directory() {
    let home = TempDir::new("home");
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", RECEIVER_SCRIPT, "sh", "", STAGE_TOKEN])
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped());

    let output = command.output().unwrap();
    let tokens = TokenReader::default().feed(&output.stdout).unwrap();

    let canonical = std::fs::canonicalize(home.path()).unwrap();
    assert_eq!(tokens[0], Token::Ready(canonical.to_str().unwrap().into()));
}

#[test]
fn the_stale_sweep_removes_only_old_empty_stages_and_warns_about_kept_originals() {
    for shell in posix_shells() {
        let dest = TempDir::new("sweep");
        let old_empty = dest.join(".herdr-upload-old-empty");
        let old_kept = dest.join(".herdr-upload-old-kept");
        let fresh = dest.join(".herdr-upload-fresh");
        for stage in [&old_empty, &old_kept, &fresh] {
            std::fs::create_dir_all(stage.join("o")).unwrap();
        }
        std::fs::write(old_kept.join("o/1"), b"original").unwrap();
        let two_days_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 86_400);
        for stage in [&old_empty, &old_kept] {
            let status = Command::new("touch")
                .arg("-d")
                .arg(format!(
                    "@{}",
                    two_days_ago
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                ))
                .arg(stage)
                .status()
                .unwrap();
            assert!(status.success());
        }

        let tokens = run(&shell, dest.str(), Input::default());

        assert!(!old_empty.exists(), "{shell:?}");
        assert!(old_kept.join("o/1").exists());
        assert!(fresh.exists());
        assert!(tokens.contains(&Token::Warning(
            "leftover".into(),
            old_kept.to_str().unwrap().into()
        )));
    }
}

#[test]
fn an_existing_stage_name_is_refused_instead_of_reused() {
    let dest = TempDir::new("taken");
    std::fs::create_dir(stage_path(dest.path())).unwrap();

    let tokens = run(&["/bin/sh".to_owned()], dest.str(), Input::default());

    assert_eq!(tokens, [Token::StageFailed]);
}

#[test]
fn token_reader_skips_a_login_banner_and_splits_tokens_across_feeds() {
    let mut reader = TokenReader::default();
    let mut tokens = reader.feed(b"Welcome!\nHERDR-OFFHOST-1\0read").unwrap();
    tokens.extend(reader.feed(b"y\0/x\0ok\x001\0err\x002\0sh").unwrap());
    tokens.extend(reader.feed(b"ort\0").unwrap());

    assert_eq!(
        tokens,
        [
            Token::Ready("/x".into()),
            Token::Ok(1),
            Token::Error("2".into(), "short".into())
        ]
    );
    assert!(reader.feed(b"bogus\0").is_err());
}

#[test]
fn header_lines_put_octal_modes_and_stamps_before_the_path() {
    assert_eq!(
        file_line("a b", 3, 0o644, Some(0)),
        "F 3 0644 197001010000.00 a b\n"
    );
    assert_eq!(nested_attrs_line("d/e", 0o755, None), "M 0755 - d/e\n");
    assert_eq!(top_attrs_line("d", 0o700, None), "T 0700 - d\n");
    assert_eq!(commit_line(PaneUploadCollision::Unknown), None);
}

#[test]
fn a_file_is_never_written_through_a_symlink_already_at_its_path() {
    // Where names fold (case or Unicode), `F a` can land on the symlink `A`;
    // an exact duplicate reproduces that on any filesystem.
    for shell in posix_shells() {
        let dir = TempDir::new("through-link");
        let victim = dir.join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        let victim_mode = mode_of(&victim);
        let target = dir.join("target");
        std::fs::create_dir(&target).unwrap();
        let input = Input::default()
            .link("a", victim.to_str().unwrap())
            .file("a", b"payload", 0o777, None);

        let tokens = run(&shell, target.to_str().unwrap(), input);

        assert_eq!(
            tokens.last(),
            Some(&Token::Error("2".into(), "create".into())),
            "{shell:?}: {tokens:?}"
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        assert_eq!(mode_of(&victim), victim_mode);
        assert!(!stage_path(&target).exists());
    }
}

#[test]
fn nothing_is_created_below_a_symlinked_parent_or_inside_an_existing_directory() {
    for shell in posix_shells() {
        let dir = TempDir::new("below-link");
        let outside = dir.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let target = dir.join("target");
        std::fs::create_dir(&target).unwrap();
        let link = || Input::default().link("a", outside.to_str().unwrap());

        let mkdir = run(&shell, target.to_str().unwrap(), link().dir("a/sub"));
        let file = run(
            &shell,
            target.to_str().unwrap(),
            link().file("a/x", b"payload", 0o644, None),
        );
        let symlink = run(
            &shell,
            target.to_str().unwrap(),
            Input::default().dir("d").link("d", "/etc/passwd"),
        );

        assert_eq!(
            mkdir.last(),
            Some(&Token::Error("2".into(), "mkdir".into())),
            "{shell:?}: {mkdir:?}"
        );
        assert_eq!(
            file.last(),
            Some(&Token::Error("2".into(), "create".into())),
            "{shell:?}: {file:?}"
        );
        assert_eq!(
            symlink.last(),
            Some(&Token::Error("2".into(), "symlink".into())),
            "{shell:?}: {symlink:?}"
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "{shell:?}");
        assert!(!stage_path(&target).exists());
    }
}

#[test]
fn commit_reports_a_step_for_every_nested_directory_and_published_name() {
    for shell in posix_shells() {
        let dest = TempDir::new("steps");
        let input = Input::default()
            .dir("t")
            .dir("t/a")
            .dir("t/b")
            .file("f", b"x", 0o644, None)
            .text(&nested_attrs_line("t/a", 0o755, None))
            .text(&nested_attrs_line("t/b", 0o755, None))
            .text(&top_attrs_line("t", 0o755, None));

        let tokens = run(&shell, dest.str(), input.commit(PaneUploadCollision::Fail));

        let committed = tokens.iter().position(|t| *t == Token::Committed);
        let steps = tokens.iter().filter(|t| **t == Token::Step).count();
        assert!(committed.is_some(), "{shell:?}: {tokens:?}");
        // Two nested directories, two published names, one top-level directory.
        assert!(steps >= 5, "{shell:?}: {tokens:?}");
    }
}

#[test]
fn a_cleanup_that_fails_after_publishing_still_reports_every_path_and_keeps_no_originals() {
    for shell in shells_honouring_path() {
        let dir = TempDir::new("late-wipe");
        let target = dir.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("a"), b"old").unwrap();
        // Removing the stage is the last step, and it must not hold back the report.
        let bin = fake_bin(&dir, "rm", "exit 1");
        let input = Input::default()
            .file("a", b"new", 0o644, None)
            .file("b", b"new", 0o644, None)
            .commit(PaneUploadCollision::Replace);

        let tokens = run_with(&shell, target.to_str().unwrap(), input, Some(&bin));

        assert_eq!(published(&tokens).len(), 2, "{shell:?}: {tokens:?}");
        assert_eq!(std::fs::read(target.join("a")).unwrap(), b"new");
        let stage = stage_path(&target);
        // The stale sweep removes a stage whose `o/` is empty, so a cleanup
        // cut short after publishing never reads as kept originals.
        assert!(!stage.join("o").exists(), "{shell:?}");
    }
}
