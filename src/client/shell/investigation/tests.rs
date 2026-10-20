use super::*;
#[test]
fn document_selection_preserves_utf8_and_defers_external_changes() {
    let mut doc = Document::new("/a".into());
    doc.update("a日本\r\n👨‍👩‍👧\n".into(), "one".into());
    doc.motion(KeyCode::Right, false, 10);
    doc.motion(KeyCode::Right, true, 10);
    assert_eq!(&doc.text[doc.range().unwrap()], "日");
    doc.update("other".into(), "two".into());
    assert!(doc.pending.is_some());
    assert_eq!(doc.revision, "one");
    doc.clear_selection();
    assert_eq!(doc.text, "other");
    assert_eq!(doc.revision, "two");
}
#[test]
fn saved_document_does_not_contain_content_clipboard_or_undo() {
    let saved = SavedInvestigation {
        files: vec![SavedDocument {
            path: "/a".into(),
            cursor: 3,
            scroll: 1,
            horizontal: 0,
        }],
        ..Default::default()
    };
    let json = serde_json::to_string(&saved).unwrap();
    assert!(!json.contains("revision"));
    assert!(!json.contains("clipboard"));
    assert!(!json.contains("undo"));
}

fn state() -> ClientShellState {
    use crate::api::schema::{DirectoryEntry, DirectoryEntryKind};
    use crate::client::shell::file_viewer::model::{
        DirectoryListing, DirectoryState, FileViewerOptions,
    };
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
    let mut browser = ClientFileViewerOverlay::new(FileViewerOptions {
        generation: 1,
        workspace_id: Some("ws_1".into()),
        show_hidden: true,
        show_ignored: false,
        index_supported: false,
        location: None,
    });
    browser.root = Some("/repo".into());
    browser.pane_id = Some("pane_1".into());
    browser.directories.insert(
        "/repo".into(),
        DirectoryState::Loaded(DirectoryListing {
            canonical: "/repo".into(),
            entries: vec![DirectoryEntry {
                name: "a.txt".into(),
                kind: DirectoryEntryKind::File,
                target_kind: None,
                size: Some(6),
                mtime: None,
                hidden: false,
                ignored: Some(false),
            }],
            truncated: false,
            omitted: None,
            unlistable: None,
            git_error: None,
        }),
    );
    browser.rebuild(false);
    let mut investigation = Investigation::new(browser, ClientEndpointId::Local, "boot-1".into());
    let mut document = Document::new("/repo/a.txt".into());
    document.update("hello\n".into(), "old".into());
    investigation.preview = Some(document);
    state.workbench.tabs.push(investigation);
    state.workbench.active = Some(0);
    state.workbench.visible = true;
    state
}
fn press(
    state: &mut ClientShellState,
    code: KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> ClientShellInput {
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Key(
        crate::input::TerminalKey::new(code, modifiers),
    )])
}
fn assert_no_terminal_input(outcome: &ClientShellInput) {
    assert!(!outcome.requests.iter().any(|r| matches!(
        r,
        ClientMessage::ClientShellPaneInput { .. } | ClientMessage::ClientShellPopupInput { .. }
    )));
}
#[test]
fn investigation_enter_space_typing_and_paste_never_reach_terminal() {
    use crossterm::event::KeyModifiers as M;
    let mut state = state();
    assert_no_terminal_input(&press(&mut state, KeyCode::Char(' '), M::empty()));
    assert_eq!(state.investigation().unwrap().focus, Focus::Body);
    assert_no_terminal_input(&press(&mut state, KeyCode::Char('x'), M::empty()));
    assert_no_terminal_input(&state.handle_raw_events(vec![
        crate::raw_input::RawInputEvent::Paste("must not execute".into()),
    ]));
    press(&mut state, KeyCode::Esc, M::empty());
    assert_no_terminal_input(&press(&mut state, KeyCode::Enter, M::empty()));
    assert_eq!(state.investigation().unwrap().documents.len(), 1);
    assert_eq!(state.snapshot.as_ref().unwrap().tabs.len(), 1);
    assert_eq!(
        state.snapshot.as_ref().unwrap().focused_tab_id.as_deref(),
        Some("tab_1")
    );
}
#[test]
fn investigation_search_spaces_and_file_close_are_client_local() {
    use crossterm::event::KeyModifiers as M;
    let mut state = state();
    press(&mut state, KeyCode::Char('f'), M::CONTROL | M::SHIFT);
    assert_no_terminal_input(&state.handle_raw_events(vec![
        crate::raw_input::RawInputEvent::Paste("hello world".into()),
    ]));
    assert_eq!(state.investigation().unwrap().query.as_str(), "hello world");
    press(&mut state, KeyCode::Esc, M::empty());
    let outcome = press(&mut state, KeyCode::Char('w'), M::CONTROL);
    assert_no_terminal_input(&outcome);
    assert!(outcome.actions.is_empty());
    assert_eq!(state.snapshot.as_ref().unwrap().tabs.len(), 1);
}
#[test]
fn investigation_ignores_superseded_search_and_read_responses() {
    use crate::api::schema::{FileDocument, ResponseResult};
    let mut state = state();
    state.investigation_mut().unwrap().search_generation = 3;
    let mut outcome = ClientShellInput::default();
    state.handle_investigation_result(
        FileRequest::Search {
            id: 1,
            generation: 2,
        },
        Ok(ResponseResult::FileSearch {
            matches: Vec::new(),
            truncated: false,
            cancelled: false,
            warnings: Vec::new(),
        }),
        &mut outcome,
    );
    assert!(!state.investigation().unwrap().search_ready);
    state
        .investigation_mut()
        .unwrap()
        .preview
        .as_mut()
        .unwrap()
        .request_generation = 4;
    state.handle_investigation_result(
        FileRequest::Read {
            id: 1,
            path: "/repo/a.txt".into(),
            generation: 2,
            prepare: false,
        },
        Ok(ResponseResult::FileDocument {
            document: FileDocument {
                path: "/repo/a.txt".into(),
                text: "stale".into(),
                revision: "stale".into(),
                unchanged: false,
            },
        }),
        &mut outcome,
    );
    assert_eq!(
        state.investigation().unwrap().document().unwrap().text,
        "hello\n"
    );
}
#[test]
fn investigation_outer_tab_navigation_never_changes_terminal_identity() {
    use crate::input::KeybindAction;
    let mut state = state();
    let original = state.snapshot.clone();
    let mut outcome = ClientShellInput::default();
    assert!(state.investigation_terminal_action(KeybindAction::NextTab, &mut outcome));
    assert!(!state.workbench.visible);
    assert_eq!(state.snapshot, original);
    state.workbench.visible = true;
    assert!(state.investigation_terminal_action(KeybindAction::CloseTab, &mut outcome));
    assert!(state.workbench.tabs.is_empty());
    assert_eq!(state.snapshot, original);
}

