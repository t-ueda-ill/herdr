use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SshHostInfo {
    pub(crate) alias: String,
    pub(crate) hostname: Option<String>,
    pub(crate) user: Option<String>,
}

/// File access behind the parser so tests can use in-memory files.
pub(crate) trait SshConfigLoader {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    /// Names (not full paths) of the entries directly inside `dir`.
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>>;
}

/// Reads the real filesystem.
pub(crate) struct FsConfigLoader;

impl SshConfigLoader for FsConfigLoader {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            // Non-UTF-8 names cannot match a UTF-8 glob pattern anyway.
            if let Ok(name) = entry?.file_name().into_string() {
                names.push(name);
            }
        }
        Ok(names)
    }
}

/// OpenSSH's own limit (READCONF_MAX_DEPTH).
pub(crate) const MAX_INCLUDE_DEPTH: usize = 16;

/// Lists literal `Host` aliases of `root`, following `Include` directives.
///
/// A missing root yields an empty list; any other root read error is returned.
/// Missing or unreadable included files are skipped like OpenSSH does.
pub(crate) fn list_host_aliases(
    loader: &dyn SshConfigLoader,
    root: &Path,
    ssh_dir: &Path,
    home: Option<&Path>,
) -> io::Result<Vec<SshHostInfo>> {
    let text = match loader.read_to_string(root) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut parser = Parser {
        loader,
        ssh_dir,
        home,
        hosts: Vec::new(),
    };
    parser.parse_text(&text, 0, Vec::new());
    Ok(parser.hosts)
}

struct Parser<'a> {
    loader: &'a dyn SshConfigLoader,
    ssh_dir: &'a Path,
    home: Option<&'a Path>,
    hosts: Vec<SshHostInfo>,
}

