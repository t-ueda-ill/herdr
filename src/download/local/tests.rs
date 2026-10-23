use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use base64::Engine as _;

use super::*;
use crate::api::schema::{PaneUploadEntryHeader, PaneUploadEntryKind};
use crate::upload::sink_host::HostSink;
use crate::upload::test_support::TempDir;

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn header(kind: PaneUploadEntryKind, size: Option<u64>, mode: u32) -> PaneUploadEntryHeader {
    PaneUploadEntryHeader {
        kind,
        mode,
        mtime: Some(1_700_000_000),
        size,
        link_target: None,
    }
}

fn dir(path: &str) -> PaneDownloadRecord {
    PaneDownloadRecord {
        path: path.into(),
        entry: Some(header(PaneUploadEntryKind::Directory, None, 0o755)),
        offset: 0,
        data: String::new(),
        last: true,
    }
}

fn link(path: &str, target: &str) -> PaneDownloadRecord {
    let mut entry = header(PaneUploadEntryKind::Symlink, None, 0o777);
    entry.link_target = Some(target.into());
    PaneDownloadRecord {
        path: path.into(),
        entry: Some(entry),
        offset: 0,
        data: String::new(),
        last: true,
    }
}

fn encode(body: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(body)
}

/// The records of one file, split at `chunk` bytes.
fn file(path: &str, body: &[u8], mode: u32, chunk: usize) -> Vec<PaneDownloadRecord> {
    let mut records = Vec::new();
    let mut offset = 0;
    let parts: Vec<&[u8]> = if body.is_empty() {
        vec![&[]]
    } else {
        body.chunks(chunk).collect()
    };
    for (index, part) in parts.iter().enumerate() {
        records.push(PaneDownloadRecord {
            path: path.into(),
            entry: (index == 0)
                .then(|| header(PaneUploadEntryKind::File, Some(body.len() as u64), mode)),
            offset,
            data: encode(part),
            last: index + 1 == parts.len(),
        });
        offset += part.len() as u64;
    }
    records
}

fn apply_all(writer: &mut LocalWriter, records: &[PaneDownloadRecord]) {
    for record in records {
        writer.apply(record).unwrap();
    }
}

fn stages(dest: &Path) -> Vec<String> {
    std::fs::read_dir(dest)
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(crate::upload::STAGE_PREFIX))
        .collect()
}

fn published(committed: Committed) -> (Vec<(String, String)>, Vec<String>) {
    match committed {
        Committed::Published { paths, warnings } => (paths, warnings),
        other => panic!("{other:?}"),
    }
}

fn tree_writer(dest: &TempDir) -> LocalWriter {
    LocalWriter::open(dest.path().to_path_buf(), &names(&["t"]))
        .unwrap()
        .0
}

/// A filesystem that treats names that differ only in case as one.
#[derive(Default)]
struct Folding {
    seen: HashSet<String>,
}

impl StagedNames for Folding {
    fn started(&mut self, rel: &str) {
        self.seen.insert(rel.to_lowercase());
    }

    fn exists(&self, sink: &HostSink, rel: &str) -> bool {
        self.seen.contains(&rel.to_lowercase()) || sink.staged_exists(rel)
    }
}