#[test]
fn investigation_reopened_replace_preview_rejects_prior_prepare_generation() {
    use crate::api::schema::{FileDocument, ResponseResult};
    let mut state = state();
    let tab = state.investigation_mut().unwrap();
    tab.prepare_generation = 2;
    tab.prepare_pending = 1;
    tab.dialog = Some(Dialog::Replace { scroll: 0 });
    state.handle_investigation_result(
        FileRequest::Read {
            id: 1,
            path: "/repo/a.txt".into(),
            generation: 1,
            prepare: true,
        },
        Ok(ResponseResult::FileDocument {
            document: FileDocument {
                path: "/repo/a.txt".into(),
                text: "hello".into(),
                revision: "old".into(),
                unchanged: false,
            },
        }),
        &mut ClientShellInput::default(),
    );
    assert_eq!(state.investigation().unwrap().prepare_pending, 1);
    assert!(state.investigation().unwrap().plan.is_empty());
}
#[test]
fn investigation_search_jump_clamps_cached_utf8_cursor_and_render_fits_small_screens() {
    use crate::api::schema::FileMatch;
    let mut state = state();
    let tab = state.investigation_mut().unwrap();
    let mut document = Document::new("/repo/a.txt".into());
    document.update("あx\n".into(), "old".into());
    tab.documents.push(document);
    tab.content_search = true;
    tab.matches.push(FileMatch {
        path: "/repo/a.txt".into(),
        revision: "new".into(),
        start: 1,
        end: 2,
        line: 1,
        text: "xx".into(),
        matched: "x".into(),
        replacement: None,
    });
    state.request_investigation_preview(false, true, &mut ClientShellInput::default());
    assert_eq!(state.investigation().unwrap().document().unwrap().cursor, 0);
    for (cols, rows) in [(160, 48), (100, 30), (60, 20), (20, 10), (8, 4)] {
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
        let layout = state.layout(cols, rows);
        state.render_investigation(&mut buffer, layout);
    }
}
#[test]
fn investigation_copy_mode_input_is_captured_and_server_restart_invalidates_mutations() {
    use crate::api::schema::FileChange;
    use crossterm::event::KeyModifiers as M;
    let mut state = state();
    state.mode = ClientShellMode::Copy;
    assert_no_terminal_input(&press(&mut state, KeyCode::Char(' '), M::empty()));
    assert_eq!(state.investigation().unwrap().focus, Focus::Body);
    state.investigation_mut().unwrap().undo.push(FileChange {
        path: "/repo/a.txt".into(),
        revision: "old".into(),
        text: "saved".into(),
    });
    state.workbench.tabs[0].boot = "previous-boot".into();
    state.tick_investigation(std::time::Instant::now(), &mut ClientShellInput::default());
    assert!(state.workbench.tabs[0].undo.is_empty());
    assert_eq!(state.workbench.tabs[0].boot, "boot-1");
}
#[test]
fn investigation_document_copy_excludes_line_numbers_and_keeps_original_newlines() {
    use crossterm::event::KeyModifiers as M;
    let mut state = state();
    let tab = state.investigation_mut().unwrap();
    tab.focus = Focus::Body;
    let doc = tab.document_mut().unwrap();
    doc.update("one\r\n日本語\r\n".into(), "crlf".into());
    doc.anchor = Some(0);
    doc.cursor = doc.text.len();
    let result = press(&mut state, KeyCode::Char('c'), M::CONTROL);
    assert_no_terminal_input(&result);
    assert!(result.actions.iter().any(|a|matches!(a,ClientShellAction::ClipboardWrite(bytes) if bytes=="one\r\n日本語\r\n".as_bytes())));
}
#[test]
fn investigation_frame_snapshot() {
    let mut state = state();
    state.set_pane_surface(crate::client::shell::tests::surface());
    let tab = state.investigation_mut().unwrap();
    tab.preview.as_mut().unwrap().update(
        "pub fn greet(name: &str) {\n    println!(\"Hello, {name}\");\n}\n".into(),
        "demo".into(),
    );
    let frame = state.compose(120, 32).expect("composed investigation");
    if let Some(path) = std::env::var_os("HERDR_WORKBENCH_SNAPSHOT") {
        let rows: Vec<_> = frame
            .cells
            .chunks(frame.width as usize)
            .map(|cells| cells.iter().map(|c| c.symbol.as_str()).collect::<String>())
            .collect();
        std::fs::write(path, rows.join("\n")).unwrap();
    }
    assert!(state.hits.panes.is_empty());
    assert!(!state.investigation_hits.tabs.is_empty());
}

#[test]
fn investigation_cached_preview_jump_and_restored_scroll_survive_read() {
    use crate::api::schema::{FileDocument, FileMatch, ResponseResult};
    let mut state = state();
    let tab = state.investigation_mut().unwrap();
    tab.preview
        .as_mut()
        .unwrap()
        .update("あx\n".into(), "old".into());
    tab.content_search = true;
    tab.matches.push(FileMatch {
        path: "/repo/a.txt".into(),
        revision: "new".into(),
        start: 1,
        end: 2,
        line: 1,
        text: "xx".into(),
        matched: "x".into(),
        replacement: None,
    });
    state.request_investigation_preview(false, true, &mut ClientShellInput::default());
    let document = state.investigation().unwrap().document().unwrap();
    assert!(document.text.is_char_boundary(document.cursor));
    let tab = state.investigation_mut().unwrap();
    let document = tab.preview.as_mut().unwrap();
    document.loaded = false;
    document.restored_scroll = true;
    document.scroll = 3;
    document.request_generation = 7;
    state.handle_investigation_result(
        FileRequest::Read {
            id: 1,
            path: "/repo/a.txt".into(),
            generation: 7,
            prepare: false,
        },
        Ok(ResponseResult::FileDocument {
            document: FileDocument {
                path: "/repo/a.txt".into(),
                text: "1\n2\n3\n4\n5\n".into(),
                revision: "new".into(),
                unchanged: false,
            },
        }),
        &mut ClientShellInput::default(),
    );
    assert_eq!(state.investigation().unwrap().document().unwrap().scroll, 3);
}
#[test]
fn investigation_fast_editor_returns_after_authoritative_missing_pane() {
    let mut state = state();
    state.workbench.visible = false;
    state.workbench.editor = Some(EditorReturn {
        id: 1,
        pane: "short-lived".into(),
        due: std::time::Instant::now(),
        awaiting: true,
    });
    state.handle_investigation_result(
        FileRequest::EditorCheck {
            id: 1,
            pane: "short-lived".into(),
        },
        Err(ClientShellEndpointError {
            code: Some("pane_not_found".into()),
            message: "exited".into(),
        }),
        &mut ClientShellInput::default(),
    );
    assert!(state.investigation_active());
    assert!(state.workbench.editor.is_none());
}
#[test]
fn investigation_optional_search_failure_does_not_block_other_actions() {
    let mut state = state();
    state.set_endpoint_methods(Some(vec!["file.read".into()]));
    let tab = state.investigation_mut().unwrap();
    tab.content_search = true;
    tab.query = "hello".into();
    tab.schedule_search();
    state.tick_investigation(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
        &mut ClientShellInput::default(),
    );
    assert!(!state.investigation().unwrap().searching);
    assert!(!state.investigation().unwrap().operation_pending);
}

