use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "herdr-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create test root");
        Self(path)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn file_service_replace_and_undo_refuse_external_edits() {
    let root = Temp::new();
    let path = root.path("a.txt");
    fs::write(&path, "original\r\n").unwrap();
    let change = FileChange {
        path: path.clone(),
        revision: revision(b"original\r\n"),
        text: "replacement\r\n".into(),
    };
    let ResponseResult::FilesChanged { outcomes } = replace(FilesReplaceParams {
        changes: vec![change.clone()],
    })
    .unwrap() else {
        panic!()
    };
    assert!(outcomes[0].error.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), "replacement\r\n");
    let undo = FileChange {
        path: path.clone(),
        revision: outcomes[0].revision.clone().unwrap(),
        text: "original\r\n".into(),
    };
    fs::write(&path, "external edit").unwrap();
    let ResponseResult::FilesChanged { outcomes } = replace(FilesReplaceParams {
        changes: vec![undo, change],
    })
    .unwrap() else {
        panic!()
    };
    assert!(outcomes.iter().all(|o| o.error.is_some()));
    assert_eq!(fs::read_to_string(&path).unwrap(), "external edit");
}
#[test]
fn file_service_search_patterns_hidden_files_and_cancellation() {
    let root = Temp::new();
    fs::create_dir(root.0.join("src")).unwrap();
    fs::write(root.path("src/a.rs"), "hello needle\nneedle\n").unwrap();
    fs::write(root.path("src/a.txt"), "needle").unwrap();
    fs::write(root.path(".hidden.rs"), "needle").unwrap();
    let params = FilesSearchParams {
        root: root.0.to_string_lossy().into_owned(),
        query: "needle".into(),
        include: "**/*.rs".into(),
        exclude: "src/*".into(),
        search_id: "test".into(),
        generation: 1,
        ..Default::default()
    };
    let ResponseResult::FileSearch {
        matches, cancelled, ..
    } = search(params.clone(), &AtomicU64::new(1)).unwrap()
    else {
        panic!()
    };
    assert!(!cancelled);
    assert_eq!(matches.len(), 1);
    assert!(matches[0].path.ends_with(".hidden.rs"));
    let ResponseResult::FileSearch {
        cancelled, matches, ..
    } = search(params, &AtomicU64::new(2)).unwrap()
    else {
        panic!()
    };
    assert!(cancelled);
    assert!(matches.is_empty());
}
#[test]
fn search_matches_carry_up_to_two_lines_around_the_hit_clamped_at_the_file_edges() {
    let root = Temp::new();
    fs::write(
        root.path("a.txt"),
        "needle top\none\ntwo\nneedle mid\nfour\r\n",
    )
    .unwrap();
    let params = FilesSearchParams {
        root: root.0.to_string_lossy().into_owned(),
        query: "needle".into(),
        search_id: "context".into(),
        generation: 1,
        ..Default::default()
    };
    let ResponseResult::FileSearch { matches, .. } = search(params, &AtomicU64::new(1)).unwrap()
    else {
        panic!()
    };
    let contexts: Vec<_> = matches.iter().map(|m| m.context.clone().unwrap()).collect();
    assert_eq!(contexts[0].before, Vec::<String>::new());
    assert_eq!(contexts[0].after, ["one", "two"]);
    assert_eq!(contexts[1].before, ["one", "two"]);
    assert_eq!(contexts[1].after, ["four"]);
}
#[test]
fn a_match_from_a_server_without_context_decodes_as_none() {
    let decoded: FileMatch = serde_json::from_value(serde_json::json!({
        "path": "/a", "revision": "r", "start": 0, "end": 1, "line": 1,
        "text": "a", "matched": "a", "replacement": null
    }))
    .unwrap();
    assert_eq!(decoded.context, None);
}
#[test]
fn file_service_no_directory_merge_and_no_unconfirmed_delete() {
    let root = Temp::new();
    fs::create_dir(root.0.join("a")).unwrap();
    fs::create_dir(root.0.join("b")).unwrap();
    fs::create_dir(root.0.join("b/a")).unwrap();
    let params = FilesOperateParams {
        operation: FileOperation::Copy,
        paths: vec![root.path("a")],
        destination: Some(root.path("b")),
        collision: FileCollision::Replace,
        confirmed: true,
    };
    let ResponseResult::FilesChanged { outcomes } = operate(params).unwrap() else {
        panic!()
    };
    assert!(outcomes[0]
        .error
        .as_ref()
        .unwrap()
        .contains("cannot be replaced"));
    assert!(operate(FilesOperateParams {
        operation: FileOperation::Delete,
        paths: vec![root.path("a")],
        destination: None,
        collision: FileCollision::Skip,
        confirmed: false
    })
    .is_err());
    assert!(root.0.join("a").exists());
}
#[test]
fn file_service_read_revision_does_not_return_duplicate_text() {
    let root = Temp::new();
    fs::write(root.path("a"), "日本語\n").unwrap();
    let ResponseResult::FileDocument { document } = read_document(FileReadParams {
        path: root.path("a"),
        revision: Some(revision("日本語\n".as_bytes())),
    })
    .unwrap() else {
        panic!()
    };
    assert!(document.unchanged);
    assert!(document.text.is_empty());
}

