//! Scratch directories and shells for upload tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A directory under the test temp dir, removed (read-only parts included) on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        // Canonical, because the receiver reports canonical paths and macOS
        // temp dirs sit behind the /var -> /private/var symlink.
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = base.join(format!(
            "herdr-upload-test-{}-{}-{label}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn join(&self, child: &str) -> PathBuf {
        self.0.join(child)
    }

    pub(crate) fn str(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+w"])
            .arg(&self.0)
            .status();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every POSIX shell on this machine the receiver must work with, as an argv
/// prefix. Missing ones are skipped with a note, since CI images differ.
pub(crate) fn posix_shells() -> Vec<Vec<String>> {
    let mut shells = vec![vec!["/bin/sh".to_owned()]];
    for (program, argv) in [
        ("dash", vec!["dash"]),
        ("bash", vec!["bash", "--posix"]),
        ("busybox", vec!["busybox", "sh"]),
    ] {
        let found = std::process::Command::new("sh")
            .args(["-c", &format!("command -v {program}")])
            .output()
            .is_ok_and(|output| output.status.success());
        if found {
            shells.push(argv.into_iter().map(str::to_owned).collect());
        } else {
            eprintln!("skipping {program}: not installed");
        }
    }
    // For shells that only exist elsewhere, e.g. BusyBox in a container:
    // HERDR_TEST_EXTRA_SHELL="docker run -i --rm -u 1000 -v /tmp:/tmp alpine sh".
    if let Ok(extra) = std::env::var("HERDR_TEST_EXTRA_SHELL") {
        shells.push(extra.split_whitespace().map(str::to_owned).collect());
    }
    shells
}