#[test]
#[ignore = "supporting fixed-geometry client compose scaling profile"]
fn investigation_render_scale_profile() {
    use crate::protocol::SurfaceRect;
    for count in [1, 15] {
        let mut state = state();
        state.config.hide_tab_bar_when_single_tab = false;
        let mut snapshot = crate::client::shell::tests::snapshot();
        let template = snapshot.panes[0].clone();
        snapshot.panes = (0..count)
            .map(|i| {
                let mut p = template.clone();
                p.pane_id = format!("pane_{}", i + 1);
                p.focused = i == 0;
                p
            })
            .collect();
        state.set_snapshot(Box::new(snapshot));
        let mut surface = crate::client::shell::tests::surface();
        let template = surface.panes[0].clone();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 132, 47));
        surface.panes = (0..count)
            .map(|i| {
                let mut p = template.clone();
                p.pane_id = format!("pane_{}", i + 1);
                p.focused = i == 0;
                p.rect = SurfaceRect {
                    x: 0,
                    y: i * 3,
                    width: 132,
                    height: 3,
                };
                p.inner_rect = p.rect;
                buffer.set_string(
                    0,
                    i * 3,
                    format!("populated pane {} output for compose scaling", i + 1),
                    Style::default(),
                );
                p
            })
            .collect();
        surface.frame = FrameData::from_ratatui_buffer(&buffer, None);
        state.set_pane_surface(surface);
        for mode in [0, 1, 2] {
            if mode == 0 {
                state.workbench.visible = false;
                state.workbench.tabs[0].browser.workspace_id = Some("other".into());
            } else {
                state.workbench.tabs[0].browser.workspace_id = Some("ws_1".into());
                state.workbench.visible = mode == 2;
            }
            for _ in 0..5 {
                std::hint::black_box(state.compose(160, 48));
            }
            let start = std::time::Instant::now();
            for _ in 0..100 {
                std::hint::black_box(state.compose(160, 48));
            }
            eprintln!(
                "investigation-compose panes={count} mode={mode} us/frame={:.2}",
                start.elapsed().as_micros() as f64 / 100.0
            );
        }
    }
}

#[test]
fn investigation_surface_suspend_resume_waits_for_ack_and_idle_lane() {
    let mut state = state();
    let mut out = ClientShellInput::default();
    assert!(!state.reconcile_investigation_surface(7, "boot-1", true, true, &mut out));
    assert!(state.file_surface_pause().is_none());
    assert!(!state.reconcile_investigation_surface(7, "boot-1", true, false, &mut out));
    assert!(state.investigation_surface_paused(&ClientEndpointId::Local, 7, "boot-1"));
    assert_eq!(out.actions.len(), 1);
    state.workbench.visible = false;
    assert!(!state.reconcile_investigation_surface(7, "boot-1", false, false, &mut out));
    state.workbench.pause.as_mut().unwrap().confirmed = true;
    assert!(!state.reconcile_investigation_surface(7, "boot-1", false, true, &mut out));
    assert!(state.reconcile_investigation_surface(7, "boot-1", false, false, &mut out));
    assert!(state.file_surface_pause().unwrap().resume_requested);
}

#[test]
fn investigation_surface_reconnect_discards_old_terminal_intents() {
    let mut state = state();
    let mut out = ClientShellInput::default();
    state.reconcile_investigation_surface(7, "boot-1", true, false, &mut out);
    state.defer_investigation_terminal_action(ClientShellAction::ClipboardWrite(b"old".to_vec()));
    state.reconcile_investigation_surface(8, "boot-2", false, false, &mut out);
    assert!(state.file_surface_pause().is_none());
    assert!(state.workbench.terminal_requests.is_empty());
    assert!(!state.workbench.terminal_requested);
    assert!(!state.investigation_surface_paused(&ClientEndpointId::Local, 7, "boot-1"));
}

fn saved_in(endpoint: &str, workspace: &str, root: &str) -> SavedInvestigation {
    SavedInvestigation {
        endpoint: endpoint.into(),
        workspace: Some(workspace.into()),
        root: root.into(),
        ..Default::default()
    }
}

fn first_listing_error() -> Result<crate::api::schema::ResponseResult, ClientShellEndpointError> {
    Err(ClientShellEndpointError {
        code: Some("permission_denied".into()),
        message: "denied".into(),
    })
}

#[test]
fn failed_first_listing_closes_the_promoted_tab_and_raises_a_notice() {
    let mut state = state();
    let tab = state.investigation_mut().unwrap();
    tab.browser.root = None;
    let generation = tab.browser.generation;
    state.handle_file_viewer_endpoint_result(
        FileViewerRequest::List {
            generation,
            path: None,
        },
        first_listing_error(),
        &mut ClientShellInput::default(),
    );
    assert!(state.workbench.tabs.is_empty());
    assert!(!state.investigation_active());
}

#[test]
fn unusable_first_listing_answer_closes_the_promoted_tab_with_a_notice() {
    let mut state = state();
    let tab = state.investigation_mut().unwrap();
    tab.browser.root = None;
    let generation = tab.browser.generation;
    state.handle_file_viewer_endpoint_result(
        FileViewerRequest::List {
            generation,
            path: None,
        },
        Ok(crate::api::schema::ResponseResult::Pong {
            version: String::new(),
            protocol: 0,
            capabilities: None,
        }),
        &mut ClientShellInput::default(),
    );
    assert!(state.workbench.tabs.is_empty());
    assert_eq!(
        state.endpoint_error.as_deref(),
        Some("endpoint returned an unexpected listing")
    );
}

#[test]
fn failed_first_listing_keeps_the_other_investigation_tabs() {
    let mut state = state();
    let loading = ClientFileViewerOverlay::new(
        crate::client::shell::file_viewer::model::FileViewerOptions {
            generation: 9,
            workspace_id: Some("ws_1".into()),
            show_hidden: true,
            show_ignored: false,
            index_supported: false,
            location: None,
        },
    );
    state.workbench.tabs.push(Investigation::new(
        loading,
        ClientEndpointId::Local,
        "boot-1".into(),
    ));
    state.handle_file_viewer_endpoint_result(
        FileViewerRequest::List {
            generation: 9,
            path: None,
        },
        first_listing_error(),
        &mut ClientShellInput::default(),
    );
    assert_eq!(state.workbench.tabs.len(), 1);
    assert_eq!(state.workbench.active, Some(0));
    assert!(state.investigation_active());
}

#[test]
fn failed_listing_of_a_restored_root_keeps_the_tab_with_a_failed_root_row() {
    let mut state = state();
    let tab = state.investigation_mut().unwrap();
    let generation = tab.browser.generation;
    state.handle_file_viewer_endpoint_result(
        FileViewerRequest::List {
            generation,
            path: Some("/repo".into()),
        },
        Err(ClientShellEndpointError {
            code: Some("not_found".into()),
            message: "gone".into(),
        }),
        &mut ClientShellInput::default(),
    );
    assert_eq!(state.workbench.tabs.len(), 1);
    assert!(matches!(
        state.workbench.tabs[0].browser.directories.get("/repo"),
        Some(crate::client::shell::file_viewer::model::DirectoryState::Failed(notice))
            if notice == "not found"
    ));
}

