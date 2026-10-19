//! Filesystem side of `directory.list` and `directory.index`. Nothing here
//! touches `App`, so it runs on a worker thread and is tested without PTYs.

use std::path::{Path, PathBuf};

use crate::api::schema::{DirectoryEntry, DirectoryEntryKind};

use super::git::GitCli;

pub(super) const MAX_LIST_ENTRIES: usize = 2_000;
pub(super) const MAX_INDEX_ENTRIES: usize = 50_000;

/// API error code and message, as `encode_error` takes them.
pub(in crate::app::api) type Failure = (&'static str, String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Listing {
    pub(super) canonical: PathBuf,
    pub(super) entries: Vec<DirectoryEntry>,
    pub(super) truncated: bool,
    /// Entries past the limit, zero unless `truncated`.
    pub(super) omitted: usize,
    /// Entries skipped because their names are not valid UTF-8.
    pub(super) unlistable: usize,
    pub(super) git_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Index {
    pub(super) repo_root: Option<PathBuf>,
    pub(super) entries: Vec<String>,
    pub(super) truncated: bool,
    pub(super) git_error: Option<String>,
}

/// Accepts only absolute paths: the server cannot know which working
/// directory a relative path was meant against.
pub(in crate::app::api) fn requested_path(path: &str) -> Result<PathBuf, Failure> {
    let requested = PathBuf::from(path);
    if path.is_empty() || !requested.is_absolute() {
        return Err(("invalid_params", format!("path must be absolute: {path:?}")));
    }
    Ok(requested)
}

pub(super) fn list_directory(
    path: &Path,
    git: &GitCli<'_>,
    max_entries: usize,
) -> Result<Listing, Failure> {
    let canonical = canonical_directory(path)?;
    let read = std::fs::read_dir(&canonical).map_err(|err| io_failure(&canonical, &err))?;
    let mut candidates = Vec::new();
    let mut unlistable = 0;
    for entry in read.filter_map(Result::ok) {
        if entry.file_name() == ".git" {
            continue;
        }
        // Why not a lossy name: the client joins names into the paths it
        // sends and opens, and a replaced byte names a file that does not exist.
        match Candidate::new(&entry) {
            Some(candidate) => candidates.push(candidate),
            None => unlistable += 1,
        }
    }
    // Directories (and links to them) first, then case-insensitive names with
    // the exact name as a tie-break so the order is stable across calls.
    candidates.sort_by_cached_key(|candidate| {
        (
            !candidate.directory_like(),
            candidate.name.to_lowercase(),
            candidate.name.clone(),
        )
    });
    let omitted = candidates.len().saturating_sub(max_entries);
    candidates.truncate(max_entries);

    let (ignored, git_error) = match classify_ignored(&canonical, &candidates, git) {
        Ok(ignored) => (ignored, None),
        Err(error) => (None, Some(error)),
    };
    let entries = candidates
        .into_iter()
        .map(|candidate| {
            let ignored = ignored
                .as_ref()
                .map(|ignored| ignored.contains(&candidate.name));
            candidate.into_entry(ignored)
        })
        .collect();
    Ok(Listing {
        canonical,
        entries,
        truncated: omitted > 0,
        omitted,
        unlistable,
        git_error,
    })
}

pub(super) fn index_repository(
    path: &Path,
    git: &GitCli<'_>,
    max_entries: usize,
) -> Result<Index, Failure> {
    let canonical = std::fs::canonicalize(path).map_err(|err| io_failure(path, &err))?;
    let Some(candidate_root) = repository_root(&canonical) else {
        return Ok(Index {
            repo_root: None,
            entries: Vec::new(),
            truncated: false,
            git_error: None,
        });
    };
    let files = git
        .toplevel(&candidate_root)
        .and_then(|toplevel| Ok((git.files(&toplevel)?, toplevel)));
    match files {
        Ok((mut entries, toplevel)) => {
            let truncated = entries.len() > max_entries;
            entries.truncate(max_entries);
            Ok(Index {
                repo_root: Some(toplevel),
                entries,
                truncated,
                git_error: None,
            })
        }
        // Degrade to a non-Git index; a timeout also reads as truncated so the
        // client knows the repository holds files it was not given.
        Err(error) => Ok(Index {
            repo_root: None,
            entries: Vec::new(),
            truncated: error.timed_out,
            git_error: Some(error.message),
        }),
    }
}

/// Top level of the work tree containing `canonical`, found without Git so a
/// plain directory costs no process spawn.
fn repository_root(canonical: &Path) -> Option<PathBuf> {
    crate::workspace::git_space_metadata(canonical).map(|space| space.repo_root)
}

/// `Some(names)` of ignored entries, `None` outside a work tree, `Err` when Git failed.
fn classify_ignored(
    canonical: &Path,
    candidates: &[Candidate],
    git: &GitCli<'_>,
) -> Result<Option<std::collections::HashSet<String>>, String> {
    let Some(repo_root) = repository_root(canonical) else {
        return Ok(None);
    };
    let Ok(relative_dir) = canonical.strip_prefix(&repo_root) else {
        return Ok(None);
    };
    // Why not ask Git about entries under `.git`: check-ignore rejects them.
    if relative_dir
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return Ok(None);
    }
    let prefix = relative_dir.to_string_lossy();
    let relative = |name: &str| {
        if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        }
    };
    let paths: Vec<String> = candidates
        .iter()
        .map(|candidate| relative(&candidate.name))
        .collect();
    let ignored = git
        .ignored(&repo_root, &paths)
        .map_err(|error| error.message)?;
    Ok(Some(
        candidates
            .iter()
            .filter(|candidate| ignored.contains(&relative(&candidate.name)))
            .map(|candidate| candidate.name.clone())
            .collect(),
    ))
}

