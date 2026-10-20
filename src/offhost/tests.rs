use std::path::{Path, PathBuf};
use std::process::Command;

use super::exec::ExecError;
use super::ops::{self, ScriptRunner};
use crate::api::schema::{
    DirectoryEntry, DirectoryEntryKind, PaneDirectoryIndexing, PaneDirectoryListing, PaneLocation,
    PaneLocationKind,
};

/// Runs a script like the transports do (`sh -c script sh args…`) on this host.
struct LocalShell {
    shell: Vec<String>,
    home: Option<PathBuf>,
    /// Directory searched before `PATH`, to stand in for tools.
    tool_dir: Option<PathBuf>,
    /// Replaces `PATH` entirely, to take a tool away.
    only_path: Option<PathBuf>,
    /// Printed by the "login shell" before the script runs.
    banner: Option<&'static str>,
    seconds_left: Option<u64>,
}

impl LocalShell {
    fn new(shell: &[&str]) -> Self {
        Self {
            shell: shell.iter().map(|part| (*part).to_owned()).collect(),
            home: None,
            tool_dir: None,
            only_path: None,
            banner: None,
            seconds_left: None,
        }
    }

    fn sh() -> Self {
        Self::new(&["/bin/sh"])
    }
}

impl ScriptRunner for LocalShell {
    fn seconds_left(&self) -> Option<u64> {
        self.seconds_left
    }

