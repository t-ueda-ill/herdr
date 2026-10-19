//! The Git CLI calls behind `directory.list` and `directory.index`.
//!
//! Every call runs from the work tree's top level so the same path serves as
//! `-C` and as the `safe.directory` value when the caller trusts the repository.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::noninteractive_process::{run_with_deadline, ProcessOutput, RunError};

/// Why a Git call produced no usable answer. The message is shown to users as is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GitError {
    pub(super) message: String,
    pub(super) timed_out: bool,
}

impl GitError {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timed_out: false,
        }
    }
}

pub(super) struct GitCli<'a> {
    pub(super) program: &'a Path,
    pub(super) trust_repository: bool,
    pub(super) deadline: Instant,
}

impl GitCli<'_> {
    fn run(
        &self,
        repo_root: &Path,
        args: &[&str],
        stdin: Option<Vec<u8>>,
        accepted_status: &[i32],
    ) -> Result<ProcessOutput, GitError> {
        let mut argv = crate::worktree::repository_git_args(repo_root, self.trust_repository);
        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
        let output = run_with_deadline(self.program, &argv, stdin, self.deadline).map_err(
            |err| match err {
                RunError::NotFound => GitError::failed("git is not installed"),
                RunError::Timeout => GitError {
                    message: "git timed out".into(),
                    timed_out: true,
                },
                RunError::Failed { detail, .. } => GitError::failed(detail),
            },
        )?;
        match output.status {
            Some(status) if accepted_status.contains(&status) => Ok(output),
            status => {
                let stderr = output.stderr.trim();
                Err(GitError::failed(if stderr.is_empty() {
                    match status {
                        Some(status) => format!("git exited with status {status}"),
                        None => "git was terminated by a signal".to_owned(),
                    }
                } else {
                    stderr.to_owned()
                }))
            }
        }
    }

    /// Returns which of `paths` (relative to `repo_root`) Git ignores.
    pub(super) fn ignored(
        &self,
        repo_root: &Path,
        paths: &[String],
    ) -> Result<HashSet<String>, GitError> {
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let mut stdin = Vec::new();
        for path in paths {
            stdin.extend_from_slice(path.as_bytes());
            stdin.push(0);
        }
        // Exit status 1 is check-ignore's "nothing matched", not a failure.
        let output = self.run(
            repo_root,
            &["check-ignore", "-z", "--stdin"],
            Some(stdin),
            &[0, 1],
        )?;
        Ok(split_nul(&output.stdout).map(str::to_owned).collect())
    }

    pub(super) fn toplevel(&self, repo_root: &Path) -> Result<PathBuf, GitError> {
        let output = self.run(repo_root, &["rev-parse", "--show-toplevel"], None, &[0])?;
        let toplevel = output.stdout.trim_end_matches(['\n', '\r']);
        if toplevel.is_empty() {
            return Err(GitError::failed("git did not report a work tree"));
        }
        Ok(PathBuf::from(toplevel))
    }

    /// Tracked files without submodule gitlinks, plus untracked files that are
    /// not ignored, without the contents of nested repositories.
    pub(super) fn files(&self, repo_root: &Path) -> Result<Vec<String>, GitError> {
        // Why `--stage` on a separate call: only the staged form carries the
        // mode that tells a submodule gitlink (160000) from a file, and mixing
        // it with `--others` makes untracked names ambiguous.
        let cached = self.run(
            repo_root,
            &["ls-files", "-z", "--stage", "--cached"],
            None,
            &[0],
        )?;
        let others = self.run(
            repo_root,
            &["ls-files", "-z", "--others", "--exclude-standard"],
            None,
            &[0],
        )?;
        let mut files: Vec<String> = split_nul(&cached.stdout)
            .filter_map(staged_file_path)
            .chain(
                // Nested repositories are reported as `dir/`, never their files.
                split_nul(&others.stdout).filter(|path| !path.ends_with('/')),
            )
            // The output was decoded lossily, so U+FFFD marks a path that is
            // not valid UTF-8 and cannot be named over the API. Why not keep
            // the bytes: that decoding is shared with every CLI call; a real
            // U+FFFD in a name only drops that file from search.
            .filter(|path| !path.contains(char::REPLACEMENT_CHARACTER))
            .map(str::to_owned)
            .collect();
        files.sort_unstable();
        // Unmerged paths appear once per conflict stage.
        files.dedup();
        Ok(files)
    }
}

fn split_nul(output: &str) -> impl Iterator<Item = &str> {
    output.split('\0').filter(|record| !record.is_empty())
}

/// `<mode> <object> <stage>\t<path>`; gitlinks are submodules, which the index excludes.
fn staged_file_path(record: &str) -> Option<&str> {
    let (meta, path) = record.split_once('\t')?;
    (meta.split(' ').next() != Some("160000")).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_records_drop_submodule_gitlinks() {
        assert_eq!(
            staged_file_path("100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\tsrc/a b.rs"),
            Some("src/a b.rs")
        );
        assert_eq!(
            staged_file_path("160000 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\tvendor/lib"),
            None
        );
        assert_eq!(staged_file_path("malformed"), None);
    }
}