impl Parser<'_> {
    /// `block` is passed by value so a `Host` line inside an include cannot leak to the includer.
    fn parse_text(&mut self, text: &str, depth: usize, mut block: Vec<String>) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (keyword, args) = split_keyword(line);
            let args = split_args(args);
            match keyword.to_ascii_lowercase().as_str() {
                "host" => {
                    block = Vec::new();
                    for arg in args {
                        if arg.starts_with('!') || arg.contains(['*', '?']) {
                            continue;
                        }
                        self.ensure_host(&arg);
                        block.push(arg);
                    }
                }
                "match" => block = Vec::new(),
                "hostname" => self.set_option(&block, args.first(), |h| &mut h.hostname),
                "user" => self.set_option(&block, args.first(), |h| &mut h.user),
                "include" if depth < MAX_INCLUDE_DEPTH => {
                    for pattern in &args {
                        for path in self.resolve_include(pattern) {
                            if let Ok(included) = self.loader.read_to_string(&path) {
                                self.parse_text(&included, depth + 1, block.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn ensure_host(&mut self, alias: &str) {
        if !self.hosts.iter().any(|h| h.alias == alias) {
            self.hosts.push(SshHostInfo {
                alias: alias.to_string(),
                hostname: None,
                user: None,
            });
        }
    }

    fn set_option(
        &mut self,
        block: &[String],
        value: Option<&String>,
        field: fn(&mut SshHostInfo) -> &mut Option<String>,
    ) {
        let Some(value) = value else { return };
        for alias in block {
            if let Some(host) = self.hosts.iter_mut().find(|h| &h.alias == alias) {
                let slot = field(host);
                if slot.is_none() {
                    *slot = Some(value.clone());
                }
            }
        }
    }

    fn resolve_include(&self, pattern: &str) -> Vec<PathBuf> {
        let path = if let Some(rest) = pattern.strip_prefix("~/") {
            match self.home {
                Some(home) => home.join(rest),
                None => return Vec::new(),
            }
        } else if Path::new(pattern).is_absolute() {
            PathBuf::from(pattern)
        } else {
            self.ssh_dir.join(pattern)
        };
        self.expand_glob(&path)
    }

    fn expand_glob(&self, path: &Path) -> Vec<PathBuf> {
        let mut candidates = vec![PathBuf::new()];
        for component in path.components() {
            let glob = match component {
                Component::Normal(name) => name.to_str().filter(|n| n.contains(['*', '?'])),
                _ => None,
            };
            let Some(glob) = glob else {
                for candidate in &mut candidates {
                    candidate.push(component.as_os_str());
                }
                continue;
            };
            let mut next = Vec::new();
            for candidate in &candidates {
                let dir = if candidate.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    candidate.as_path()
                };
                let Ok(mut names) = self.loader.list_dir(dir) else {
                    continue;
                };
                names.retain(|name| glob_match(glob, name));
                names.sort();
                next.extend(names.into_iter().map(|name| candidate.join(name)));
            }
            candidates = next;
        }
        candidates
    }
}

fn split_keyword(line: &str) -> (&str, &str) {
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    (&line[..end], rest.trim_start())
}

fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = args.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(first) = chars.peek().copied() else {
            break;
        };
        let mut token = String::new();
        if first == '"' {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                token.push(c);
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                token.push(c);
            }
            if token.starts_with('#') {
                break;
            }
        }
        out.push(token);
    }
    out
}

/// Matches one path component the way glob(3) does for `*` and `?`.
fn glob_match(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = name.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while t < txt.len() {
        if p < pat.len() && pat[p] == '*' {
            backtrack = Some((p, t));
            p += 1;
        } else if p < pat.len() && (pat[p] == '?' || pat[p] == txt[t]) {
            p += 1;
            t += 1;
        } else if let Some((star_p, star_t)) = backtrack {
            p = star_p + 1;
            t = star_t + 1;
            backtrack = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct MemLoader {
        files: HashMap<PathBuf, String>,
        denied: HashSet<PathBuf>,
    }

    impl MemLoader {
        fn with(files: &[(&str, &str)]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(p, c)| (PathBuf::from(p), c.to_string()))
                    .collect(),
                denied: HashSet::new(),
            }
        }
    }

    impl SshConfigLoader for MemLoader {
        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            if self.denied.contains(path) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| io::ErrorKind::NotFound.into())
        }

        fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
            Ok(self
                .files
                .keys()
                .filter(|p| p.parent() == Some(dir))
                .filter_map(|p| p.file_name()?.to_str().map(String::from))
                .collect())
        }
    }

    fn list(loader: &MemLoader, home: Option<&str>) -> Vec<SshHostInfo> {
        list_host_aliases(
            loader,
            Path::new("/h/.ssh/config"),
            Path::new("/h/.ssh"),
            home.map(Path::new),
        )
        .unwrap()
    }

    fn aliases(hosts: &[SshHostInfo]) -> Vec<&str> {
        hosts.iter().map(|h| h.alias.as_str()).collect()
    }

    fn info(alias: &str, hostname: Option<&str>, user: Option<&str>) -> SshHostInfo {
        SshHostInfo {
            alias: alias.into(),
            hostname: hostname.map(String::from),
            user: user.map(String::from),
        }
    }

    #[test]
    fn config_with_literal_hosts_lists_aliases_in_file_order_with_hostname_and_user() {
        let l = MemLoader::with(&[(
            "/h/.ssh/config",
            "Host b\n  HostName %h.example.com\n  User bob\nHost a\n  HostName a.example.com\n",
        )]);
        assert_eq!(
            list(&l, None),
            vec![
                info("b", Some("%h.example.com"), Some("bob")),
                info("a", Some("a.example.com"), None),
            ]
        );
    }

    #[test]
    fn host_line_with_patterns_skips_wildcards_and_negations_but_keeps_literals() {
        let l = MemLoader::with(&[(
            "/h/.ssh/config",
            "Host web !bastion *.corp db? \nHost *\n  User x\n",
        )]);
        assert_eq!(aliases(&list(&l, None)), vec!["web"]);
    }

    #[test]
    fn match_block_options_are_not_attributed_to_the_previous_host() {
        let l = MemLoader::with(&[(
            "/h/.ssh/config",
            "Host a\n  HostName a.example\nMatch host a\n  User nobody\n  HostName other\n",
        )]);
        assert_eq!(list(&l, None), vec![info("a", Some("a.example"), None)]);
    }

    #[test]
    fn keywords_are_case_insensitive_and_accept_equals_quotes_and_trailing_comments() {
        let l = MemLoader::with(&[(
            "/h/.ssh/config",
            "# comment\n\nhost=a # note\nHOSTNAME = \"my host\"\n  user=\tbob #x\nHost \"q s\" #c\nHostNameX nope\n",
        )]);
        assert_eq!(
            list(&l, None),
            vec![
                info("a", Some("my host"), Some("bob")),
                info("q s", None, None)
            ]
        );
    }

    #[test]
    fn duplicate_aliases_keep_first_position_and_values_and_later_blocks_fill_unset_fields() {
        let l = MemLoader::with(&[(
            "/h/.ssh/config",
            "Host a\n  HostName one\nHost b\nHost a\n  HostName two\n  User late\n",
        )]);
        assert_eq!(
            list(&l, None),
            vec![info("a", Some("one"), Some("late")), info("b", None, None)]
        );
    }

    #[test]
    fn include_resolves_relative_to_ssh_dir_and_expands_globs_sorted_skipping_dotfiles() {
        let l = MemLoader::with(&[
            ("/h/.ssh/config", "Include conf.d/*.conf\n"),
            ("/h/.ssh/conf.d/b.conf", "Host b\n"),
            ("/h/.ssh/conf.d/a.conf", "Host a\n"),
            ("/h/.ssh/conf.d/.hidden.conf", "Host hidden\n"),
            ("/h/.ssh/conf.d/c.txt", "Host c\n"),
        ]);
        assert_eq!(aliases(&list(&l, None)), vec!["a", "b"]);
    }

    #[test]
    fn include_expands_tilde_from_home_and_accepts_absolute_paths_and_skips_tilde_without_home() {
        let l = MemLoader::with(&[
            (
                "/h/.ssh/config",
                "Include ~/extra/t.conf /etc/ssh/abs.conf\n",
            ),
            ("/h/extra/t.conf", "Host tilde\n"),
            ("/etc/ssh/abs.conf", "Host abs\n"),
        ]);
        assert_eq!(aliases(&list(&l, Some("/h"))), vec!["tilde", "abs"]);
        assert_eq!(aliases(&list(&l, None)), vec!["abs"]);
    }

    #[test]
    fn include_inside_host_block_applies_leading_options_and_restores_block_afterwards() {
        let l = MemLoader::with(&[
            (
                "/h/.ssh/config",
                "Host x\n  Include inc\n  User after\nHost y\n",
            ),
            ("/h/.ssh/inc", "HostName inc.example\nHost leaked\n"),
        ]);
        assert_eq!(
            list(&l, None),
            vec![
                info("x", Some("inc.example"), Some("after")),
                info("leaked", None, None),
                info("y", None, None),
            ]
        );
    }

    #[test]
    fn self_include_terminates_and_lists_alias_once() {
        let l = MemLoader::with(&[("/h/.ssh/config", "Host a\nInclude config\n")]);
        assert_eq!(aliases(&list(&l, None)), vec!["a"]);
    }

    #[test]
    fn include_chain_deeper_than_max_depth_stops_descending() {
        let mut files = vec![(
            "/h/.ssh/config".to_string(),
            "Host h0\nInclude f1\n".to_string(),
        )];
        for i in 1..20 {
            files.push((
                format!("/h/.ssh/f{i}"),
                format!("Host h{i}\nInclude f{}\n", i + 1),
            ));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();
        let hosts = list(&MemLoader::with(&refs), None);
        // Depth 0 (root) through MAX_INCLUDE_DEPTH inclusive are parsed.
        assert_eq!(hosts.len(), MAX_INCLUDE_DEPTH + 1);
        assert_eq!(hosts.last().unwrap().alias, format!("h{MAX_INCLUDE_DEPTH}"));
    }

    #[test]
    fn missing_root_yields_empty_list_and_unreadable_root_is_an_error() {
        let empty = MemLoader::default();
        assert!(list(&empty, None).is_empty());

        let mut denied = MemLoader::with(&[("/h/.ssh/config", "Host a\n")]);
        denied.denied.insert(PathBuf::from("/h/.ssh/config"));
        let err = list_host_aliases(
            &denied,
            Path::new("/h/.ssh/config"),
            Path::new("/h/.ssh"),
            None,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn missing_or_unreadable_include_is_skipped() {
        let mut l = MemLoader::with(&[
            (
                "/h/.ssh/config",
                "Host a\nInclude nope locked *.none\nHost b\n",
            ),
            ("/h/.ssh/locked", "Host c\n"),
        ]);
        l.denied.insert(PathBuf::from("/h/.ssh/locked"));
        assert_eq!(aliases(&list(&l, None)), vec!["a", "b"]);
    }

    #[test]
    fn glob_match_handles_star_question_literals_and_leading_dot_rule() {
        assert!(glob_match("*.conf", "a.conf"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b", "aXXc"));
        assert!(glob_match("?.conf", "a.conf"));
        assert!(!glob_match("?.conf", "ab.conf"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exactly"));
        assert!(!glob_match("*", ".hidden"));
        assert!(!glob_match("?hidden", ".hidden"));
        assert!(glob_match(".*", ".hidden"));
        assert!(glob_match("*", ""));
    }
}
