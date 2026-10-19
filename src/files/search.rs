use super::*;
use regex::{Regex, RegexBuilder};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub(super) fn pattern(query: &FilesSearchParams) -> Result<Regex> {
    let source = if query.regex {
        query.query.clone()
    } else {
        regex::escape(&query.query)
    };
    let source = if query.whole_word {
        format!(r"\b(?:{source})\b")
    } else {
        source
    };
    RegexBuilder::new(&source)
        .case_insensitive(!query.case_sensitive)
        .size_limit(2 * 1024 * 1024)
        .build()
        .map_err(failure)
}

/// Glob patterns are comma/newline separated; **/ also matches the root directory.
fn glob(source: &str) -> Result<Regex> {
    let mut target = String::from("^");
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    target.push_str("(?:.*/)?");
                } else {
                    target.push_str(".*");
                }
            }
            '*' => target.push_str("[^/]*"),
            '?' => target.push_str("[^/]"),
            '[' => {
                target.push('[');
                if chars.peek() == Some(&'!') {
                    chars.next();
                    target.push('^');
                }
                let mut closed = false;
                for c in chars.by_ref() {
                    target.push(c);
                    if c == ']' {
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    return Err(failure("unclosed glob character class"));
                }
            }
            _ => target.push_str(&regex::escape(&ch.to_string())),
        }
    }
    target.push('$');
    Regex::new(&target).map_err(failure)
}
fn globs(source: &str) -> Result<Vec<Regex>> {
    source
        .split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(glob)
        .collect()
}

pub(crate) fn search(params: FilesSearchParams, latest: &AtomicU64) -> Result<ResponseResult> {
    let root = absolute(&params.root)?;
    if !root.is_dir() {
        return Err(failure("search root is not a directory"));
    }
    let query = pattern(&params)?;
    let include = globs(&params.include)?;
    let exclude = globs(&params.exclude)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut paths = Vec::new();
    let mut warnings = Vec::new();
    let mut pending = vec![root.clone()];
    // Git supplies tracked and nonignored files in one bounded subprocess. A Git
    // failure is visible, rather than silently widening a repository search.
    if !params.include_ignored {
        let args = vec![
            "-C".to_owned(),
            params.root.clone(),
            "rev-parse".into(),
            "--show-toplevel".into(),
        ];
        let repository = crate::noninteractive_process::run_with_deadline(
            Path::new("git"),
            &args,
            None,
            deadline,
        );
        if root
            .ancestors()
            .any(|p| p.join(".git").is_file() || p.join(".git/HEAD").is_file())
            && !repository.as_ref().is_ok_and(|o| o.status == Some(0))
        {
            return Err(failure("Git could not determine ignored files; enable ignored files explicitly or repair the repository"));
        }
        if repository.as_ref().is_ok_and(|o| o.status == Some(0)) {
            let args = vec![
                "-C".to_owned(),
                params.root.clone(),
                "ls-files".into(),
                "--cached".into(),
                "--others".into(),
                "--exclude-standard".into(),
                "-z".into(),
            ];
            let output = crate::noninteractive_process::run_with_deadline(
                Path::new("git"),
                &args,
                None,
                deadline,
            )
            .map_err(|e| failure(format!("Git index failed: {e:?}")))?;
            if output.status != Some(0) {
                return Err(failure(output.stderr));
            }
            paths.extend(
                output
                    .stdout
                    .split('\0')
                    .filter(|s| !s.is_empty())
                    .map(|s| root.join(s)),
            );
            pending.clear();
        }
    }
    let mut truncated = false;
    while let Some(directory) = pending.pop() {
        if latest.load(Ordering::Relaxed) != params.generation {
            break;
        }
        if Instant::now() > deadline || paths.len() >= 50_000 {
            truncated = true;
            break;
        }
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            warnings.push(e.to_string());
                            continue;
                        }
                    };
                    if entry.file_name() == ".git" {
                        continue;
                    }
                    let kind = match entry.file_type() {
                        Ok(k) => k,
                        Err(e) => {
                            warnings.push(e.to_string());
                            continue;
                        }
                    };
                    if kind.is_dir() {
                        pending.push(entry.path());
                    } else if kind.is_file() {
                        paths.push(entry.path());
                    }
                    if paths.len() + pending.len() >= 50_000 {
                        truncated = true;
                        break;
                    }
                }
            }
            Err(error) => warnings.push(format!("{}: {error}", directory.display())),
        }
    }
    paths.sort();
    paths.dedup();
    let mut matches = Vec::new();
    if !params.query.is_empty() {
        'files: for path in paths.into_iter().take(50_000) {
            if latest.load(Ordering::Relaxed) != params.generation {
                break;
            }
            if Instant::now() > deadline {
                truncated = true;
                break;
            }
            let relative = path
                .strip_prefix(&root)
                .map_err(failure)?
                .to_string_lossy()
                .replace('\\', "/");
            if relative.split('/').any(|s| s == ".git")
                || !include.is_empty() && !include.iter().any(|g| g.is_match(&relative))
                || exclude.iter().any(|g| g.is_match(&relative))
            {
                continue;
            }
            // Inspect every component, not just the leaf: a symlink directory
            // must not turn Git's path index into traversal outside the root.
            let mut component = root.clone();
            let linked = path
                .strip_prefix(&root)
                .map_err(failure)?
                .components()
                .any(|part| {
                    component.push(part);
                    fs::symlink_metadata(&component).is_ok_and(|m| m.file_type().is_symlink())
                });
            if linked {
                continue;
            }
            let text = match read(&path) {
                Ok(t) => t,
                Err(e) => {
                    if warnings.len() < 100 {
                        warnings.push(format!("{relative}: {}", e.1));
                    }
                    continue;
                }
            };
            let version = revision(text.as_bytes());
            let line_starts: Vec<_> = std::iter::once(0)
                .chain(text.match_indices('\n').map(|(i, _)| i + 1))
                .collect();
            for found in query.find_iter(&text) {
                if latest.load(Ordering::Relaxed) != params.generation {
                    break;
                }
                let line_number = line_starts.partition_point(|offset| *offset <= found.start());
                let line_start = line_starts[line_number.saturating_sub(1)];
                let line_end = line_starts
                    .get(line_number)
                    .map_or(text.len(), |i| i.saturating_sub(1));
                matches.push(FileMatch {
                    path: crate::platform::file_path_display(&path),
                    revision: version.clone(),
                    start: found.start(),
                    end: found.end(),
                    line: line_number,
                    text: text[line_start..line_end]
                        .trim_end_matches('\r')
                        .chars()
                        .take(400)
                        .collect(),
                    matched: found.as_str().into(),
                    replacement: None,
                });
                if matches.len() >= MAX_MATCHES {
                    truncated = true;
                    break 'files;
                }
            }
        }
    }
    warnings.truncate(100);
    Ok(ResponseResult::FileSearch {
        matches,
        truncated,
        cancelled: latest.load(Ordering::Relaxed) != params.generation,
        warnings,
    })
}