#[test]
fn restore_keeps_the_saved_entry_while_the_viewer_cannot_be_opened() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
    state.workbench.saved = vec![saved_in("local", "ws_1", "/repo")];
    state.set_endpoint_methods(Some(vec!["file.read".into()]));
    let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
    state.tick_investigation(later, &mut ClientShellInput::default());
    assert!(state.workbench.tabs.is_empty());
    assert_eq!(state.workbench.saved.len(), 1);
    state.set_endpoint_methods(None);
    state.tick_investigation(later, &mut ClientShellInput::default());
    assert_eq!(state.workbench.tabs.len(), 1);
    assert!(state.workbench.saved.is_empty());
    assert_eq!(
        state.workbench.tabs[0].browser.root.as_deref(),
        Some("/repo")
    );
}

#[test]
fn first_snapshot_drops_saved_entries_of_missing_workspaces_for_that_endpoint_only() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.workbench.saved = vec![
        saved_in("local", "ws_gone", "/a"),
        saved_in("local", "ws_1", "/b"),
        saved_in("ssh:other", "ws_gone", "/c"),
    ];
    assert_eq!(state.persisted_investigations().len(), 3);
    state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
    let roots: Vec<_> = state
        .persisted_investigations()
        .into_iter()
        .map(|s| s.root)
        .collect();
    assert_eq!(roots, ["/b", "/c"]);
}

#[test]
fn saved_entries_are_judged_once_per_connection() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
    state.workbench.saved = vec![saved_in("local", "ws_later", "/a")];
    let mut next = crate::client::shell::tests::snapshot();
    next.revision = 2;
    state.set_snapshot(Box::new(next));
    assert_eq!(state.workbench.saved.len(), 1);

    let mut reconnected = crate::client::shell::tests::snapshot();
    reconnected.revision = 3;
    state.cache_endpoint_snapshot_for_generation(
        &crate::client::endpoint::ClientEndpointId::Local,
        2,
        Box::new(reconnected),
    );
    assert!(state.workbench.saved.is_empty());
}

#[test]
fn persisted_investigations_keep_the_newest_fifty_per_endpoint() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.workbench.saved = (0..55)
        .map(|i| saved_in("local", "ws_1", &format!("/l{i}")))
        .chain((0..3).map(|i| saved_in("ssh:other", "ws_1", &format!("/s{i}"))))
        .collect();
    let persisted = state.persisted_investigations();
    let local: Vec<_> = persisted.iter().filter(|s| s.endpoint == "local").collect();
    assert_eq!(local.len(), 50);
    assert_eq!(local[0].root, "/l5");
    assert_eq!(local[49].root, "/l54");
    assert_eq!(
        persisted.iter().filter(|s| s.endpoint != "local").count(),
        3
    );
}

// ---- off-host tabs (ssh / container locations) ----

mod offhost {
    use super::*;
    use crate::api::schema::{
        DirectoryEntry, DirectoryEntryKind, FileDocument, Method, PaneDirectoryListing,
        PaneFileContent, PaneLocation, PaneLocationKind, ResponseResult,
    };
    use crate::client::shell::file_viewer::FileLocation;
    use crate::protocol::ClientShellPane;

    fn pane(
        id: &str,
        container: Option<&str>,
        ssh: Option<&str>,
        focused: bool,
    ) -> ClientShellPane {
        let mut pane = crate::client::shell::tests::snapshot().panes.remove(0);
        pane.pane_id = id.into();
        pane.focused = focused;
        pane.container = container.map(str::to_owned);
        pane.ssh_host = ssh.map(str::to_owned);
        pane
    }

    fn container() -> FileLocation {
        FileLocation::Container {
            name: "app".into(),
            ssh_host: Some("devbox".into()),
        }
    }

    /// Panes of ws_1 as `(id, container, ssh_host)`; the first is focused.
    fn set_panes(state: &mut ClientShellState, panes: &[(&str, Option<&str>, Option<&str>)]) {
        let mut snapshot = crate::client::shell::tests::snapshot();
        snapshot.panes = panes
            .iter()
            .enumerate()
            .map(|(i, (id, c, s))| pane(id, *c, *s, i == 0))
            .collect();
        snapshot.focused_pane_id = Some(panes[0].0.into());
        state_set_snapshot(state, snapshot);
    }

    fn state_set_snapshot(
        state: &mut ClientShellState,
        mut snapshot: crate::protocol::ClientShellSnapshot,
    ) {
        snapshot.revision = state.snapshot.as_ref().map_or(1, |s| s.revision + 1);
        state.set_snapshot(Box::new(snapshot));
    }

    /// An investigation tab of the container `app@devbox` with two panes there.
    fn offhost_state() -> ClientShellState {
        let mut state = state();
        set_panes(
            &mut state,
            &[
                ("pane_1", Some("app"), Some("devbox")),
                ("pane_2", Some("app"), Some("devbox")),
            ],
        );
        let tab = state.investigation_mut().unwrap();
        tab.browser.location = Some(container());
        tab.browser.root = Some("/work".into());
        tab.preview = None;
        state
    }