    fn run(&self, script: &str, args: &[&str]) -> Result<Vec<u8>, ExecError> {
        let mut command = Command::new(&self.shell[0]);
        command
            .args(&self.shell[1..])
            .arg("-c")
            .arg(match self.banner {
                Some(banner) => format!("printf '%s' '{banner}'; {script}"),
                None => script.to_owned(),
            })
            .arg("sh")
            .args(args);
        if let Some(only) = &self.only_path {
            command.env("PATH", only);
        }
        match &self.home {
            Some(home) => command.env("HOME", home),
            None => command.env_remove("HOME"),
        };
        if let Some(tool_dir) = &self.tool_dir {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut joined = tool_dir.as_os_str().to_owned();
            joined.push(":");
            joined.push(path);
            command.env("PATH", joined);
        }
        let output = command.output().expect("spawn shell");
        if !output.status.success() {
            return Err(ExecError::Failed {
                over_ssh: false,
                detail: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(output.stdout)
    }
}

/// Every POSIX shell worth proving the scripts against; BusyBox only when installed.
fn shells() -> Vec<LocalShell> {
    let mut shells = vec![LocalShell::sh()];
    let has_busybox = Command::new("busybox")
        .arg("true")
        .output()
        .is_ok_and(|output| output.status.success());
    if has_busybox {
        shells.push(LocalShell::new(&["busybox", "sh"]));
    }
    shells
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "herdr-offhost-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        // Canonical so expectations match where /tmp is a symlink.
        Self(std::fs::canonicalize(&path).unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn str(&self) -> &str {
        self.0.to_str().unwrap()
    }

    fn file(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn location() -> PaneLocation {
    PaneLocation {
        kind: PaneLocationKind::Ssh,
        ssh_host: Some("devbox".into()),
        container: None,
    }
}

fn list(
    shell: &LocalShell,
    root: Option<&str>,
    path: Option<&str>,
) -> Result<PaneDirectoryListing, (&'static str, String)> {
    ops::list(shell, location(), root, path, false)
}

fn index(shell: &LocalShell, path: &str) -> PaneDirectoryIndexing {
    ops::index(shell, location(), None, Some(path), false).expect("index")
}

fn names(listing: &PaneDirectoryListing) -> Vec<&str> {
    listing.entries.iter().map(|e| e.name.as_str()).collect()
}

fn entry<'a>(listing: &'a PaneDirectoryListing, name: &str) -> &'a DirectoryEntry {
    listing
        .entries
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("missing entry {name}: {listing:?}"))
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=herdr@example.invalid",
            "-c",
            "user.name=Herdr Test",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repo(name: &str) -> TempDir {
    let repo = TempDir::new(name);
    git(repo.path(), &["init", "--quiet"]);
    repo
}

/// A `git` that refuses every call the way git refuses another user's checkout.
fn untrusting_git() -> TempDir {
    let dir = TempDir::new("untrusting-git");
    let path = dir.file(
        "git",
        b"#!/bin/sh\necho \"fatal: detected dubious ownership in repository\" >&2\nexit 128\n",
    );
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

#[test]
fn listing_puts_directories_first_then_sorts_names_case_insensitively() {
    for shell in shells() {
        let dir = TempDir::new("order");
        dir.file("b.txt", b"");
        dir.file("A.txt", b"");
        dir.dir("zeta");
        dir.dir("alpha");

        let listing = list(&shell, None, Some(dir.str())).unwrap();

        assert_eq!(names(&listing), ["alpha", "zeta", "A.txt", "b.txt"]);
        assert_eq!(listing.canonical, dir.str());
        assert_eq!(listing.path, dir.str());
        assert!(!listing.truncated);
    }
}

#[test]
fn listing_includes_hidden_entries_and_excludes_dot_git() {
    for shell in shells() {
        let dir = TempDir::new("hidden");
        dir.file(".env", b"");
        dir.file("..double", b"");
        dir.dir(".git");
        dir.file("plain", b"");

        let listing = list(&shell, None, Some(dir.str())).unwrap();

        assert_eq!(names(&listing), ["..double", ".env", "plain"]);
        assert!(entry(&listing, ".env").hidden);
        assert!(entry(&listing, "..double").hidden);
        assert!(!entry(&listing, "plain").hidden);
    }
}

#[test]
fn listing_keeps_names_with_spaces_newlines_and_leading_dashes_intact() {
    for shell in shells() {
        let dir = TempDir::new("odd-names");
        dir.file("a b.txt", b"");
        dir.file("line\nbreak", b"");
        dir.file("-dash", b"");
        dir.file("-e", b"");
        dir.file("quote'name", b"");
        dir.file("$(touch pwned)", b"");

        let listing = list(&shell, None, Some(dir.str())).unwrap();

        let mut got = names(&listing);
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "$(touch pwned)",
                "-dash",
                "-e",
                "a b.txt",
                "line\nbreak",
                "quote'name"
            ]
        );
        assert!(!dir.path().join("pwned").exists());
    }
}

// Why not every Unix: macOS filesystems refuse to create names that are not UTF-8.
#[cfg(target_os = "linux")]
#[test]
fn listing_counts_non_utf8_names_as_unlistable_and_omits_them() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    for shell in shells() {
        let dir = TempDir::new("non-utf8");
        dir.file("good.txt", b"");
        std::fs::write(dir.path().join(OsStr::from_bytes(b"bad-\xff.txt")), b"").unwrap();
        std::fs::write(dir.path().join(OsStr::from_bytes(b"worse-\xfe")), b"").unwrap();

        let listing = list(&shell, None, Some(dir.str())).unwrap();

        assert_eq!(names(&listing), ["good.txt"]);
        assert_eq!(listing.unlistable, Some(2));
    }
}

#[test]
fn listing_classifies_symlinks_by_what_they_point_at() {
    for shell in shells() {
        let dir = TempDir::new("links");
        dir.dir("real-dir");
        dir.file("real-file", b"x");
        std::os::unix::fs::symlink("real-dir", dir.path().join("to-dir")).unwrap();
        std::os::unix::fs::symlink("real-file", dir.path().join("to-file")).unwrap();
        std::os::unix::fs::symlink("missing", dir.path().join("dangling")).unwrap();
        std::os::unix::fs::symlink("loop-b", dir.path().join("loop-a")).unwrap();
        std::os::unix::fs::symlink("loop-a", dir.path().join("loop-b")).unwrap();

        let listing = list(&shell, None, Some(dir.str())).unwrap();

        let link = |name: &str| {
            let entry = entry(&listing, name);
            (entry.kind, entry.target_kind)
        };
        assert_eq!(
            link("to-dir"),
            (DirectoryEntryKind::Symlink, Some(DirectoryEntryKind::Dir))
        );
        assert_eq!(
            link("to-file"),
            (DirectoryEntryKind::Symlink, Some(DirectoryEntryKind::File))
        );
        assert_eq!(link("dangling"), (DirectoryEntryKind::Symlink, None));
        assert_eq!(link("loop-a"), (DirectoryEntryKind::Symlink, None));
        // Links to directories sort with the directories.
        assert_eq!(&names(&listing)[..2], ["real-dir", "to-dir"]);
    }
}

#[test]
fn listing_reports_the_physical_path_of_a_symlinked_directory() {
    for shell in shells() {
        let dir = TempDir::new("canonical");
        dir.dir("real");
        std::os::unix::fs::symlink("real", dir.path().join("link")).unwrap();
        let through_link = dir.path().join("link");

        let listing = list(&shell, None, Some(through_link.to_str().unwrap())).unwrap();

        assert_eq!(listing.path, through_link.to_str().unwrap());
        assert_eq!(listing.canonical, dir.path().join("real").to_str().unwrap());
    }
}

#[test]
fn listing_truncates_at_two_thousand_entries_and_counts_the_rest() {
    let dir = TempDir::new("many");
    for index in 0..2_050 {
        dir.file(&format!("f{index:04}"), b"");
    }

    let listing = list(&LocalShell::sh(), None, Some(dir.str())).unwrap();

    assert_eq!(listing.entries.len(), 2_000);
    assert!(listing.truncated);
    assert_eq!(listing.omitted, Some(50));
}

#[test]
fn listing_counts_entries_the_script_did_not_print_as_omitted() {
    let dir = TempDir::new("huge");
    let total = super::scripts::LIST_SCRIPT_ENTRY_CAP + 100;
    for index in 0..total {
        dir.file(&format!("f{index:05}"), b"");
    }

    let listing = list(&LocalShell::sh(), None, Some(dir.str())).unwrap();

    assert_eq!(listing.entries.len(), 2_000);
    assert!(listing.truncated);
    assert_eq!(listing.omitted, Some(u64::try_from(total - 2_000).unwrap()));
}

#[test]
fn listing_marks_git_ignored_entries_inside_a_repository() {
    for shell in shells() {
        let repo = repo("ignored");
        repo.file(".gitignore", b"*.log\nbuild/\n");
        repo.file("src/keep.rs", b"");
        repo.file("src/debug.log", b"");
        repo.dir("src/build");

        let listing = list(
            &shell,
            None,
            Some(repo.path().join("src").to_str().unwrap()),
        )
        .unwrap();

        assert_eq!(entry(&listing, "keep.rs").ignored, Some(false));
        assert_eq!(entry(&listing, "debug.log").ignored, Some(true));
        assert_eq!(entry(&listing, "build").ignored, Some(true));
        assert_eq!(listing.git_error, None);
    }
}

#[test]
fn listing_leaves_ignored_unset_outside_a_repository() {
    let dir = TempDir::new("no-git");
    dir.file("a", b"");

    let listing = list(&LocalShell::sh(), None, Some(dir.str())).unwrap();

    assert_eq!(entry(&listing, "a").ignored, None);
    assert_eq!(listing.git_error, None);
}

#[test]
fn listing_reports_an_untrusted_repository_and_still_lists_entries() {
    let repo = repo("untrusted");
    repo.file("a", b"");
    let tools = untrusting_git();
    let mut shell = LocalShell::sh();
    shell.tool_dir = Some(tools.path().to_owned());

    let listing = list(&shell, None, Some(repo.str())).unwrap();

    assert_eq!(listing.git_error.as_deref(), Some("untrusted repository"));
    assert_eq!(entry(&listing, "a").ignored, None);
}

#[test]
fn listing_trusts_a_repository_only_when_asked() {
    let repo = repo("trust-flag");
    repo.file("a", b"");
    repo.file(".gitignore", b"b\n");
    repo.file("b", b"");
    let tools = TempDir::new("recording-git");
    // Records the `-c safe.directory=` argument it was given before delegating.
    let log = tools.path().join("calls.log");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexec {} \"$@\"\n",
        log.display(),
        which_git().display()
    );
    let path = tools.file("git", script.as_bytes());
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut shell = LocalShell::sh();
    shell.tool_dir = Some(tools.path().to_owned());

    ops::list(&shell, location(), None, Some(repo.str()), false).unwrap();
    let untrusting = std::fs::read_to_string(&log).unwrap();
    std::fs::remove_file(&log).unwrap();
    ops::list(&shell, location(), None, Some(repo.str()), true).unwrap();
    let trusting = std::fs::read_to_string(&log).unwrap();

    assert!(!untrusting.contains("safe.directory"), "{untrusting}");
    assert!(
        trusting.contains(&format!("safe.directory={}", repo.str())),
        "{trusting}"
    );
}

fn which_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH")
}