fn canonical_directory(path: &Path) -> Result<PathBuf, Failure> {
    let canonical = std::fs::canonicalize(path).map_err(|err| io_failure(path, &err))?;
    if !canonical.is_dir() {
        return Err((
            "not_a_directory",
            format!("not a directory: {}", path.display()),
        ));
    }
    Ok(canonical)
}

fn io_failure(path: &Path, err: &std::io::Error) -> Failure {
    match err.kind() {
        std::io::ErrorKind::NotFound => ("not_found", format!("not found: {}", path.display())),
        std::io::ErrorKind::PermissionDenied => (
            "permission_denied",
            format!("permission denied: {}", path.display()),
        ),
        _ => (
            "directory_read_failed",
            format!("{}: {err}", path.display()),
        ),
    }
}

/// An entry kept only as far as sorting needs; the rest is read after truncation.
struct Candidate {
    name: String,
    kind: DirectoryEntryKind,
    target_kind: Option<DirectoryEntryKind>,
    entry_path: PathBuf,
}

impl Candidate {
    /// `None` when the name is not valid UTF-8.
    fn new(entry: &std::fs::DirEntry) -> Option<Self> {
        let name = entry.file_name().into_string().ok()?;
        let entry_path = entry.path();
        let kind = entry
            .file_type()
            .map(kind_of)
            .unwrap_or(DirectoryEntryKind::Other);
        // Followed only for links, so links to directories sort and expand like
        // directories; a dangling link has no target kind.
        let target_kind = (kind == DirectoryEntryKind::Symlink)
            .then(|| std::fs::metadata(&entry_path).ok())
            .flatten()
            .map(|metadata| kind_of(metadata.file_type()));
        Some(Self {
            name,
            kind,
            target_kind,
            entry_path,
        })
    }

    fn directory_like(&self) -> bool {
        self.kind == DirectoryEntryKind::Dir || self.target_kind == Some(DirectoryEntryKind::Dir)
    }

    fn into_entry(self, ignored: Option<bool>) -> DirectoryEntry {
        let metadata = std::fs::symlink_metadata(&self.entry_path).ok();
        let size = metadata
            .as_ref()
            .filter(|_| self.kind == DirectoryEntryKind::File)
            .map(std::fs::Metadata::len);
        let mtime = metadata
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok());
        DirectoryEntry {
            hidden: self.name.starts_with('.'),
            name: self.name,
            kind: self.kind,
            target_kind: self.target_kind,
            size,
            mtime,
            ignored,
        }
    }
}

