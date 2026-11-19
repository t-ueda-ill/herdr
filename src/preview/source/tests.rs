use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "herdr-preview-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(&path).unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, relative: &str, contents: &[u8]) {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The preview scripts run by a local shell, the way docker or ssh would.
fn shell_source(shell: &'static [&'static str], root: &Path) -> OffHostSource {
    let run = move |script: &str, args: &[&str]| {
        let output = Command::new(shell[0])
            .args(&shell[1..])
            .arg("-c")
            .arg(script)
            .arg("sh")
            .args(args)
            .output()
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    };
    OffHostSource::new(root.to_str().unwrap().to_owned(), Box::new(run))
}

/// Why none off Unix: the scripts only ever run in ssh and container
/// environments, which a Windows server does not open, and it has no `sh`.
#[cfg(not(unix))]
fn shells() -> Vec<&'static [&'static str]> {
    Vec::new()
}

#[cfg(unix)]
fn shells() -> Vec<&'static [&'static str]> {
    let mut shells: Vec<&'static [&'static str]> = vec![&["/bin/sh"]];
    if Command::new("busybox")
        .arg("true")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        shells.push(&["busybox", "sh"]);
    }
    shells
}

/// Every implementation, so both answer the same way.
fn sources(root: &Path) -> Vec<(String, Box<dyn FileSource>)> {
    let mut sources: Vec<(String, Box<dyn FileSource>)> =
        vec![("local".into(), Box::new(LocalSource::new(root).unwrap()))];
    for shell in shells() {
        sources.push((shell.join(" "), Box::new(shell_source(shell, root))));
    }
    sources
}

const BINARY: &[u8] = b"line one\n\0\x01\xff tail\n";

#[test]
fn a_whole_file_comes_back_byte_for_byte() {
    let root = TempDir::new("whole");
    root.file("docs/a b.bin", BINARY);
    for (name, source) in sources(root.path()) {
        assert_eq!(
            source.read("docs/a b.bin", Window::Whole).unwrap(),
            FileBytes {
                size: BINARY.len() as u64,
                bytes: BINARY.to_vec()
            },
            "{name}"
        );
    }
}

#[test]
fn a_probe_reports_only_the_size_and_a_slice_only_its_bytes() {
    let root = TempDir::new("slice");
    root.file("a.bin", BINARY);
    for (name, source) in sources(root.path()) {
        let probe = source.read("a.bin", Window::Probe).unwrap();
        assert_eq!(probe.size, BINARY.len() as u64, "{name}");
        assert!(probe.bytes.is_empty(), "{name}");
        let slice = source
            .read("a.bin", Window::Slice { start: 5, len: 6 })
            .unwrap();
        assert_eq!(slice.bytes, &BINARY[5..11], "{name}");
        let head = source
            .read("a.bin", Window::Slice { start: 0, len: 4 })
            .unwrap();
        assert_eq!(head.bytes, b"line", "{name}");
    }
}

#[test]
fn missing_files_and_folders_are_told_apart() {
    let root = TempDir::new("kinds");
    root.file("docs/a.md", b"x");
    for (name, source) in sources(root.path()) {
        assert_eq!(
            source.read("nope.md", Window::Whole),
            Err(ReadError::NotFound),
            "{name}"
        );
        assert_eq!(
            source.read("docs", Window::Whole),
            Err(ReadError::Directory),
            "{name}"
        );
    }
}

#[cfg(unix)]
#[test]
fn links_that_lead_out_of_the_root_are_refused_and_links_inside_are_followed() {
    let outside = TempDir::new("outside");
    outside.file("secret.txt", b"secret");
    let root = TempDir::new("links");
    root.file("real/page.html", b"inside");
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        root.path().join("leak.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("away")).unwrap();
    std::os::unix::fs::symlink(
        root.path().join("real/page.html"),
        root.path().join("alias.html"),
    )
    .unwrap();
    for (name, source) in sources(root.path()) {
        assert_eq!(
            source.read("leak.txt", Window::Whole),
            Err(ReadError::Forbidden),
            "{name}"
        );
        assert_eq!(
            source.read("away/secret.txt", Window::Whole),
            Err(ReadError::Forbidden),
            "{name}"
        );
        assert_eq!(
            source.read("alias.html", Window::Whole).unwrap().bytes,
            b"inside",
            "{name}"
        );
    }
}

#[test]
fn the_fingerprint_follows_only_the_files_the_browser_loaded() {
    let root = TempDir::new("watch");
    root.file("index.html", b"one");
    root.file("docs/a b.css", b"x");
    let watched = [
        "index.html".to_owned(),
        "docs/a b.css".to_owned(),
        "missing.js".to_owned(),
    ];
    for (name, source) in sources(root.path()) {
        let before = source.fingerprint(&watched).unwrap();
        assert_eq!(
            source.fingerprint(&watched).unwrap(),
            before,
            "{name}: stable"
        );

        root.file("target/debug/build.log", b"churn");
        root.file("unlinked.html", b"not loaded");
        assert_eq!(
            source.fingerprint(&watched).unwrap(),
            before,
            "{name}: files the page never loaded"
        );

        root.file("index.html", b"two!");
        let written = source.fingerprint(&watched).unwrap();
        assert_ne!(written, before, "{name}: write");

        root.file("missing.js", b"now here");
        let created = source.fingerprint(&watched).unwrap();
        assert_ne!(created, written, "{name}: a loaded 404 appears");

        std::fs::remove_file(root.path().join("docs/a b.css")).unwrap();
        assert_ne!(
            source.fingerprint(&watched).unwrap(),
            created,
            "{name}: removal"
        );

        root.file("index.html", b"one");
        root.file("docs/a b.css", b"x");
        std::fs::remove_file(root.path().join("missing.js")).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn the_fingerprint_follows_a_link_to_the_file_it_serves() {
    let root = TempDir::new("watch-link");
    root.file("real/page.html", b"one");
    std::os::unix::fs::symlink(
        root.path().join("real/page.html"),
        root.path().join("alias.html"),
    )
    .unwrap();
    let watched = ["alias.html".to_owned()];
    for (name, source) in sources(root.path()) {
        let before = source.fingerprint(&watched).unwrap();
        root.file("real/page.html", b"two, longer");
        assert_ne!(
            source.fingerprint(&watched).unwrap(),
            before,
            "{name}: the target changed"
        );
        root.file("real/page.html", b"one");
    }
}

#[test]
fn a_missing_root_has_no_fingerprint() {
    let root = TempDir::new("gone");
    let path = root.path().join("missing");
    for shell in shells() {
        assert_eq!(
            shell_source(shell, &path).fingerprint(&["index.html".to_owned()]),
            Err(ReadError::NotFound)
        );
    }
}

#[test]
fn garbage_before_the_marker_is_skipped_and_unknown_output_fails() {
    let mut output = b"motd banner\n".to_vec();
    output.extend_from_slice(MARKER);
    output.extend_from_slice(b"size\x003\0abc");
    assert_eq!(
        parse_read(&output).unwrap(),
        FileBytes {
            size: 3,
            bytes: b"abc".to_vec()
        }
    );
    assert!(matches!(parse_read(b"nothing"), Err(ReadError::Failed(_))));
    let mut bad = MARKER.to_vec();
    bad.extend_from_slice(b"what\0");
    assert!(matches!(parse_read(&bad), Err(ReadError::Failed(_))));
}