#[test]
fn file_service_move_refuses_source_alias_without_deleting_file() {
    let root = Temp::new();
    let path = root.path("a");
    fs::write(&path, "keep me").unwrap();
    let ResponseResult::FilesChanged { outcomes } = operate(FilesOperateParams {
        operation: FileOperation::Move,
        paths: vec![path.clone()],
        destination: Some(root.path("./a")),
        collision: FileCollision::Replace,
        confirmed: true,
    })
    .unwrap() else {
        panic!()
    };
    assert!(outcomes[0].error.is_some());
    assert_eq!(fs::read_to_string(path).unwrap(), "keep me");
}
#[test]
fn file_service_copy_skip_never_overwrites_and_move_preserves_contents() {
    let root = Temp::new();
    fs::write(root.path("a"), "original").unwrap();
    fs::write(root.path("b"), "destination").unwrap();
    let ResponseResult::FilesChanged { outcomes } = operate(FilesOperateParams {
        operation: FileOperation::Copy,
        paths: vec![root.path("a")],
        destination: Some(root.path("b")),
        collision: FileCollision::Skip,
        confirmed: false,
    })
    .unwrap() else {
        panic!()
    };
    assert!(outcomes[0].error.is_some());
    assert_eq!(fs::read_to_string(root.path("b")).unwrap(), "destination");
    let ResponseResult::FilesChanged { outcomes } = operate(FilesOperateParams {
        operation: FileOperation::Move,
        paths: vec![root.path("a")],
        destination: Some(root.path("c")),
        collision: FileCollision::Skip,
        confirmed: true,
    })
    .unwrap() else {
        panic!()
    };
    assert!(outcomes[0].error.is_none());
    assert!(!root.0.join("a").exists());
    assert_eq!(fs::read_to_string(root.path("c")).unwrap(), "original");
}
#[test]
fn stat_reports_size_mode_owner_and_link_target_of_one_path() {
    let root = Temp::new();
    fs::write(root.path("a.txt"), "hello").unwrap();
    let ResponseResult::FileStat { stat: file } = stat(FileStatParams {
        path: root.path("a.txt"),
    })
    .unwrap() else {
        panic!()
    };
    assert_eq!(file.kind, Some(DirectoryEntryKind::File));
    assert_eq!(file.size, Some(5));
    assert!(file.mtime.is_some_and(|mtime| mtime > 0));
    assert_eq!(file.children, None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.path("a.txt"), fs::Permissions::from_mode(0o4750)).unwrap();
        crate::platform::create_symlink("a.txt", &root.0.join("link")).unwrap();
        let ResponseResult::FileStat { stat: file } = stat(FileStatParams {
            path: root.path("a.txt"),
        })
        .unwrap() else {
            panic!()
        };
        assert_eq!(file.mode, Some(0o4750));
        assert!(file.owner.is_some_and(|owner| !owner.is_empty()));
        let ResponseResult::FileStat { stat: link } = stat(FileStatParams {
            path: root.path("link"),
        })
        .unwrap() else {
            panic!()
        };
        assert_eq!(link.kind, Some(DirectoryEntryKind::Symlink));
        assert_eq!(link.link_target.as_deref(), Some("a.txt"));
        assert_eq!(link.size, None);
    }
}
#[test]
fn stat_counts_the_entries_of_a_directory_without_git() {
    let root = Temp::new();
    fs::create_dir(root.0.join(".git")).unwrap();
    fs::create_dir(root.0.join("sub")).unwrap();
    fs::write(root.path(".hidden"), "").unwrap();
    fs::write(root.path("a"), "").unwrap();
    let ResponseResult::FileStat { stat: dir } = stat(FileStatParams {
        path: root.0.to_string_lossy().into_owned(),
    })
    .unwrap() else {
        panic!()
    };
    assert_eq!(dir.kind, Some(DirectoryEntryKind::Dir));
    assert_eq!(dir.children, Some(3));
    assert!(!dir.children_limited);
    assert_eq!(dir.size, None);
}
#[test]
fn stat_rejects_relative_and_missing_paths() {
    let root = Temp::new();
    assert!(stat(FileStatParams { path: "a".into() }).is_err());
    assert!(stat(FileStatParams {
        path: root.path("missing"),
    })
    .is_err());
}
