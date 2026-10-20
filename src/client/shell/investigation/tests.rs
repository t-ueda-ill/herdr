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