#[test]
fn listing_without_a_path_starts_in_the_home_directory() {
    let home = TempDir::new("home");
    home.file("in-home", b"");
    let mut shell = LocalShell::sh();
    shell.home = Some(home.path().to_owned());

    let listing = list(&shell, None, None).unwrap();

    assert_eq!(listing.root, home.str());
    assert_eq!(listing.path, home.str());
    assert_eq!(names(&listing), ["in-home"]);
}

#[test]
fn listing_an_explicit_path_keeps_the_login_directory_as_the_root_of_an_unreported_pane() {
    let home = TempDir::new("home-root");
    let elsewhere = TempDir::new("elsewhere");
    let mut shell = LocalShell::sh();
    shell.home = Some(home.path().to_owned());

    let listing = list(&shell, None, Some(elsewhere.str())).unwrap();

    assert_eq!(listing.root, home.str());
    assert_eq!(listing.path, elsewhere.str());
}

#[test]
fn listing_without_a_path_or_home_starts_at_the_filesystem_root() {
    let listing = list(&LocalShell::sh(), None, None).unwrap();

    assert_eq!(listing.root, "/");
}

#[test]
fn listing_prefers_the_reported_root_over_the_login_directory() {
    let home = TempDir::new("home-ignored");
    let reported = TempDir::new("reported");
    reported.file("mine", b"");
    let mut shell = LocalShell::sh();
    shell.home = Some(home.path().to_owned());

    let listing = list(&shell, Some(reported.str()), None).unwrap();

    assert_eq!(listing.root, reported.str());
    assert_eq!(names(&listing), ["mine"]);
}