#[test]
fn records_become_a_published_tree_with_modes_and_times() {
    let dest = TempDir::new("local-tree");
    let mut writer = tree_writer(&dest);
    apply_all(&mut writer, &[dir("t")]);
    apply_all(&mut writer, &file("t/run.sh", b"echo hi\n", 0o755, 3));
    apply_all(&mut writer, &file("t/empty", b"", 0o600, 3));
    apply_all(&mut writer, &[dir("t/sub")]);
    apply_all(&mut writer, &file("t/sub/x", b"x", 0o644, 3));

    let (paths, warnings) = published(writer.publish(PaneUploadCollision::Rename).unwrap());

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].0, "t");
    assert!(warnings.is_empty());
    assert_eq!(std::fs::read(dest.join("t/run.sh")).unwrap(), b"echo hi\n");
    assert_eq!(std::fs::read(dest.join("t/sub/x")).unwrap(), b"x");
    assert_eq!(std::fs::read(dest.join("t/empty")).unwrap(), b"");
    let mode = |path: &str| {
        std::fs::metadata(dest.join(path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!((mode("t/run.sh"), mode("t/empty")), (0o755, 0o600));
    let mtime = std::fs::metadata(dest.join("t/run.sh"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(mtime, 1_700_000_000);
    assert!(stages(dest.path()).is_empty());
}

#[test]
fn a_name_that_exists_is_published_as_a_numbered_name() {
    let dest = TempDir::new("local-rename");
    std::fs::write(dest.join("x.txt"), b"old").unwrap();
    std::fs::create_dir(dest.join("d")).unwrap();
    std::fs::write(dest.join(".bashrc"), b"old").unwrap();
    std::fs::write(dest.join("y"), b"old").unwrap();
    std::fs::write(dest.join("y (1)"), b"old").unwrap();
    let (mut writer, _) = LocalWriter::open(
        dest.path().to_path_buf(),
        &names(&["x.txt", "d", ".bashrc", "y"]),
    )
    .unwrap();
    apply_all(&mut writer, &file("x.txt", b"new", 0o644, 9));
    apply_all(&mut writer, &[dir("d")]);
    apply_all(&mut writer, &file(".bashrc", b"new", 0o644, 9));
    apply_all(&mut writer, &file("y", b"new", 0o644, 9));

    let (paths, _) = published(writer.publish(PaneUploadCollision::Rename).unwrap());

    let shown: Vec<&str> = paths
        .iter()
        .map(|(_, path)| Path::new(path).file_name().unwrap().to_str().unwrap())
        .collect();
    // Published in the order of the stage's sorted names.
    assert_eq!(shown, [".bashrc (1)", "d (1)", "x (1).txt", "y (2)"]);
    assert_eq!(std::fs::read(dest.join("x (1).txt")).unwrap(), b"new");
    assert_eq!(std::fs::read(dest.join("x.txt")).unwrap(), b"old");
}

#[test]
fn a_symlink_is_created_only_when_everything_else_is_written() {
    let dest = TempDir::new("local-links");
    let mut writer = tree_writer(&dest);
    apply_all(&mut writer, &[dir("t"), link("t/l", "f")]);
    apply_all(&mut writer, &file("t/f", b"data", 0o644, 9));

    let stage = stages(dest.path());
    let staged = dest.join(&format!("{}/i/t", stage[0]));
    assert!(staged.join("f").exists());
    assert!(
        std::fs::symlink_metadata(staged.join("l")).is_err(),
        "a link written early could redirect a later write outside the stage"
    );
    writer.publish(PaneUploadCollision::Rename).unwrap();
    assert_eq!(
        std::fs::read_link(dest.join("t/l")).unwrap(),
        Path::new("f")
    );
}

#[test]
fn a_record_sent_twice_fails_instead_of_being_skipped() {
    let dest = TempDir::new("local-twice");
    let mut writer = tree_writer(&dest);
    apply_all(&mut writer, &[dir("t")]);
    apply_all(&mut writer, &file("t/f", b"a", 0o644, 9));

    let failure = writer.apply(&file("t/f", b"a", 0o644, 9)[0]).unwrap_err();

    assert_eq!(failure.0, "download_failed");
    writer.abort();
    assert!(stages(dest.path()).is_empty());
}

#[test]
fn a_name_that_collides_by_case_is_skipped_with_its_subtree_and_the_rest_is_published() {
    let dest = TempDir::new("local-fold");
    let (writer, _) = LocalWriter::open(dest.path().to_path_buf(), &names(&["a"])).unwrap();
    let mut writer = writer.with_staged_names(Box::new(Folding::default()));
    apply_all(&mut writer, &[dir("a")]);
    apply_all(&mut writer, &file("a/Makefile", b"upper", 0o644, 9));
    apply_all(&mut writer, &file("a/makefile", b"lower", 0o644, 2));
    apply_all(&mut writer, &[dir("a/Dir"), dir("a/dir")]);
    apply_all(&mut writer, &file("a/dir/inner", b"zz", 0o644, 1));
    apply_all(&mut writer, &file("a/after", b"ok", 0o644, 9));

    let (_, warnings) = published(writer.publish(PaneUploadCollision::Rename).unwrap());

    assert_eq!(std::fs::read(dest.join("a/Makefile")).unwrap(), b"upper");
    assert_eq!(std::fs::read(dest.join("a/after")).unwrap(), b"ok");
    assert!(!dest.join("a/makefile").exists());
    assert!(!dest.join("a/Dir/inner").exists());
    assert_eq!(
        warnings
            .iter()
            .filter(|line| line.contains("collides"))
            .count(),
        2,
        "one warning for each skipped root: {warnings:?}"
    );
}

#[test]
fn a_symlink_that_collides_with_a_later_name_is_left_out_with_a_warning() {
    let dest = TempDir::new("local-link-fold");
    let (writer, _) = LocalWriter::open(dest.path().to_path_buf(), &names(&["t"])).unwrap();
    let mut writer = writer.with_staged_names(Box::new(Folding::default()));
    apply_all(&mut writer, &[dir("t"), link("t/A", "x"), dir("t/a")]);
    apply_all(&mut writer, &file("t/a/f", b"1", 0o644, 9));

    let (_, warnings) = published(writer.publish(PaneUploadCollision::Rename).unwrap());

    assert!(std::fs::symlink_metadata(dest.join("t/A")).is_err());
    assert_eq!(std::fs::read(dest.join("t/a/f")).unwrap(), b"1");
    assert!(
        warnings.iter().any(|line| line.contains("t/A")),
        "{warnings:?}"
    );
}

#[test]
fn a_top_level_name_that_collides_by_case_fails_the_download() {
    let dest = TempDir::new("local-top-fold");
    let (writer, _) =
        LocalWriter::open(dest.path().to_path_buf(), &names(&["Make", "make"])).unwrap();
    let mut writer = writer.with_staged_names(Box::new(Folding::default()));
    apply_all(&mut writer, &[dir("Make")]);

    let failure = writer.apply(&dir("make")).unwrap_err();

    assert_eq!(failure.0, "download_failed");
    assert!(
        failure.1.contains("same name on this machine"),
        "{failure:?}"
    );
}

#[test]
fn a_collision_fails_a_publish_that_must_not_rename_and_replace_overwrites() {
    let dest = TempDir::new("local-policy");
    std::fs::write(dest.join("f"), b"old").unwrap();
    let make = || {
        let (mut writer, _) = LocalWriter::open(dest.path().to_path_buf(), &names(&["f"])).unwrap();
        apply_all(&mut writer, &file("f", b"new", 0o644, 9));
        writer
    };

    let conflict = make().publish(PaneUploadCollision::Fail).unwrap();
    assert_eq!(conflict, Committed::Conflict(names(&["f"])));
    assert_eq!(std::fs::read(dest.join("f")).unwrap(), b"old");
    assert!(
        stages(dest.path()).is_empty(),
        "a refused publish leaves no stage"
    );

    make().publish(PaneUploadCollision::Replace).unwrap();
    assert_eq!(std::fs::read(dest.join("f")).unwrap(), b"new");
    // The replaced original is removed after the publish is reported.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !stages(dest.path()).is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(stages(dest.path()).is_empty());
}

#[test]
fn a_name_that_was_never_sent_fails_the_publish() {
    let dest = TempDir::new("local-missing");
    let (writer, _) = LocalWriter::open(dest.path().to_path_buf(), &names(&["a", "b"])).unwrap();

    let failure = writer.publish(PaneUploadCollision::Rename).unwrap_err();

    assert_eq!(failure.0, "download_failed");
    assert!(stages(dest.path()).is_empty());
}

#[test]
fn a_write_that_cannot_land_fails_in_download_words_and_abort_removes_the_stage() {
    let dest = TempDir::new("local-readonly");
    let mut writer = tree_writer(&dest);
    let stage = stages(dest.path());
    let staged = dest.join(&format!("{}/i", stage[0]));
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::File::create(staged.join("probe")).is_ok() {
        eprintln!("skipping: this user can write into a read-only folder");
        return;
    }

    let failure = writer.apply(&dir("t")).unwrap_err();

    assert_eq!(failure.0, "download_failed");
    assert!(
        !failure.1.to_lowercase().contains("upload"),
        "the message must not talk about uploads: {}",
        failure.1
    );
    writer.abort();
    assert!(stages(dest.path()).is_empty());
}

#[test]
fn a_file_larger_than_the_download_limit_is_download_too_large() {
    let dest = TempDir::new("local-large");
    let mut writer = tree_writer(&dest);
    let mut record = file("t", b"x", 0o644, 9).remove(0);
    if let Some(entry) = record.entry.as_mut() {
        entry.size = Some(crate::download::MAX_DOWNLOAD_BYTES + 1);
    }

    let failure = writer.apply(&record).unwrap_err();

    assert_eq!(failure.0, "download_too_large");
    assert!(
        !failure.1.to_lowercase().contains("upload"),
        "{}",
        failure.1
    );
}

#[test]
fn data_that_is_not_base64_fails_the_download() {
    let dest = TempDir::new("local-base64");
    let mut writer = tree_writer(&dest);
    let mut record = file("t", b"x", 0o644, 9).remove(0);
    record.data = "***".into();

    let failure = writer.apply(&record).unwrap_err();

    assert_eq!(failure.0, "download_failed");
}

#[test]
fn a_missing_destination_is_refused_when_opening() {
    let dest = TempDir::new("local-nodest");

    let failure = LocalWriter::open(dest.join("nope"), &names(&["t"]))
        .err()
        .unwrap();

    assert_eq!(failure.0, "not_found");
}

#[test]
fn the_skip_set_covers_a_name_and_what_is_below_it_only() {
    let mut skipped = SkipSet::default();
    skipped.insert("a");
    skipped.insert("x/y");

    assert!(skipped.covers("a"));
    assert!(skipped.covers("a/b/c"));
    assert!(!skipped.covers("ab/x"));
    assert!(skipped.covers("x/y/z"));
    assert!(!skipped.covers("x"));
    assert!(!skipped.covers("x/yz"));
}

#[test]
fn a_selection_is_checked_for_count_length_and_names_that_would_share_a_name() {
    let one = |path: &str| vec![path.to_owned()];
    assert!(check_selection(&one("/a/b")).is_ok());
    assert!(check_selection(&[]).is_err());
    assert!(check_selection(&vec!["/a".into(); 1]).is_ok());
    let many: Vec<String> = (0..1025).map(|n| format!("/d/{n}")).collect();
    assert!(check_selection(&many).is_err());
    assert!(check_selection(&one(&format!("/{}", "x".repeat(4_096)))).is_err());
    let wide: Vec<String> = (0..70)
        .map(|n| format!("/{}/{n}", "y".repeat(4_000)))
        .collect();
    assert!(check_selection(&wide).unwrap_err().contains("together"));
    let same = check_selection(&["/a/x".into(), "/b/x".into()]).unwrap_err();
    assert!(same.contains("same name"), "{same}");
    let folded = check_selection(&["/a/Makefile".into(), "/a/makefile".into()]).unwrap_err();
    assert!(
        folded.contains("Makefile") && folded.contains("makefile"),
        "{folded}"
    );
    let windows = check_selection(&["C:\\a\\Data".into(), "C:\\b\\data".into()]).unwrap_err();
    assert!(windows.contains("same name"), "{windows}");
    assert!(check_selection(&one("/")).is_err());
    assert!(check_selection(&one("relative")).is_err());
}