fn kind_of(file_type: std::fs::FileType) -> DirectoryEntryKind {
    if file_type.is_symlink() {
        DirectoryEntryKind::Symlink
    } else if file_type.is_dir() {
        DirectoryEntryKind::Dir
    } else if file_type.is_file() {
        DirectoryEntryKind::File
    } else {
        DirectoryEntryKind::Other
    }
}

// The tests drive Git and Unix absolute paths.
#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "herdr-directory-{name}-{}-{stamp}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            // Canonical so expectations match on macOS, where /tmp is a symlink.
            Self(std::fs::canonicalize(&path).unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn file(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, relative).unwrap();
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

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
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
            status.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn repo(name: &str) -> TempDir {
        let repo = TempDir::new(name);
        git(repo.path(), &["init", "--quiet"]);
        repo
    }

    fn real_git() -> GitCli<'static> {
        GitCli {
            program: Path::new("git"),
            trust_repository: false,
            deadline: Instant::now() + Duration::from_secs(10),
        }
    }

    fn names(listing: &Listing) -> Vec<&str> {
        listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    fn entry<'a>(listing: &'a Listing, name: &str) -> &'a DirectoryEntry {
        listing
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("missing entry {name}: {listing:?}"))
    }

    #[test]
    fn requested_path_rejects_relative_and_parent_relative_paths() {
        for path in ["", "src", "../etc", "./a/../b", "~/repo"] {
            let (code, _) = requested_path(path).unwrap_err();
            assert_eq!(code, "invalid_params", "{path:?}");
        }
        assert_eq!(
            requested_path("/srv/app/../lib").unwrap(),
            PathBuf::from("/srv/app/../lib")
        );
    }

    #[test]
    fn listing_puts_directories_first_then_sorts_by_name() {
        let dir = TempDir::new("order");
        dir.file("b.txt");
        dir.file("A.txt");
        dir.dir("zeta");
        dir.dir("alpha");

        let listing = list_directory(dir.path(), &real_git(), MAX_LIST_ENTRIES).unwrap();

        assert_eq!(names(&listing), ["alpha", "zeta", "A.txt", "b.txt"]);
        assert_eq!(listing.canonical, dir.path());
        assert!(!listing.truncated);
        assert_eq!(entry(&listing, "alpha").kind, DirectoryEntryKind::Dir);
        let file = entry(&listing, "b.txt");
        assert_eq!(file.kind, DirectoryEntryKind::File);
        assert_eq!(file.size, Some(5));
        assert!(file.mtime.is_some_and(|mtime| mtime > 0));
    }

    #[test]
    fn listing_never_returns_dot_git_but_flags_other_hidden_entries() {
        let dir = repo("hidden");
        dir.file(".env");
        dir.file("visible");

        let listing = list_directory(dir.path(), &real_git(), MAX_LIST_ENTRIES).unwrap();

        assert_eq!(names(&listing), [".env", "visible"]);
        assert!(entry(&listing, ".env").hidden);
        assert!(!entry(&listing, "visible").hidden);
    }

    #[test]
    fn listing_truncates_after_the_limit_counting_hidden_and_ignored_entries() {
        let dir = repo("truncate");
        std::fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();
        dir.file("a.log");
        dir.file("b.txt");
        dir.dir("sub");

        let listing = list_directory(dir.path(), &real_git(), 3).unwrap();

        assert_eq!(names(&listing), ["sub", ".gitignore", "a.log"]);
        assert!(listing.truncated);
        assert_eq!(listing.omitted, 1, "b.txt was left out");
        let exact = list_directory(dir.path(), &real_git(), 4).unwrap();
        assert_eq!(exact.entries.len(), 4);
        assert!(!exact.truncated);
        assert_eq!(exact.omitted, 0);
    }

    #[test]
    fn listing_marks_git_ignored_entries() {
        let dir = repo("ignored");
        std::fs::write(dir.path().join(".gitignore"), "target/\n*.log\n").unwrap();
        dir.file("target/debug/app");
        dir.file("build.log");
        dir.file("src/main.rs");

        let listing = list_directory(dir.path(), &real_git(), MAX_LIST_ENTRIES).unwrap();

        assert_eq!(listing.git_error, None);
        assert_eq!(entry(&listing, "target").ignored, Some(true));
        assert_eq!(entry(&listing, "build.log").ignored, Some(true));
        assert_eq!(entry(&listing, "src").ignored, Some(false));
        assert_eq!(entry(&listing, ".gitignore").ignored, Some(false));

        let nested = list_directory(
            &dir.path().join("target/debug"),
            &real_git(),
            MAX_LIST_ENTRIES,
        )
        .unwrap();
        assert_eq!(entry(&nested, "app").ignored, Some(true));
    }

    #[test]
    fn listing_outside_git_leaves_ignored_unset_without_error() {
        let dir = TempDir::new("plain");
        dir.file("notes.md");

        let missing_git = GitCli {
            program: Path::new("/nonexistent/herdr-git"),
            ..real_git()
        };
        let listing = list_directory(dir.path(), &missing_git, MAX_LIST_ENTRIES).unwrap();

        assert_eq!(listing.git_error, None);
        assert_eq!(entry(&listing, "notes.md").ignored, None);
    }

    #[test]
    fn listing_degrades_to_non_git_when_git_is_missing() {
        let dir = repo("git-missing");
        dir.file("main.rs");

        let missing_git = GitCli {
            program: Path::new("/nonexistent/herdr-git"),
            ..real_git()
        };
        let listing = list_directory(dir.path(), &missing_git, MAX_LIST_ENTRIES).unwrap();

        assert_eq!(listing.git_error.as_deref(), Some("git is not installed"));
        assert_eq!(names(&listing), ["main.rs"]);
        assert_eq!(entry(&listing, "main.rs").ignored, None);
    }

    #[test]
    fn listing_rejects_missing_paths_and_files() {
        let dir = TempDir::new("missing");
        let file = dir.file("file.txt");

        let (code, _) =
            list_directory(&dir.path().join("absent"), &real_git(), MAX_LIST_ENTRIES).unwrap_err();
        assert_eq!(code, "not_found");
        let (code, _) = list_directory(&file, &real_git(), MAX_LIST_ENTRIES).unwrap_err();
        assert_eq!(code, "not_a_directory");
    }

    #[test]
    fn index_lists_tracked_and_untracked_files_relative_to_the_repo_root() {
        let dir = repo("index");
        std::fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();
        dir.file("src/main.rs");
        dir.file("README.md");
        git(dir.path(), &["add", ".gitignore", "src/main.rs"]);
        git(dir.path(), &["commit", "--quiet", "-m", "init"]);
        dir.file("debug.log");

        let index =
            index_repository(&dir.path().join("src"), &real_git(), MAX_INDEX_ENTRIES).unwrap();

        assert_eq!(index.repo_root.as_deref(), Some(dir.path()));
        assert_eq!(index.entries, [".gitignore", "README.md", "src/main.rs"]);
        assert!(!index.truncated);
        assert_eq!(index.git_error, None);
    }

    #[test]
    fn index_truncates_after_the_limit() {
        let dir = repo("index-truncate");
        for name in ["a", "b", "c"] {
            dir.file(name);
        }

        let index = index_repository(dir.path(), &real_git(), 2).unwrap();

        assert_eq!(index.entries, ["a", "b"]);
        assert!(index.truncated);
    }

    #[test]
    fn index_excludes_nested_repositories_and_submodules() {
        let dir = repo("index-nested");
        dir.file("top.txt");
        let nested = dir.dir("nested");
        git(&nested, &["init", "--quiet"]);
        dir.file("nested/inner.txt");
        git(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,e69de29bb2d1d6434b8b29ae775ad8c2e48c5391,vendor/lib",
            ],
        );

        let index = index_repository(dir.path(), &real_git(), MAX_INDEX_ENTRIES).unwrap();

        assert_eq!(index.entries, ["top.txt"]);
    }

    #[test]
    fn index_outside_git_is_empty_without_error() {
        let dir = TempDir::new("index-plain");
        dir.file("notes.md");

        let index = index_repository(dir.path(), &real_git(), MAX_INDEX_ENTRIES).unwrap();

        assert_eq!(
            index,
            Index {
                repo_root: None,
                entries: Vec::new(),
                truncated: false,
                git_error: None,
            }
        );
    }

    #[test]
    fn index_degrades_to_non_git_when_git_is_missing() {
        let dir = repo("index-git-missing");
        dir.file("main.rs");

        let missing_git = GitCli {
            program: Path::new("/nonexistent/herdr-git"),
            ..real_git()
        };
        let index = index_repository(dir.path(), &missing_git, MAX_INDEX_ENTRIES).unwrap();

        assert_eq!(index.repo_root, None);
        assert!(index.entries.is_empty());
        assert!(!index.truncated);
        assert_eq!(index.git_error.as_deref(), Some("git is not installed"));
    }

    #[test]
    fn index_rejects_missing_paths() {
        let dir = TempDir::new("index-missing");

        let (code, _) =
            index_repository(&dir.path().join("absent"), &real_git(), MAX_INDEX_ENTRIES)
                .unwrap_err();

        assert_eq!(code, "not_found");
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

        use super::*;
        use crate::noninteractive_process::fake_program::FakeProgram;

        #[test]
        fn listing_reports_symlinks_with_their_target_kind_and_canonical_path() {
            let dir = TempDir::new("symlink");
            let target = dir.dir("real");
            dir.file("real/file.txt");
            symlink(&target, dir.path().join("link-dir")).unwrap();
            symlink(
                dir.path().join("real/file.txt"),
                dir.path().join("link-file"),
            )
            .unwrap();
            symlink(dir.path().join("gone"), dir.path().join("dangling")).unwrap();
            // A link back to an ancestor: the client stops expanding it by
            // comparing canonical paths.
            symlink(dir.path(), dir.path().join("real/loop")).unwrap();

            let listing = list_directory(dir.path(), &real_git(), MAX_LIST_ENTRIES).unwrap();

            assert_eq!(
                names(&listing),
                ["link-dir", "real", "dangling", "link-file"]
            );
            let link_dir = entry(&listing, "link-dir");
            assert_eq!(link_dir.kind, DirectoryEntryKind::Symlink);
            assert_eq!(link_dir.target_kind, Some(DirectoryEntryKind::Dir));
            assert_eq!(
                entry(&listing, "link-file").target_kind,
                Some(DirectoryEntryKind::File)
            );
            assert_eq!(entry(&listing, "dangling").target_kind, None);
            assert_eq!(entry(&listing, "real").target_kind, None);

            let through_link =
                list_directory(&dir.path().join("link-dir"), &real_git(), MAX_LIST_ENTRIES)
                    .unwrap();
            assert_eq!(through_link.canonical, target);
            assert_eq!(names(&through_link), ["loop", "file.txt"]);
            let looped =
                list_directory(&dir.path().join("real/loop"), &real_git(), MAX_LIST_ENTRIES)
                    .unwrap();
            assert_eq!(looped.canonical, dir.path());
        }

        #[test]
        fn listing_reports_permission_denied() {
            let dir = TempDir::new("denied");
            // Root (as in some CI containers) ignores permission bits, so
            // the case cannot be produced there.
            if std::fs::metadata(dir.file("owner-probe")).unwrap().uid() == 0 {
                eprintln!("skipped: running as root, which can read any directory");
                return;
            }
            let locked = dir.dir("locked");
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

            let result = list_directory(&locked, &real_git(), MAX_LIST_ENTRIES);

            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(result.unwrap_err().0, "permission_denied");
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn listing_leaves_out_names_that_are_not_utf8_and_counts_them() {
            use std::os::unix::ffi::OsStrExt;

            let dir = repo("non-utf8");
            dir.file("cafe.txt");
            std::fs::write(
                dir.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9.txt")),
                "latin-1",
            )
            .unwrap();

            let listing = list_directory(dir.path(), &real_git(), MAX_LIST_ENTRIES).unwrap();
            let index = index_repository(dir.path(), &real_git(), MAX_INDEX_ENTRIES).unwrap();

            assert_eq!(names(&listing), ["cafe.txt"]);
            assert_eq!(listing.unlistable, 1);
            assert_eq!(entry(&listing, "cafe.txt").ignored, Some(false));
            assert_eq!(index.entries, ["cafe.txt"]);
        }

        #[test]
        fn listing_degrades_to_non_git_when_git_refuses_the_repository() {
            let dir = repo("dubious");
            dir.file("main.rs");
            let fake = FakeProgram::new(
                "git",
                "directory-dubious",
                "echo \"fatal: detected dubious ownership in repository at '$PWD'\" >&2; exit 128",
            );
            let program = fake.program();
            let options = GitCli {
                program: &program,
                ..real_git()
            };

            let listing = list_directory(dir.path(), &options, MAX_LIST_ENTRIES).unwrap();

            assert!(listing
                .git_error
                .as_deref()
                .is_some_and(|error| error.contains("dubious ownership")));
            assert_eq!(entry(&listing, "main.rs").ignored, None);
        }

        #[test]
        fn trusted_listing_and_index_pass_safe_directory_to_git() {
            let dir = repo("trusted");
            dir.file("main.rs");
            let args_file = dir.path().join(".git/herdr-args");
            let fake = FakeProgram::new(
                "git",
                "directory-trusted",
                &format!(
                    "printf '%s\\n' \"$*\" >> '{}'; case \"$*\" in *rev-parse*) printf '%s\\n' '{}';; esac; exit 0",
                    args_file.display(),
                    dir.path().display()
                ),
            );
            let program = fake.program();
            let options = GitCli {
                program: &program,
                trust_repository: true,
                ..real_git()
            };

            list_directory(dir.path(), &options, MAX_LIST_ENTRIES).unwrap();
            index_repository(dir.path(), &options, MAX_INDEX_ENTRIES).unwrap();

            let args = std::fs::read_to_string(&args_file).unwrap();
            let safe_directory = format!("safe.directory={}", dir.path().display());
            let invocations: Vec<&str> = args.lines().collect();
            assert!(
                invocations
                    .iter()
                    .all(|invocation| invocation.contains(&safe_directory)),
                "{args}"
            );
            for command in ["check-ignore", "rev-parse", "ls-files"] {
                assert!(
                    invocations
                        .iter()
                        .any(|invocation| invocation.contains(command)),
                    "{command} was not run: {args}"
                );
            }

            let untrusted = GitCli {
                program: &program,
                ..real_git()
            };
            std::fs::remove_file(&args_file).unwrap();
            list_directory(dir.path(), &untrusted, MAX_LIST_ENTRIES).unwrap();
            assert!(!std::fs::read_to_string(&args_file)
                .unwrap()
                .contains("safe.directory"));
        }

        #[test]
        fn index_reports_a_git_timeout_as_truncated_with_git_error() {
            let dir = repo("index-timeout");
            dir.file("main.rs");
            let fake = FakeProgram::new("git", "directory-timeout", "sleep 5");
            let program = fake.program();
            let options = GitCli {
                program: &program,
                trust_repository: false,
                deadline: Instant::now() + Duration::from_millis(100),
            };
            let started = Instant::now();

            let index = index_repository(dir.path(), &options, MAX_INDEX_ENTRIES).unwrap();

            assert!(started.elapsed() < Duration::from_secs(3));
            assert!(index.truncated);
            assert!(index.entries.is_empty());
            assert_eq!(index.git_error.as_deref(), Some("git timed out"));
        }

        #[test]
        fn index_does_not_follow_symlinked_directories() {
            let dir = repo("index-symlink");
            let outside = TempDir::new("index-symlink-outside");
            outside.file("secret.txt");
            dir.file("main.rs");
            symlink(outside.path(), dir.path().join("linked")).unwrap();

            let index = index_repository(dir.path(), &real_git(), MAX_INDEX_ENTRIES).unwrap();

            assert_eq!(index.entries, ["linked", "main.rs"]);
        }
    }
}