#[test]
fn listing_reports_missing_paths_files_and_denied_directories() {
    let dir = TempDir::new("errors");
    let file = dir.file("plain", b"");
    let missing = dir.path().join("missing");

    let code = |path: &Path| {
        list(&LocalShell::sh(), None, Some(path.to_str().unwrap()))
            .unwrap_err()
            .0
    };

    assert_eq!(code(&missing), "not_found");
    assert_eq!(code(&file), "not_a_directory");
}

#[test]
fn requests_reject_relative_and_parent_relative_paths() {
    for path in ["", "src", "../etc", "/srv/../etc", "/a/..", "~/x", "/a\0b"] {
        let list = ops::list(&LocalShell::sh(), location(), None, Some(path), false);
        assert_eq!(list.unwrap_err().0, "invalid_params", "{path:?}");
        let read = ops::read(&LocalShell::sh(), location(), path, None);
        assert_eq!(read.unwrap_err().0, "invalid_params", "{path:?}");
    }
}

#[test]
fn index_lists_tracked_and_untracked_files_but_not_ignored_ones() {
    for shell in shells() {
        let repo = repo("index-git");
        repo.file(".gitignore", b"ignored.txt\n");
        repo.file("tracked.rs", b"");
        repo.file("sub/deep file.rs", b"");
        repo.file("ignored.txt", b"");
        git(repo.path(), &["add", "tracked.rs"]);
        repo.file("untracked.rs", b"");

        let index = ops::index(&shell, location(), None, Some(repo.str()), false).unwrap();

        assert_eq!(index.repo_root.as_deref(), Some(repo.str()));
        assert_eq!(
            index.entries,
            [
                ".gitignore",
                "sub/deep file.rs",
                "tracked.rs",
                "untracked.rs"
            ]
        );
        assert!(!index.truncated);
    }
}