    fn entry(name: &str, kind: DirectoryEntryKind) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind,
            target_kind: None,
            size: None,
            mtime: None,
            hidden: false,
            ignored: None,
        }
    }

    fn pane_listing(path: &str) -> ResponseResult {
        ResponseResult::PaneDirectoryList(PaneDirectoryListing {
            location: PaneLocation {
                kind: PaneLocationKind::Container,
                ssh_host: Some("devbox".into()),
                container: Some("app".into()),
            },
            root: "/work".into(),
            path: path.into(),
            canonical: path.into(),
            entries: vec![
                entry("src", DirectoryEntryKind::Dir),
                entry("a.txt", DirectoryEntryKind::File),
            ],
            truncated: false,
            git_error: None,
            omitted: None,
            unlistable: None,
        })
    }

    fn requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
        outcome
            .actions
            .iter()
            .filter_map(|action| match action {
                ClientShellAction::Endpoint { request, .. } => {
                    Some((request.id.clone(), request.method.clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn answer(
        state: &mut ClientShellState,
        id: &str,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) {
        state.handle_endpoint_result("boot-1", id, result);
    }

    fn later(seconds: u64) -> std::time::Instant {
        std::time::Instant::now() + std::time::Duration::from_secs(seconds)
    }

    fn tick(state: &mut ClientShellState, seconds: u64) -> Vec<(String, Method)> {
        let mut outcome = ClientShellInput::default();
        state.tick_investigation(later(seconds), &mut outcome);
        requests(&outcome)
    }

    fn with_loaded_preview(state: &mut ClientShellState) {
        let tab = state.investigation_mut().unwrap();
        let mut document = Document::new("/work/a.txt".into());
        document.update("hello\n".into(), "cksum:1:6".into());
        tab.preview = Some(document);
    }

    #[test]
    fn a_new_viewer_takes_its_location_from_the_focused_pane_before_any_answer() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
        set_panes(
            &mut state,
            &[
                ("pane_1", Some("app"), Some("devbox")),
                ("pane_2", None, None),
            ],
        );
        let mut outcome = ClientShellInput::default();

        state.open_file_viewer_for(Some("ws_1".into()), &mut outcome);

        let [(_, Method::PaneDirectoryList(list)), (_, Method::PaneDirectoryIndex(_))] =
            &requests(&outcome)[..]
        else {
            panic!("an off-host pane lists and indexes through pane.directory.*");
        };
        assert_eq!(list.pane_id, "pane_1");
        assert_eq!(list.path, None);
        assert_eq!(state.workbench.tabs[0].browser.location, Some(container()));
    }

    #[test]
    fn browsing_another_workspace_decides_the_location_from_that_workspace() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        let mut snapshot = crate::client::shell::tests::snapshot();
        let mut second = snapshot.workspaces[0].clone();
        second.workspace_id = "ws_2".into();
        second.active_tab_id = "tab_2".into();
        second.focused = false;
        snapshot.workspaces.push(second);
        let mut remote = pane("pane_9", None, Some("devbox"), false);
        remote.workspace_id = "ws_2".into();
        remote.tab_id = "tab_2".into();
        snapshot.panes.push(remote);
        state.set_snapshot(Box::new(snapshot));
        let mut outcome = ClientShellInput::default();

        state.open_file_viewer_for(Some("ws_2".into()), &mut outcome);

        let methods = requests(&outcome);
        assert!(
            methods.iter().any(|(_, m)| matches!(
                m,
                Method::PaneDirectoryList(list) if list.pane_id == "pane_9"
            )),
            "{methods:?}"
        );
        assert_eq!(
            state.workbench.tabs[0].browser.location,
            Some(FileLocation::Ssh {
                host: "devbox".into()
            })
        );
    }

    #[test]
    fn a_server_without_pane_directory_methods_keeps_listing_the_host() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_endpoint_methods(Some(
            Method::OPERATIONS
                .iter()
                .map(|op| op.name)
                .filter(|name| {
                    !name.starts_with("pane.directory.") && !name.starts_with("pane.file.")
                })
                .map(str::to_owned)
                .collect(),
        ));
        state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
        set_panes(&mut state, &[("pane_1", Some("app"), None)]);
        let mut outcome = ClientShellInput::default();

        state.open_file_viewer_for(Some("ws_1".into()), &mut outcome);

        assert!(matches!(&requests(&outcome)[0].1, Method::DirectoryList(_)));
        assert_eq!(state.workbench.tabs[0].browser.location, None);
    }

    #[test]
    fn expanding_lists_through_the_focused_pane_of_the_tabs_location() {
        let mut state = offhost_state();
        set_panes(
            &mut state,
            &[
                ("pane_1", Some("app"), Some("devbox")),
                ("pane_2", Some("app"), Some("devbox")),
            ],
        );
        let mut snapshot = state.snapshot.as_deref().unwrap().clone();
        snapshot.focused_pane_id = Some("pane_2".into());
        state_set_snapshot(&mut state, snapshot);
        let mut outcome = ClientShellInput::default();

        state.request_file_viewer_listing("/work/src".into(), &mut outcome);

        let [(_, Method::PaneDirectoryList(list))] = &requests(&outcome)[..] else {
            panic!("{:?}", requests(&outcome));
        };
        assert_eq!(list.pane_id, "pane_2");
        assert_eq!(list.path.as_deref(), Some("/work/src"));
    }

    #[test]
    fn a_closed_pane_hands_requests_to_another_pane_of_the_location() {
        let mut state = offhost_state();
        set_panes(
            &mut state,
            &[
                ("pane_9", None, None),
                ("pane_2", Some("app"), Some("devbox")),
            ],
        );
        let mut outcome = ClientShellInput::default();

        state.request_file_viewer_listing("/work/src".into(), &mut outcome);

        let [(_, Method::PaneDirectoryList(list))] = &requests(&outcome)[..] else {
            panic!("{:?}", requests(&outcome));
        };
        assert_eq!(list.pane_id, "pane_2");
    }

    #[test]
    fn a_pane_of_another_location_is_never_used_for_the_tab() {
        let mut state = offhost_state();
        set_panes(&mut state, &[("pane_1", Some("app"), None)]);
        let mut outcome = ClientShellInput::default();

        state.request_file_viewer_listing("/work/src".into(), &mut outcome);

        assert!(requests(&outcome).is_empty());
        let tab = state.investigation().unwrap();
        assert!(matches!(
            tab.browser.directories.get("/work/src"),
            Some(crate::client::shell::file_viewer::model::DirectoryState::Failed(n))
                if n == "no pane running in app@devbox"
        ));
    }

    #[test]
    fn a_tab_without_a_pane_stops_polling_and_resumes_when_one_returns() {
        let mut state = offhost_state();
        with_loaded_preview(&mut state);
        set_panes(&mut state, &[("pane_9", None, None)]);
        let mut outcome = ClientShellInput::default();
        state.request_file_viewer_listing("/work/src".into(), &mut outcome);

        assert!(tick(&mut state, 6).is_empty());
        assert_eq!(
            state.investigation().unwrap().notice,
            "no pane running in app@devbox"
        );

        set_panes(
            &mut state,
            &[
                ("pane_9", None, None),
                ("pane_3", Some("app"), Some("devbox")),
            ],
        );
        let resumed = tick(&mut state, 12);

        assert!(
            resumed.iter().any(|(_, m)| matches!(
                m,
                Method::PaneDirectoryList(list) if list.pane_id == "pane_3" && list.path.as_deref() == Some("/work/src")
            )),
            "{resumed:?}"
        );
        assert_eq!(state.investigation().unwrap().notice, "");
    }

    #[test]
    fn selecting_a_file_reads_it_through_pane_file_read_without_poll() {
        let mut state = offhost_state();
        let tab = state.investigation_mut().unwrap();
        tab.preview = Some(Document::new("/work/a.txt".into()));
        let mut outcome = ClientShellInput::default();
        let tab = state.investigation_mut().unwrap();
        tab.browser.directories.insert(
            "/work".into(),
            crate::client::shell::file_viewer::model::DirectoryState::Loaded(
                crate::client::shell::file_viewer::model::DirectoryListing {
                    canonical: "/work".into(),
                    entries: vec![entry("a.txt", DirectoryEntryKind::File)],
                    truncated: false,
                    omitted: None,
                    unlistable: None,
                    git_error: None,
                },
            ),
        );
        tab.browser.rebuild(false);
        tab.preview = None;

        state.request_investigation_preview(false, false, &mut outcome);

        let [(_, Method::PaneFileRead(read))] = &requests(&outcome)[..] else {
            panic!("{:?}", requests(&outcome));
        };
        assert_eq!(read.path, "/work/a.txt");
        assert!(!read.poll);
        assert_eq!(read.revision, None);
        assert_eq!(read.pane_id, "pane_1");
    }

    #[test]
    fn a_pane_file_answer_loads_the_document_like_a_host_answer() {
        let mut state = offhost_state();
        let tab = state.investigation_mut().unwrap();
        let mut document = Document::new("/work/a.txt".into());
        document.request_generation = 5;
        tab.preview = Some(document);
        let mut outcome = ClientShellInput::default();

        state.handle_investigation_result(
            FileRequest::Read {
                id: 1,
                path: "/work/a.txt".into(),
                generation: 5,
                prepare: false,
            },
            Ok(ResponseResult::PaneFileDocument(PaneFileContent {
                location: PaneLocation {
                    kind: PaneLocationKind::Container,
                    ssh_host: Some("devbox".into()),
                    container: Some("app".into()),
                },
                document: FileDocument {
                    path: "/work/a.txt".into(),
                    text: "remote\n".into(),
                    revision: "cksum:9:7".into(),
                    unchanged: false,
                },
            })),
            &mut outcome,
        );

        assert_eq!(
            state.investigation().unwrap().document().unwrap().text,
            "remote\n"
        );
    }

    #[test]
    fn a_pane_directory_answer_fills_the_tree_and_names_no_pane() {
        let mut state = offhost_state();
        let tab = state.investigation_mut().unwrap();
        tab.browser.root = None;
        let generation = tab.browser.generation;

        state.handle_file_viewer_endpoint_result(
            FileViewerRequest::List {
                generation,
                path: None,
            },
            Ok(pane_listing("/work")),
            &mut ClientShellInput::default(),
        );

        let tab = state.investigation().unwrap();
        assert_eq!(tab.browser.root.as_deref(), Some("/work"));
        assert_eq!(tab.browser.rows.len(), 2);
        assert_eq!(tab.notice, "");
    }

    #[test]
    fn off_host_polls_every_five_seconds_with_poll_set() {
        let mut state = offhost_state();
        with_loaded_preview(&mut state);
        state.investigation_mut().unwrap().last_poll = std::time::Instant::now();

        assert!(tick(&mut state, 4).is_empty());
        let due = tick(&mut state, 6);

        let [(_, Method::PaneFileRead(read))] = &due[..] else {
            panic!("{due:?}");
        };
        assert!(read.poll);
        assert_eq!(read.revision.as_deref(), Some("cksum:1:6"));
    }

    #[test]
    fn host_tabs_keep_polling_every_second_through_file_read() {
        let mut state = state();
        state.investigation_mut().unwrap().last_poll = std::time::Instant::now();

        assert!(tick(&mut state, 0).is_empty());
        let due = tick(&mut state, 2);

        assert!(matches!(&due[..], [(_, Method::FileRead(_))]), "{due:?}");
    }

    #[test]
    fn off_host_polling_waits_while_any_endpoint_request_is_pending() {
        let mut state = offhost_state();
        with_loaded_preview(&mut state);
        state.investigation_mut().unwrap().last_poll = std::time::Instant::now();
        let mut outcome = ClientShellInput::default();
        state.request_file_viewer_listing("/work/src".into(), &mut outcome);
        let (list_id, _) = requests(&outcome).remove(0);

        assert!(tick(&mut state, 6).is_empty());

        answer(&mut state, &list_id, Ok(pane_listing("/work/src")));
        assert!(matches!(
            &tick(&mut state, 7)[..],
            [(_, Method::PaneFileRead(_))]
        ));
    }

    #[test]
    fn off_host_polling_skips_a_tick_while_the_previous_read_is_pending() {
        let mut state = offhost_state();
        with_loaded_preview(&mut state);
        let tab = state.investigation_mut().unwrap();
        tab.last_poll = std::time::Instant::now();
        tab.pending_reads.insert("/work/a.txt".into());

        assert!(tick(&mut state, 6).is_empty());
    }

    #[test]
    fn content_search_replace_and_file_operations_only_say_they_are_unavailable() {
        use crossterm::event::KeyModifiers as M;
        let mut state = offhost_state();
        with_loaded_preview(&mut state);
        for index in (6..=15).chain([22]) {
            let mut outcome = ClientShellInput::default();
            state.investigation_menu_action(index, &mut outcome);
            assert!(requests(&outcome).is_empty(), "menu {index}");
            assert_eq!(
                state.investigation().unwrap().notice,
                "not available inside app@devbox",
                "menu {index}"
            );
            state.investigation_mut().unwrap().notice.clear();
        }
        for (code, modifiers) in [
            (KeyCode::Char('f'), M::CONTROL | M::SHIFT),
            (KeyCode::Char('h'), M::CONTROL),
            (KeyCode::F(2), M::empty()),
            (KeyCode::Delete, M::empty()),
        ] {
            state.investigation_mut().unwrap().focus = Focus::Results;
            let outcome = press(&mut state, code, modifiers);
            assert!(requests(&outcome).is_empty(), "{code:?}");
            let tab = state.investigation().unwrap();
            assert!(!tab.content_search, "{code:?}");
            assert_eq!(tab.notice, "not available inside app@devbox", "{code:?}");
            state.investigation_mut().unwrap().notice.clear();
        }
        let mut outcome = ClientShellInput::default();
        state.investigation_paste(&mut outcome);
        assert_eq!(
            state.investigation().unwrap().notice,
            "not available inside app@devbox"
        );
    }

    #[test]
    fn host_tabs_still_run_their_file_operations() {
        let mut state = state();
        let mut outcome = ClientShellInput::default();
        state.investigation_menu_action(12, &mut outcome);
        assert!(state.investigation().unwrap().notice.is_empty());
    }

    #[test]
    fn toggling_reshows_the_tab_whose_location_is_the_focused_panes() {
        let mut state = offhost_state();
        let host = {
            let mut tab = state_tab_host(&state);
            tab.id = 77;
            tab
        };
        state.workbench.tabs.push(host);
        state.workbench.visible = false;

        set_panes(&mut state, &[("pane_1", Some("app"), Some("devbox"))]);
        assert!(state.toggle_investigation());
        assert_eq!(state.workbench.active, Some(0));

        state.workbench.visible = false;
        set_panes(&mut state, &[("pane_9", None, None)]);
        assert!(state.toggle_investigation());
        assert_eq!(state.workbench.active, Some(1));

        state.workbench.visible = false;
        set_panes(&mut state, &[("pane_5", None, Some("other"))]);
        assert!(!state.toggle_investigation(), "no tab of that location yet");
    }

    #[test]
    fn toggling_on_a_server_without_pane_directory_reshows_the_host_fallback_tab() {
        let mut state = state();
        state.set_endpoint_methods(Some(
            Method::OPERATIONS
                .iter()
                .map(|op| op.name)
                .filter(|name| {
                    !name.starts_with("pane.directory.") && !name.starts_with("pane.file.")
                })
                .map(str::to_owned)
                .collect(),
        ));
        set_panes(&mut state, &[("pane_1", Some("app"), None)]);
        state.workbench.visible = false;

        assert!(state.toggle_investigation());
        assert_eq!(state.workbench.active, Some(0));
    }

    fn state_tab_host(state: &ClientShellState) -> Investigation {
        use crate::client::shell::file_viewer::model::FileViewerOptions;
        let mut browser = ClientFileViewerOverlay::new(FileViewerOptions {
            generation: 50,
            workspace_id: state.workbench.tabs[0].browser.workspace_id.clone(),
            show_hidden: true,
            show_ignored: false,
            index_supported: false,
            location: None,
        });
        browser.root = Some("/repo".into());
        Investigation::new(browser, ClientEndpointId::Local, "boot-1".into())
    }

    #[test]
    fn titles_and_tab_labels_prefix_the_location() {
        let mut state = offhost_state();
        let tab = state.investigation_mut().unwrap();
        assert_eq!(tab.browser.title(), "files · app@devbox:/work");
        tab.browser.location = Some(FileLocation::Container {
            name: "app".into(),
            ssh_host: None,
        });
        assert_eq!(tab.browser.title(), "files · app:/work");
        tab.browser.location = Some(FileLocation::Ssh {
            host: "devbox".into(),
        });
        tab.browser.root = Some("/home/u/proj".into());
        assert_eq!(tab.browser.title(), "files · devbox:/home/u/proj");
        tab.browser.location = None;
        assert_eq!(tab.browser.title(), "files · /home/u/proj");

        let tab = state.investigation_mut().unwrap();
        tab.browser.location = Some(container());
        tab.browser.root = Some("/work".into());
        state.set_pane_surface(crate::client::shell::tests::surface());
        let frame = state.compose(120, 32).expect("composed investigation");
        let text: String = frame.cells.iter().map(|c| c.symbol.as_str()).collect();
        assert!(text.contains("app@devbox:work"), "tab label");
        assert!(text.contains("app@devbox:/work"), "header path");
    }

    #[test]
    fn paths_go_only_to_the_focused_pane_in_the_tabs_location() {
        let mut state = offhost_state();
        let mut outcome = ClientShellInput::default();

        state.send_file_viewer_path("/work/a.txt", false, &mut outcome);
        assert!(!outcome.requests.is_empty(), "same location sends");

        set_panes(&mut state, &[("pane_9", None, None)]);
        let mut outcome = ClientShellInput::default();
        state.send_file_viewer_path("/work/a.txt", false, &mut outcome);
        assert!(outcome.requests.is_empty());
        assert_eq!(
            state.investigation().unwrap().browser.notice.as_deref(),
            Some("terminal destination is not in app@devbox")
        );
    }

    #[test]
    fn a_host_tab_refuses_to_send_host_paths_into_an_off_host_pane() {
        let mut state = state();
        set_panes(&mut state, &[("pane_1", None, Some("devbox"))]);
        let mut outcome = ClientShellInput::default();

        state.send_file_viewer_path("/repo/a.txt", false, &mut outcome);

        assert!(outcome.requests.is_empty());
        assert_eq!(
            state.investigation().unwrap().browser.notice.as_deref(),
            Some("terminal destination is not in host")
        );
    }

    #[test]
    fn the_send_menu_names_the_focused_pane_of_the_location_not_a_stored_one() {
        let mut state = offhost_state();
        with_loaded_preview(&mut state);
        let tab = state.investigation_mut().unwrap();
        tab.focus = Focus::Body;
        let mut outcome = ClientShellInput::default();

        state.investigation_menu_action(4, &mut outcome);

        assert!(matches!(
            &state.investigation().unwrap().dialog,
            Some(Dialog::Send { pane, .. }) if pane == "pane_1"
        ));
    }

    #[test]
    fn the_editor_opens_in_the_focused_pane_and_tracks_the_overlay_pane_it_answers_with() {
        let mut state = offhost_state();
        with_loaded_preview(&mut state);
        state.investigation_mut().unwrap().focus = Focus::Body;
        let mut outcome = ClientShellInput::default();

        state.investigation_menu_action(5, &mut outcome);

        let [(id, Method::PaneDirectoryOpenEditor(edit))] = &requests(&outcome)[..] else {
            panic!("{:?}", requests(&outcome));
        };
        assert_eq!(
            (edit.pane_id.as_str(), edit.path.as_str()),
            ("pane_1", "/work/a.txt")
        );
        let pane: crate::api::schema::PaneInfo = serde_json::from_value(serde_json::json!({
            "pane_id": "pane_editor", "terminal_id": "t9", "workspace_id": "ws_1",
            "tab_id": "tab_1", "focused": true, "agent_status": "unknown", "revision": 0
        }))
        .unwrap();
        answer(&mut state, id, Ok(ResponseResult::PaneInfo { pane }));
        assert!(!state.workbench.visible);
        assert_eq!(state.workbench.editor.as_ref().unwrap().pane, "pane_editor");
    }

    #[test]
    fn an_editor_answer_without_a_pane_is_reported_and_keeps_the_tab() {
        let mut state = offhost_state();
        with_loaded_preview(&mut state);
        state.investigation_mut().unwrap().focus = Focus::Body;
        let mut outcome = ClientShellInput::default();
        state.investigation_menu_action(5, &mut outcome);
        let (id, _) = requests(&outcome).remove(0);

        answer(&mut state, &id, Ok(ResponseResult::Ok {}));

        assert!(state.workbench.editor.is_none());
        assert!(state.workbench.visible);
        assert_eq!(
            state.investigation().unwrap().notice,
            "unexpected editor response"
        );
    }

    #[test]
    fn saved_entries_keep_their_location_and_old_entries_load_as_host() {
        let saved = SavedInvestigation {
            location: Some(container()),
            root: "/work".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&saved).unwrap();
        let back: SavedInvestigation = serde_json::from_str(&json).unwrap();
        assert_eq!(back.location, Some(container()));

        let old: SavedInvestigation = serde_json::from_str(
            r#"{"endpoint":"local","workspace":"ws_1","root":"/r","files":[],"active_file":null,"horizontal":false,"query":"","include":"","exclude":"","regex":false,"case_sensitive":false,"whole_word":false,"ignored":false,"content_search":false}"#,
        )
        .unwrap();
        assert_eq!(old.location, None);
        assert!(!serde_json::to_string(&old).unwrap().contains("location"));
    }

    #[test]
    fn an_off_host_saved_entry_waits_for_a_pane_of_its_location() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
        state.workbench.saved = vec![
            SavedInvestigation {
                location: Some(container()),
                ..saved_in("local", "ws_1", "/work")
            },
            saved_in("local", "ws_1", "/repo"),
        ];

        state.tick_investigation(later(1), &mut ClientShellInput::default());
        assert_eq!(
            state.workbench.tabs.len(),
            1,
            "the host entry is not held back"
        );
        assert_eq!(
            state.workbench.tabs[0].browser.root.as_deref(),
            Some("/repo")
        );
        assert_eq!(
            state.workbench.saved.len(),
            1,
            "the off-host entry stays saved"
        );

        set_panes(
            &mut state,
            &[
                ("pane_1", None, None),
                ("pane_3", Some("app"), Some("devbox")),
            ],
        );
        let mut outcome = ClientShellInput::default();
        state.tick_investigation(later(2), &mut outcome);

        assert!(state.workbench.saved.is_empty());
        let restored = &state.workbench.tabs[1];
        assert_eq!(restored.browser.location, Some(container()));
        assert_eq!(restored.browser.root.as_deref(), Some("/work"));
        assert!(requests(&outcome).iter().any(|(_, m)| matches!(
            m,
            Method::PaneDirectoryList(list) if list.pane_id == "pane_3" && list.path.as_deref() == Some("/work")
        )));
    }

    #[test]
    fn an_index_lost_for_lack_of_a_pane_is_requested_again_when_one_returns() {
        let mut state = offhost_state();
        let generation = state.investigation().unwrap().browser.generation;
        state.handle_file_viewer_endpoint_result(
            FileViewerRequest::Index { generation },
            Err(ClientShellEndpointError {
                code: Some("pane_not_found".into()),
                message: "pane not found: pane_1".into(),
            }),
            &mut ClientShellInput::default(),
        );
        set_panes(&mut state, &[("pane_3", Some("app"), Some("devbox"))]);

        let resumed = tick(&mut state, 2);

        assert!(
            resumed.iter().any(|(_, m)| matches!(
                m,
                Method::PaneDirectoryIndex(index)
                    if index.pane_id == "pane_3" && index.path.as_deref() == Some("/work")
            )),
            "{resumed:?}"
        );
        assert!(state.investigation().unwrap().browser.indexing());
        assert!(
            !tick(&mut state, 3)
                .iter()
                .any(|(_, m)| matches!(m, Method::PaneDirectoryIndex(_))),
            "asked once, not on every tick"
        );
    }

    fn focus_pane(state: &mut ClientShellState, pane_id: &str) {
        let mut snapshot = state.snapshot.as_deref().unwrap().clone();
        for pane in &mut snapshot.panes {
            pane.focused = pane.pane_id == pane_id;
        }
        snapshot.focused_pane_id = Some(pane_id.into());
        state_set_snapshot(state, snapshot);
    }

    fn list_pane_of(state: &mut ClientShellState, path: &str) -> String {
        let mut outcome = ClientShellInput::default();
        state.request_file_viewer_listing(path.into(), &mut outcome);
        let [(_, Method::PaneDirectoryList(list))] = &requests(&outcome)[..] else {
            panic!("{:?}", requests(&outcome));
        };
        list.pane_id.clone()
    }

    #[test]
    fn focusing_another_pane_of_the_location_does_not_move_the_requests() {
        let mut state = offhost_state();
        assert_eq!(list_pane_of(&mut state, "/work/a"), "pane_1");

        focus_pane(&mut state, "pane_2");

        assert_eq!(list_pane_of(&mut state, "/work/b"), "pane_1");
    }

    #[test]
    fn requests_move_to_the_focused_pane_of_the_location_once_the_used_pane_closes_and_stay_there()
    {
        let mut state = offhost_state();
        assert_eq!(list_pane_of(&mut state, "/work/a"), "pane_1");
        set_panes(
            &mut state,
            &[
                ("pane_3", Some("app"), Some("devbox")),
                ("pane_2", Some("app"), Some("devbox")),
            ],
        );

        assert_eq!(list_pane_of(&mut state, "/work/b"), "pane_3");

        focus_pane(&mut state, "pane_2");

        assert_eq!(list_pane_of(&mut state, "/work/c"), "pane_3");
    }

    #[test]
    fn a_listing_refused_because_its_pane_closed_is_requested_again_when_one_returns() {
        let mut state = offhost_state();
        let generation = state.investigation().unwrap().browser.generation;
        for code in ["pane_not_found", "pane_not_live", "stale_pane_target"] {
            state.handle_file_viewer_endpoint_result(
                FileViewerRequest::List {
                    generation,
                    path: Some(format!("/work/{code}")),
                },
                Err(ClientShellEndpointError {
                    code: Some(code.into()),
                    message: "gone".into(),
                }),
                &mut ClientShellInput::default(),
            );
        }
        state.handle_file_viewer_endpoint_result(
            FileViewerRequest::List {
                generation,
                path: Some("/work/denied".into()),
            },
            Err(ClientShellEndpointError {
                code: Some("permission_denied".into()),
                message: "denied".into(),
            }),
            &mut ClientShellInput::default(),
        );
        set_panes(&mut state, &[("pane_3", Some("app"), Some("devbox"))]);

        let mut asked = Vec::new();
        for seconds in [2, 3, 4, 5] {
            for (id, method) in tick(&mut state, seconds) {
                if let Method::PaneDirectoryList(list) = method {
                    let path = list.path.unwrap();
                    answer(&mut state, &id, Ok(pane_listing(&path)));
                    asked.push(path);
                }
            }
        }

        asked.sort();
        assert_eq!(
            asked,
            [
                "/work/pane_not_found",
                "/work/pane_not_live",
                "/work/stale_pane_target"
            ]
        );
    }

    #[test]
    fn an_index_that_is_simply_not_a_repository_is_not_requested_again() {
        let mut state = offhost_state();
        let generation = state.investigation().unwrap().browser.generation;
        state.handle_file_viewer_endpoint_result(
            FileViewerRequest::Index { generation },
            Ok(ResponseResult::DirectoryIndex {
                repo_root: None,
                entries: Vec::new(),
                truncated: false,
                git_error: None,
            }),
            &mut ClientShellInput::default(),
        );

        assert!(!tick(&mut state, 2)
            .iter()
            .any(|(_, m)| matches!(m, Method::PaneDirectoryIndex(_))));
    }

    fn restored_requests(location: Option<FileLocation>) -> Vec<(String, Method)> {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
        set_panes(
            &mut state,
            &[(
                "pane_1",
                location.as_ref().map(|_| "app"),
                location.as_ref().map(|_| "devbox"),
            )],
        );
        state.workbench.saved = vec![SavedInvestigation {
            location,
            ..saved_in("local", "ws_1", "/saved/root")
        }];
        let mut outcome = ClientShellInput::default();
        state.tick_investigation(later(1), &mut outcome);
        assert_eq!(
            state.workbench.tabs[0].browser.root.as_deref(),
            Some("/saved/root")
        );
        requests(&outcome)
    }

    #[test]
    fn restoring_a_host_tab_lists_and_indexes_the_saved_root_in_the_first_requests() {
        let sent = restored_requests(None);
        let [(_, Method::DirectoryList(list)), (_, Method::DirectoryIndex(index))] = &sent[..]
        else {
            panic!("{sent:?}");
        };
        assert_eq!(list.path.as_deref(), Some("/saved/root"));
        assert_eq!(index.path.as_deref(), Some("/saved/root"));
    }

    #[test]
    fn restoring_an_off_host_tab_lists_and_indexes_the_saved_root_in_the_first_requests() {
        let sent = restored_requests(Some(container()));
        let [(_, Method::PaneDirectoryList(list)), (_, Method::PaneDirectoryIndex(index))] =
            &sent[..]
        else {
            panic!("{sent:?}");
        };
        assert_eq!(list.path.as_deref(), Some("/saved/root"));
        assert_eq!(index.path.as_deref(), Some("/saved/root"));
    }
}