#[test]
fn index_from_a_subdirectory_is_relative_to_the_repository_root() {
    let repo = repo("index-sub");
    repo.file("a/one.rs", b"");
    repo.file("b/two.rs", b"");

    let index = index(&LocalShell::sh(), repo.path().join("a").to_str().unwrap());

    assert_eq!(index.repo_root.as_deref(), Some(repo.str()));
    assert_eq!(index.entries, ["a/one.rs", "b/two.rs"]);
}

#[test]
fn index_excludes_submodule_gitlinks_and_nested_repository_contents() {
    let repo = repo("index-gitlink");
    repo.file("keep.rs", b"");
    git(repo.path(), &["add", "keep.rs"]);
    git(
        repo.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,e69de29bb2d1d6434b8b29ae775ad8c2e48c5391,vendor/lib",
        ],
    );
    let nested = repo.dir("nested");
    git(&nested, &["init", "--quiet"]);
    repo.file("nested/inner.rs", b"");

    let index = index(&LocalShell::sh(), repo.str());

    assert_eq!(index.entries, ["keep.rs"]);
}

#[test]
fn index_without_git_falls_back_to_find_and_skips_dot_git_directories() {
    for shell in shells() {
        let dir = TempDir::new("index-find");
        dir.file("a.txt", b"");
        dir.file("sub/b c.txt", b"");
        dir.file("line\nbreak", b"");
        dir.file(".git/config", b"");
        dir.file("sub/.git/HEAD", b"");

        let index = index(&shell, dir.str());

        assert_eq!(index.entries, ["a.txt", "line\nbreak", "sub/b c.txt"]);
        // `find` paths are relative to the searched directory.
        assert_eq!(index.repo_root.as_deref(), Some(dir.str()));
        assert_eq!(index.git_error, None);
    }
}

#[test]
fn index_falls_back_to_find_and_reports_an_untrusted_repository() {
    let repo = repo("index-untrusted");
    repo.file("a.txt", b"");
    let tools = untrusting_git();
    let mut shell = LocalShell::sh();
    shell.tool_dir = Some(tools.path().to_owned());

    let index = index(&shell, repo.str());

    assert_eq!(index.git_error.as_deref(), Some("untrusted repository"));
    assert_eq!(index.entries, ["a.txt"]);
}

// Why not every Unix: macOS filesystems refuse to create names that are not UTF-8.
#[cfg(target_os = "linux")]
#[test]
fn index_drops_non_utf8_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = TempDir::new("index-non-utf8");
    dir.file("ok.txt", b"");
    std::fs::write(dir.path().join(OsStr::from_bytes(b"bad-\xff")), b"").unwrap();

    let index = index(&LocalShell::sh(), dir.str());

    assert_eq!(index.entries, ["ok.txt"]);
}

fn read(
    shell: &LocalShell,
    path: &Path,
    revision: Option<&str>,
) -> Result<crate::api::schema::PaneFileContent, (&'static str, String)> {
    ops::read(shell, location(), path.to_str().unwrap(), revision)
}

#[test]
fn read_returns_text_with_a_cksum_revision() {
    for shell in shells() {
        let dir = TempDir::new("read");
        let file = dir.file("note.txt", "héllo\nworld\n".as_bytes());

        let content = read(&shell, &file, None).unwrap();

        assert_eq!(content.document.text, "héllo\nworld\n");
        assert!(!content.document.unchanged);
        assert!(
            content.document.revision.starts_with("cksum:"),
            "{}",
            content.document.revision
        );
        assert_eq!(content.document.path, file.to_str().unwrap());
    }
}

#[test]
fn read_answers_unchanged_without_content_when_the_revision_matches() {
    for shell in shells() {
        let dir = TempDir::new("read-unchanged");
        let file = dir.file("note.txt", b"first");
        let first = read(&shell, &file, None).unwrap();

        let again = read(&shell, &file, Some(&first.document.revision)).unwrap();
        std::fs::write(&file, b"second").unwrap();
        let changed = read(&shell, &file, Some(&first.document.revision)).unwrap();

        assert!(again.document.unchanged);
        assert_eq!(again.document.text, "");
        assert_eq!(again.document.revision, first.document.revision);
        assert!(!changed.document.unchanged);
        assert_eq!(changed.document.text, "second");
        assert_ne!(changed.document.revision, first.document.revision);
    }
}

#[test]
fn read_rejects_files_like_the_host_file_read() {
    let dir = TempDir::new("read-reject");
    let huge = dir.file("huge", &vec![b'a'; 8 * 1024 * 1024 + 1]);
    let binary = dir.file("binary", b"ab\0cd");
    let latin1 = dir.file("latin1", b"caf\xe9");
    let exactly_limit = dir.file("limit", &vec![b'a'; 8 * 1024 * 1024]);
    let directory = dir.dir("directory");
    let missing = dir.path().join("missing");

    let message = |path: &Path| read(&LocalShell::sh(), path, None).unwrap_err();

    assert_eq!(
        message(&huge),
        (
            "file_error",
            "file exceeds the 8 MiB text limit; open in external editor".to_owned()
        )
    );
    assert_eq!(
        message(&binary),
        (
            "file_error",
            "binary file; open in external editor".to_owned()
        )
    );
    assert_eq!(
        message(&latin1),
        (
            "file_error",
            "file is not UTF-8; open in external editor".to_owned()
        )
    );
    assert_eq!(
        message(&directory),
        ("file_error", "not a regular file".to_owned())
    );
    assert_eq!(message(&missing).0, "not_found");
    assert!(read(&LocalShell::sh(), &exactly_limit, None).is_ok());
}

#[test]
fn read_passes_a_hostile_path_as_data() {
    let dir = TempDir::new("read-hostile");
    let file = dir.file("$(touch pwned) '; touch pwned2; '", b"ok");

    let content = read(&LocalShell::sh(), &file, None).unwrap();

    assert_eq!(content.document.text, "ok");
    assert!(!dir.path().join("pwned").exists());
    assert!(!dir.path().join("pwned2").exists());
}

fn which(tool: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{tool} on PATH"))
}

fn executable(dir: &TempDir, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.file(name, script.as_bytes());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `PATH` holding what the scripts use except git.
fn path_without_git() -> TempDir {
    let dir = TempDir::new("no-git-path");
    for tool in [
        "head", "find", "wc", "tr", "cksum", "cat", "printf", "dirname",
    ] {
        if let Ok(path) = std::panic::catch_unwind(|| which(tool)) {
            std::os::unix::fs::symlink(path, dir.path().join(tool)).unwrap();
        }
    }
    dir
}

#[test]
fn output_before_the_scripts_marker_is_ignored() {
    let dir = TempDir::new("banner");
    let file = dir.file("a.txt", b"hello");
    let mut shell = LocalShell::sh();
    shell.banner = Some("Welcome to devbox\nLast login: today\n");

    let listing = list(&shell, None, Some(dir.str())).unwrap();
    let index = ops::index(&shell, location(), None, Some(dir.str()), false).unwrap();
    let content = read(&shell, &file, None).unwrap();

    assert_eq!(names(&listing), ["a.txt"]);
    assert_eq!(index.entries, ["a.txt"]);
    assert_eq!(content.document.text, "hello");
    assert!(ops::probe_editable(&shell, dir.str()).is_err());
}

#[test]
fn a_script_that_never_printed_its_marker_is_reported_not_parsed() {
    struct Silent;
    impl ScriptRunner for Silent {
        fn run(&self, _: &str, _: &[&str]) -> Result<Vec<u8>, ExecError> {
            Ok(b"just a banner\n".to_vec())
        }
    }

    let error = ops::list(&Silent, location(), None, Some("/x"), false).unwrap_err();

    assert_eq!(error.0, "directory_read_failed");
}

#[test]
fn git_stderr_never_leaks_into_the_repository_path() {
    let repo = repo("stderr-noise");
    repo.file("a", b"");
    let tools = TempDir::new("noisy-git");
    executable(
        &tools,
        "git",
        &format!(
            "#!/bin/sh\ncase \"$*\" in *rev-parse*) echo 'hint: noisy advice' >&2 ;; esac\nexec {} \"$@\"\n",
            which("git").display()
        ),
    );
    let mut shell = LocalShell::sh();
    shell.tool_dir = Some(tools.path().to_owned());

    let index = ops::index(&shell, location(), None, Some(repo.str()), false).unwrap();
    let listing = list(&shell, None, Some(repo.str())).unwrap();

    assert_eq!(index.repo_root.as_deref(), Some(repo.str()));
    assert_eq!(index.entries, ["a"]);
    assert_eq!(entry(&listing, "a").ignored, Some(false));
}

#[test]
fn a_repository_rooted_at_the_filesystem_root_still_matches_its_subdirectories() {
    let repo = repo("root-repo");
    repo.file("a", b"");
    let tools = TempDir::new("root-git");
    // Reports `/` as the top level, like a checkout of the whole filesystem,
    // and ignores everything so the marks are observable.
    executable(
        &tools,
        "git",
        "#!/bin/sh\ncase \"$*\" in\n *rev-parse*) echo /;;\n *check-ignore*) cat;;\nesac\n",
    );
    let mut shell = LocalShell::sh();
    shell.tool_dir = Some(tools.path().to_owned());

    let listing = list(&shell, None, Some(repo.str())).unwrap();

    // The prefix was computed (not "outside the repository"), so ignored is set.
    assert_eq!(entry(&listing, "a").ignored, Some(true));
}

#[test]
fn a_missing_git_is_reported_as_not_installed_and_the_listing_still_works() {
    let repo = repo("no-git-installed");
    repo.file("a", b"");
    let path = path_without_git();
    let mut shell = LocalShell::sh();
    shell.only_path = Some(path.path().to_owned());

    let listing = list(&shell, None, Some(repo.str())).unwrap();
    let index = ops::index(&shell, location(), None, Some(repo.str()), false).unwrap();

    assert_eq!(listing.git_error.as_deref(), Some("git not installed"));
    assert_eq!(index.git_error.as_deref(), Some("git not installed"));
    assert!(index.entries.contains(&"a".to_owned()));
}

#[test]
fn the_find_fallback_never_walks_the_filesystem_root_or_kernel_trees() {
    for path in ["/", "/proc", "/sys/kernel", "/dev"] {
        if !Path::new(path).is_dir() {
            continue;
        }

        let index = ops::index(&LocalShell::sh(), location(), None, Some(path), false).unwrap();

        assert!(index.entries.is_empty(), "{path}");
        assert!(index.truncated, "{path}");
        assert_eq!(index.git_error, None, "{path}");
    }
}

#[test]
fn the_find_fallback_runs_under_timeout_one_second_short_of_the_deadline() {
    let dir = TempDir::new("find-timeout");
    dir.file("a.txt", b"");
    let tools = TempDir::new("recording-timeout");
    let log = tools.path().join("timeout.log");
    executable(
        &tools,
        "timeout",
        &format!(
            "#!/bin/sh\nprintf '%s' \"$1\" > {}\nshift\nexec \"$@\"\n",
            log.display()
        ),
    );
    let mut shell = LocalShell::sh();
    shell.tool_dir = Some(tools.path().to_owned());
    shell.seconds_left = Some(8);

    let index = ops::index(&shell, location(), None, Some(dir.str()), false).unwrap();

    assert_eq!(index.entries, ["a.txt"]);
    assert_eq!(std::fs::read_to_string(log).unwrap(), "7");
}

#[test]
fn the_find_fallback_runs_unbounded_when_timeout_is_not_installed() {
    let dir = TempDir::new("find-no-timeout");
    dir.file("a.txt", b"");
    let path = path_without_git();
    let mut shell = LocalShell::sh();
    shell.only_path = Some(path.path().to_owned());
    shell.seconds_left = Some(8);

    let index = ops::index(&shell, location(), None, Some(dir.str()), false).unwrap();

    assert_eq!(index.entries, ["a.txt"]);
}

#[test]
fn the_marker_constant_matches_the_one_the_scripts_print() {
    for script in [
        super::scripts::list_script(),
        super::scripts::index_script(),
        super::scripts::read_script(),
    ] {
        assert!(script.contains(&format!("printf '{}\\0'", super::scripts::OUTPUT_MARKER)));
    }
}

fn running_as_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| output.stdout == b"0\n")
}

#[test]
fn a_searchable_but_unreadable_directory_is_permission_denied_not_empty() {
    use std::os::unix::fs::PermissionsExt;
    if running_as_root() {
        return;
    }
    let dir = TempDir::new("unreadable");
    let locked = dir.dir("locked");
    dir.file("locked/inside.txt", b"");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o111)).unwrap();

    let listing = list(&LocalShell::sh(), None, Some(locked.to_str().unwrap()));
    let indexing = ops::index(
        &LocalShell::sh(),
        location(),
        None,
        Some(locked.to_str().unwrap()),
        false,
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(listing.unwrap_err().0, "permission_denied");
    assert_eq!(indexing.unwrap_err().0, "permission_denied");
}

fn find_index_with(find_body: &str, timeout_body: Option<&str>) -> PaneDirectoryIndexing {
    let dir = TempDir::new("find-status");
    dir.file("a.txt", b"");
    let tools = TempDir::new("fake-find");
    executable(&tools, "find", &format!("#!/bin/sh\n{find_body}\n"));
    if let Some(body) = timeout_body {
        executable(&tools, "timeout", &format!("#!/bin/sh\n{body}\n"));
    }
    let mut shell = LocalShell::sh();
    shell.tool_dir = Some(tools.path().to_owned());
    shell.seconds_left = timeout_body.map(|_| 8);
    index(&shell, dir.str())
}

#[test]
fn a_find_that_exits_nonzero_marks_the_index_truncated() {
    let index = find_index_with("printf './a.txt\\0'\nexit 1", None);

    assert_eq!(index.entries, ["a.txt"]);
    assert!(index.truncated);
}

#[test]
fn a_find_stopped_by_timeout_marks_the_index_truncated() {
    let index = find_index_with("printf './a.txt\\0'", Some("shift\n\"$@\"\nexit 124"));

    assert_eq!(index.entries, ["a.txt"]);
    assert!(index.truncated);
}

#[test]
fn a_find_that_finishes_cleanly_is_not_truncated() {
    let index = find_index_with("printf './a.txt\\0'", None);

    assert_eq!(index.entries, ["a.txt"]);
    assert!(!index.truncated);
}

#[test]
fn a_failing_git_ls_files_marks_the_index_truncated() {
    let repo = repo("ls-files-fails");
    repo.file("tracked.rs", b"");
    git(repo.path(), &["add", "tracked.rs"]);
    let tools = TempDir::new("fake-git");
    // Why delegate: rev-parse must still succeed so the script reaches ls-files.
    let real = which_git();
    executable(
        &tools,
        "git",
        &format!(
            "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = ls-files ] && {{ printf 'x\\0'; exit 1; }}; done\nexec {} \"$@\"\n",
            real.display()
        ),
    );
    let mut shell = LocalShell::sh();
    shell.tool_dir = Some(tools.path().to_owned());

    let index = index(&shell, repo.str());

    assert!(index.truncated);
}
