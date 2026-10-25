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
    // Why full screen: these tests characterize the workbench covering the
    // whole pane surface; split tests choose split themselves.
    state.workbench.placement.presentation = FilesPresentation::Full;
    state.remember_files_placement();
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
        // 0: another workspace's tab, 1: hidden, 2: full screen, 3: docked.
        for mode in [0, 1, 2, 3] {
            if mode == 0 {
                state.workbench.visible = false;
                state.workbench.tabs[0].browser.workspace_id = Some("other".into());
            } else {
                state.workbench.tabs[0].browser.workspace_id = Some("ws_1".into());
                state.workbench.visible = mode >= 2;
            }
            state.workbench.placement.presentation = if mode == 3 {
                FilesPresentation::Split
            } else {
                FilesPresentation::Full
            };
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

/// Restores `saved` into a fresh state whose shown placement is full
/// screen at the bottom, then reopens it with open_files.
fn reopened_after_restore(saved: SavedInvestigation) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
    state.workbench.placement = FilesPlacement::restore(SavedFilesPlacement {
        presentation: FilesPresentation::Full,
        dock: FilesDock::Bottom,
        ..Default::default()
    });
    state.workbench.saved = vec![saved];
    let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
    state.tick_investigation(later, &mut ClientShellInput::default());
    assert_eq!(state.workbench.tabs.len(), 1, "the entry was restored");
    assert!(!state.investigation_active(), "restoring does not show it");
    assert!(state.toggle_investigation(&mut ClientShellInput::default()));
    state
}

#[test]
fn saved_entries_without_a_placement_load_and_reopen_split_on_the_left() {
    let old: SavedInvestigation = serde_json::from_str(
        r#"{"endpoint":"local","workspace":"ws_1","root":"/r","files":[],"active_file":null,"horizontal":false,"query":"","include":"","exclude":"","regex":false,"case_sensitive":false,"whole_word":false,"ignored":false,"content_search":false}"#,
    )
    .unwrap();
    assert_eq!(old.placement, None);
    assert!(!serde_json::to_string(&old).unwrap().contains("placement"));

    let state = reopened_after_restore(old);
    assert_eq!(
        state.workbench.placement.presentation,
        FilesPresentation::Split
    );
    assert_eq!(state.workbench.placement.dock, FilesDock::Left);
}

#[test]
fn an_unreadable_saved_placement_loads_as_the_default() {
    let saved: SavedInvestigation = serde_json::from_str(
        r#"{"endpoint":"local","workspace":"ws_1","root":"/r","files":[],"active_file":null,"horizontal":false,"query":"","include":"","exclude":"","regex":false,"case_sensitive":false,"whole_word":false,"ignored":false,"content_search":false,"placement":"sideways"}"#,
    )
    .unwrap();
    assert_eq!(saved.placement, None);
    assert_eq!(saved.root, "/r");
}

#[test]
fn a_restored_entry_reopens_with_its_saved_presentation_dock_and_ratio() {
    let saved = SavedInvestigation {
        placement: Some(SavedFilesPlacement {
            presentation: FilesPresentation::Split,
            dock: FilesDock::Right,
            side_permille: Some(450),
            bottom_permille: None,
        }),
        ..saved_in("local", "ws_1", "/repo")
    };
    let json = serde_json::to_string(&saved).unwrap();
    let saved: SavedInvestigation = serde_json::from_str(&json).unwrap();

    let state = reopened_after_restore(saved);
    assert_eq!(
        state.workbench.placement.presentation,
        FilesPresentation::Split
    );
    assert_eq!(state.workbench.placement.dock, FilesDock::Right);
    assert_eq!(state.workbench.placement.side_permille, 450);
}

#[test]
fn a_preferred_placement_from_the_preferences_file_is_the_new_shells_placement() {
    use super::super::preferences::{self, ClientChromePreferences};
    let path = std::env::temp_dir().join(format!(
        "herdr-files-placement-restore-{}.json",
        std::process::id()
    ));
    preferences::store(
        &path,
        ClientChromePreferences {
            files_placement: Some(SavedFilesPlacement {
                presentation: FilesPresentation::Split,
                dock: FilesDock::Right,
                side_permille: Some(450),
                bottom_permille: Some(250),
            }),
            ..ClientChromePreferences::default()
        },
    )
    .expect("seed files placement preferences");
    let config = ClientShellConfig::from_config(&crate::config::Config::default())
        .with_preferences_path(path.clone());

    let state = ClientShellState::new(config);
    let _ = std::fs::remove_file(&path);

    let placement = &state.workbench.placement;
    assert_eq!(placement.presentation, FilesPresentation::Split);
    assert_eq!(placement.dock, FilesDock::Right);
    assert_eq!(placement.side_permille, 450);
    assert_eq!(placement.bottom_permille, 250);
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
        assert_eq!(tab.browser.rows.len(), 3);
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
        for label in HOST_ONLY {
            let mut outcome = ClientShellInput::default();
            state.investigation_menu_action(label, &mut outcome);
            assert!(requests(&outcome).is_empty(), "menu {label}");
            assert_eq!(
                state.investigation().unwrap().notice,
                "not available inside app@devbox",
                "menu {label}"
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
        state.investigation_paste(false, &mut outcome);
        assert_eq!(
            state.investigation().unwrap().notice,
            "not available inside app@devbox"
        );
    }

    #[test]
    fn host_tabs_still_run_their_file_operations() {
        let mut state = state();
        let mut outcome = ClientShellInput::default();
        state.investigation_menu_action(TRASH, &mut outcome);
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
        let mut outcome = ClientShellInput::default();

        set_panes(&mut state, &[("pane_1", Some("app"), Some("devbox"))]);
        assert!(state.toggle_investigation(&mut outcome));
        assert_eq!(state.workbench.active, Some(0));

        state.workbench.visible = false;
        set_panes(&mut state, &[("pane_9", None, None)]);
        assert!(state.toggle_investigation(&mut outcome));
        assert_eq!(state.workbench.active, Some(1));

        state.workbench.visible = false;
        set_panes(&mut state, &[("pane_5", None, Some("other"))]);
        assert!(
            !state.toggle_investigation(&mut outcome),
            "no tab of that location yet"
        );
    }

    fn with_pane_dirs(state: &mut ClientShellState, cwd: Option<&str>, foreground: Option<&str>) {
        let mut snapshot = state.snapshot.as_deref().cloned().unwrap();
        for pane in &mut snapshot.panes {
            pane.cwd = cwd.map(str::to_owned);
            pane.foreground_cwd = foreground.map(str::to_owned);
        }
        state_set_snapshot(state, snapshot);
        state.workbench.visible = false;
    }

    fn host_state() -> ClientShellState {
        let mut state = state();
        set_panes(&mut state, &[("pane_1", None, None)]);
        state
    }

    #[test]
    fn reshowing_after_cd_roots_the_tab_at_the_panes_directory() {
        let mut state = host_state();
        with_pane_dirs(&mut state, Some("/repo"), Some("/repo/sub"));
        let mut outcome = ClientShellInput::default();

        assert!(state.toggle_investigation(&mut outcome));

        assert_eq!(
            state.workbench.tabs[0].browser.root.as_deref(),
            Some("/repo/sub")
        );
        assert!(matches!(
            &requests(&outcome)[..],
            [(_, Method::DirectoryList(list)), ..] if list.path.as_deref() == Some("/repo/sub")
        ));
    }

    #[test]
    fn reshowing_in_the_same_directory_requests_nothing() {
        let mut state = host_state();
        with_pane_dirs(&mut state, Some("/repo"), None);
        let mut outcome = ClientShellInput::default();

        assert!(state.toggle_investigation(&mut outcome));

        assert!(requests(&outcome).is_empty());
        assert_eq!(
            state.workbench.tabs[0].browser.root.as_deref(),
            Some("/repo")
        );
    }

    #[test]
    fn reshowing_keeps_the_root_when_the_directory_is_unknown() {
        let mut state = host_state();
        with_pane_dirs(&mut state, None, None);
        let mut outcome = ClientShellInput::default();

        assert!(state.toggle_investigation(&mut outcome));

        assert!(requests(&outcome).is_empty());
        assert_eq!(
            state.workbench.tabs[0].browser.root.as_deref(),
            Some("/repo")
        );
    }

    #[test]
    fn reshowing_off_host_ignores_the_host_fallback_directory() {
        let mut state = offhost_state();
        with_pane_dirs(&mut state, Some("/home/host/elsewhere"), None);
        let mut outcome = ClientShellInput::default();

        assert!(state.toggle_investigation(&mut outcome));

        assert!(requests(&outcome).is_empty());
        assert_eq!(
            state.workbench.tabs[0].browser.root.as_deref(),
            Some("/work")
        );
    }

    #[test]
    fn reshowing_off_host_follows_the_reported_directory() {
        let mut state = offhost_state();
        with_pane_dirs(&mut state, Some("/home/host/elsewhere"), Some("/work/src"));
        let mut outcome = ClientShellInput::default();

        assert!(state.toggle_investigation(&mut outcome));

        assert_eq!(
            state.workbench.tabs[0].browser.root.as_deref(),
            Some("/work/src")
        );
        assert!(matches!(
            &requests(&outcome)[..],
            [(_, Method::PaneDirectoryList(list)), ..] if list.path.as_deref() == Some("/work/src")
        ));
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

        assert!(state.toggle_investigation(&mut ClientShellInput::default()));
        assert_eq!(state.workbench.active, Some(0));
    }

    #[test]
    fn reshowing_a_host_tab_from_an_ssh_pane_keeps_its_root_on_an_old_server() {
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
        set_panes(&mut state, &[("pane_1", None, Some("devbox"))]);
        with_pane_dirs(&mut state, Some("/work"), Some("/work"));
        let root = state.workbench.tabs[0].browser.root.clone();
        let mut outcome = ClientShellInput::default();

        assert!(state.toggle_investigation(&mut outcome));

        assert_eq!(state.workbench.tabs[0].browser.root, root);
        assert!(outcome.requests.is_empty());
    }

    #[test]
    fn reshowing_matches_a_windows_pane_directory_against_the_listed_root() {
        let mut state = host_state();
        with_pane_dirs(&mut state, Some(r"C:\Users\dev\project"), None);
        state.workbench.tabs[0].browser.root = Some("C:/Users/dev/project".into());
        let mut outcome = ClientShellInput::default();

        assert!(state.toggle_investigation(&mut outcome));

        assert!(requests(&outcome).is_empty());
    }

    #[test]
    fn reshowing_roots_at_the_listing_format_of_each_windows_path_style() {
        for (reported, listed) in [
            (r"C:\Users\dev\src", "C:/Users/dev/src"),
            (r"\\?\C:\Users\dev\src", "C:/Users/dev/src"),
            (r"\\?\UNC\host\share\src", "//host/share/src"),
            (r"\\host\share\src", "//host/share/src"),
            ("C:/Users/dev/src", "C:/Users/dev/src"),
        ] {
            let mut state = host_state();
            with_pane_dirs(&mut state, Some(reported), None);
            let mut outcome = ClientShellInput::default();

            assert!(state.toggle_investigation(&mut outcome));

            assert_eq!(
                state.workbench.tabs[0].browser.root.as_deref(),
                Some(listed),
                "{reported}"
            );
        }
    }

    #[test]
    fn a_posix_directory_with_a_backslash_is_left_alone() {
        let mut state = host_state();
        with_pane_dirs(&mut state, Some(r"/srv/odd\name"), None);
        let mut outcome = ClientShellInput::default();

        assert!(state.toggle_investigation(&mut outcome));

        assert_eq!(
            state.workbench.tabs[0].browser.root.as_deref(),
            Some(r"/srv/odd\name")
        );
    }

    #[test]
    fn reshowing_roots_a_replica_tab_in_its_own_replicas_directory() {
        let mut state = offhost_state();
        let mut snapshot = state.snapshot.as_deref().cloned().unwrap();
        snapshot.panes[0].foreground_cwd = Some("/work/a".into());
        snapshot.panes[1].foreground_cwd = Some("/work/b".into());
        state_set_snapshot(&mut state, snapshot);
        state.investigation_mut().unwrap().browser.location_pane = Some("pane_2".into());
        state.workbench.visible = false;
        let mut outcome = ClientShellInput::default();

        assert!(state.toggle_investigation(&mut outcome));

        assert_eq!(
            state.workbench.tabs[0].browser.root.as_deref(),
            Some("/work/b")
        );
        assert!(matches!(
            &requests(&outcome)[..],
            [(_, Method::PaneDirectoryList(list)), ..]
                if list.pane_id == "pane_2" && list.path.as_deref() == Some("/work/b")
        ));
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

        state.investigation_menu_action(SEND_PATH, &mut outcome);

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

        state.investigation_menu_action(EDIT_FILE, &mut outcome);

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
        state.investigation_menu_action(EDIT_FILE, &mut outcome);
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

    // Listing refresh: the tree follows the server without being reloaded.

    fn dir_listing(path: &str, names: &[(&str, DirectoryEntryKind)]) -> ResponseResult {
        let ResponseResult::PaneDirectoryList(mut list) = pane_listing(path) else {
            unreachable!();
        };
        list.entries = names
            .iter()
            .map(|(name, kind)| entry(name, *kind))
            .collect();
        ResponseResult::PaneDirectoryList(list)
    }

    fn loaded_listing(
        path: &str,
        names: &[(&str, DirectoryEntryKind)],
    ) -> crate::client::shell::file_viewer::model::DirectoryState {
        use crate::client::shell::file_viewer::model::{DirectoryListing, DirectoryState};
        DirectoryState::Loaded(DirectoryListing {
            canonical: path.into(),
            entries: names
                .iter()
                .map(|(name, kind)| entry(name, *kind))
                .collect(),
            truncated: false,
            omitted: None,
            unlistable: None,
            git_error: None,
        })
    }

    const ROOT_ENTRIES: [(&str, DirectoryEntryKind); 2] = [
        ("src", DirectoryEntryKind::Dir),
        ("a.txt", DirectoryEntryKind::File),
    ];
    const SRC_ENTRIES: [(&str, DirectoryEntryKind); 1] = [("main.rs", DirectoryEntryKind::File)];

    /// `/work` with `src` expanded, a ready index and no open document.
    fn refreshable_state() -> ClientShellState {
        use crate::client::shell::file_viewer::model::FileIndex;
        let mut state = offhost_state();
        let browser = &mut state.investigation_mut().unwrap().browser;
        browser
            .directories
            .insert("/work".into(), loaded_listing("/work", &ROOT_ENTRIES));
        browser.directories.insert(
            "/work/src".into(),
            loaded_listing("/work/src", &SRC_ENTRIES),
        );
        browser.expanded.insert("/work/src".into());
        browser.index = FileIndex::Ready {
            repo_root: "/work".into(),
            paths: Vec::new(),
            truncated: false,
        };
        browser.rebuild(false);
        select(&mut state, "/work/src/main.rs");
        // The selected file is already previewed and its poll is far off, so
        // an answer starts no read of its own and only listings are on the wire.
        let tab = state.investigation_mut().unwrap();
        let mut document = Document::new("/work/src/main.rs".into());
        document.update("fn main() {}\n".into(), "cksum:1:1".into());
        tab.preview = Some(document);
        tab.last_poll = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        state
    }

    fn select(state: &mut ClientShellState, path: &str) {
        let browser = &mut state.investigation_mut().unwrap().browser;
        browser.selected = browser
            .rows
            .iter()
            .position(|row| row.entry.as_ref().is_some_and(|e| e.path == path))
            .unwrap();
    }

    fn selected_path(state: &ClientShellState) -> Option<String> {
        state
            .investigation()
            .unwrap()
            .browser
            .selected_entry()
            .map(|e| e.path.clone())
    }

    fn row_labels(state: &ClientShellState) -> Vec<String> {
        state
            .investigation()
            .unwrap()
            .browser
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect()
    }

    fn list_path(method: &Method) -> Option<&str> {
        match method {
            Method::PaneDirectoryList(list) => list.path.as_deref(),
            Method::DirectoryList(list) => list.path.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn off_host_listing_sweep_lists_shown_directories_one_after_another_each_interval() {
        let mut state = refreshable_state();

        assert!(tick(&mut state, 4).is_empty());
        let first = tick(&mut state, 6);
        let [(first_id, method)] = &first[..] else {
            panic!("{first:?}");
        };
        assert_eq!(list_path(method), Some("/work"));
        assert!(
            matches!(
                state
                    .investigation()
                    .unwrap()
                    .browser
                    .directories
                    .get("/work"),
                Some(crate::client::shell::file_viewer::model::DirectoryState::Loaded(_))
            ),
            "a refresh keeps the old rows on screen"
        );
        assert!(tick(&mut state, 12).is_empty(), "one request at a time");

        answer(
            &mut state,
            first_id,
            Ok(dir_listing("/work", &ROOT_ENTRIES)),
        );
        let second = tick(&mut state, 12);
        let [(_, method)] = &second[..] else {
            panic!("{second:?}");
        };
        assert_eq!(list_path(method), Some("/work/src"));
        answer(
            &mut state,
            &second[0].0,
            Ok(dir_listing("/work/src", &SRC_ENTRIES)),
        );

        assert!(
            tick(&mut state, 8).is_empty(),
            "the next sweep waits for its interval"
        );
        let again = tick(&mut state, 12);
        assert_eq!(list_path(&again[0].1), Some("/work"));
    }

    #[test]
    fn off_host_listing_poll_waits_for_a_hidden_investigation() {
        let mut state = refreshable_state();
        state.workbench.visible = false;

        assert!(tick(&mut state, 6).is_empty());
    }

    #[test]
    fn off_host_listing_poll_waits_while_another_request_is_pending() {
        let mut state = refreshable_state();
        let mut outcome = ClientShellInput::default();
        state.request_file_viewer_listing("/work/lib".into(), &mut outcome);

        assert!(tick(&mut state, 6).is_empty());
    }

    #[test]
    fn an_unchanged_refreshed_listing_leaves_rows_selection_and_index_alone() {
        let mut state = refreshable_state();
        let rows = row_labels(&state);
        let selected = state.investigation().unwrap().browser.selected;
        let (id, _) = tick(&mut state, 6).remove(0);

        let (repaint, _) =
            state.handle_endpoint_result("boot-1", &id, Ok(dir_listing("/work", &ROOT_ENTRIES)));

        assert!(!repaint);
        assert_eq!(row_labels(&state), rows);
        assert_eq!(state.investigation().unwrap().browser.selected, selected);
        assert!(!state.investigation().unwrap().browser.refresh.index_due);
    }

    #[test]
    fn a_changed_refreshed_listing_updates_rows_keeps_state_and_schedules_the_index() {
        let mut state = refreshable_state();
        let (id, _) = tick(&mut state, 6).remove(0);
        let mut grown = ROOT_ENTRIES.to_vec();
        grown.push(("new.txt", DirectoryEntryKind::File));

        let (repaint, _) =
            state.handle_endpoint_result("boot-1", &id, Ok(dir_listing("/work", &grown)));

        assert!(repaint);
        assert!(row_labels(&state).contains(&"new.txt".to_owned()));
        let tab = state.investigation().unwrap();
        assert!(tab.browser.expanded.contains("/work/src"));
        assert_eq!(selected_path(&state).as_deref(), Some("/work/src/main.rs"));
        let sweep = tick(&mut state, 7);
        assert_eq!(list_path(&sweep[0].1), Some("/work/src"));
        answer(
            &mut state,
            &sweep[0].0,
            Ok(dir_listing("/work/src", &SRC_ENTRIES)),
        );
        let next = tick(&mut state, 7);
        let [(index_id, Method::PaneDirectoryIndex(_))] = &next[..] else {
            panic!("{next:?}");
        };
        assert!(
            matches!(
                state.investigation().unwrap().browser.index,
                crate::client::shell::file_viewer::model::FileIndex::Ready { .. }
            ),
            "the old index serves Ctrl+P while the new one is on its way"
        );
        assert!(tick(&mut state, 8).is_empty());
        answer(
            &mut state,
            index_id,
            Ok(ResponseResult::PaneDirectoryIndex(
                crate::api::schema::PaneDirectoryIndexing {
                    location: PaneLocation {
                        kind: PaneLocationKind::Container,
                        ssh_host: Some("devbox".into()),
                        container: Some("app".into()),
                    },
                    repo_root: Some("/work".into()),
                    entries: vec!["new.txt".into()],
                    truncated: false,
                    git_error: None,
                },
            )),
        );
        let crate::client::shell::file_viewer::model::FileIndex::Ready { paths, .. } =
            &state.investigation().unwrap().browser.index
        else {
            panic!("the refreshed index replaces the old one");
        };
        assert!(paths.iter().any(|p| p.relative == "new.txt"));
    }

    #[test]
    fn a_refresh_that_only_changes_metadata_does_not_ask_for_the_index() {
        let mut state = refreshable_state();
        let (id, _) = tick(&mut state, 6).remove(0);
        let ResponseResult::PaneDirectoryList(mut list) = dir_listing("/work", &ROOT_ENTRIES)
        else {
            unreachable!();
        };
        list.entries[1].size = Some(99);

        let (repaint, _) = state.handle_endpoint_result(
            "boot-1",
            &id,
            Ok(ResponseResult::PaneDirectoryList(list)),
        );

        assert!(repaint);
        let Some(crate::client::shell::file_viewer::model::DirectoryState::Loaded(held)) = state
            .investigation()
            .unwrap()
            .browser
            .directories
            .get("/work")
        else {
            panic!("the root stays loaded");
        };
        assert_eq!(held.entries[1].size, Some(99));
        assert!(!state.investigation().unwrap().browser.refresh.index_due);
    }

    #[test]
    fn a_refresh_selects_the_nearest_surviving_ancestor_of_a_vanished_selected_row() {
        let mut state = refreshable_state();
        let (id, _) = tick(&mut state, 6).remove(0);
        answer(&mut state, &id, Ok(dir_listing("/work", &ROOT_ENTRIES)));
        let (id, _) = tick(&mut state, 12).remove(0);

        answer(
            &mut state,
            &id,
            Ok(dir_listing(
                "/work/src",
                &[("other.rs", DirectoryEntryKind::File)],
            )),
        );

        assert_eq!(selected_path(&state).as_deref(), Some("/work/src"));
    }

    #[test]
    fn an_expanded_directory_that_vanished_from_its_parent_is_dropped() {
        let mut state = refreshable_state();
        let (id, _) = tick(&mut state, 6).remove(0);

        answer(
            &mut state,
            &id,
            Ok(dir_listing("/work", &[("a.txt", DirectoryEntryKind::File)])),
        );

        let tab = state.investigation().unwrap();
        assert!(!tab.browser.expanded.contains("/work/src"));
        assert!(!tab.browser.directories.contains_key("/work/src"));
    }

    #[test]
    fn a_refreshed_directory_the_server_no_longer_finds_is_dropped_with_its_children() {
        let mut state = refreshable_state();
        let browser = &mut state.investigation_mut().unwrap().browser;
        browser.directories.insert(
            "/work/src/deep".into(),
            loaded_listing("/work/src/deep", &[]),
        );
        browser.expanded.insert("/work/src/deep".into());
        let (id, _) = tick(&mut state, 6).remove(0);
        answer(&mut state, &id, Ok(dir_listing("/work", &ROOT_ENTRIES)));
        let (id, method) = tick(&mut state, 12).remove(0);
        assert_eq!(list_path(&method), Some("/work/src"));

        answer(
            &mut state,
            &id,
            Err(ClientShellEndpointError {
                code: Some("not_found".into()),
                message: "gone".into(),
            }),
        );

        let tab = state.investigation().unwrap();
        assert!(tab.browser.expanded.is_empty());
        assert!(!tab.browser.directories.contains_key("/work/src"));
        assert!(!tab.browser.directories.contains_key("/work/src/deep"));
    }

    #[test]
    fn a_failed_root_refresh_keeps_the_old_listing_and_keeps_polling() {
        let mut state = refreshable_state();
        let rows = row_labels(&state);
        let (id, _) = tick(&mut state, 6).remove(0);

        answer(
            &mut state,
            &id,
            Err(ClientShellEndpointError {
                code: Some("permission_denied".into()),
                message: "denied".into(),
            }),
        );

        assert_eq!(row_labels(&state), rows);
        assert_eq!(
            state.investigation().unwrap().browser.notice.as_deref(),
            Some("refresh failed: permission denied")
        );
        assert!(!tick(&mut state, 12).is_empty());
    }

    #[test]
    fn a_transient_failure_of_a_nested_refresh_keeps_the_directory_open() {
        let mut state = refreshable_state();
        let (id, _) = tick(&mut state, 6).remove(0);
        answer(&mut state, &id, Ok(dir_listing("/work", &ROOT_ENTRIES)));
        let (id, _) = tick(&mut state, 12).remove(0);

        answer(
            &mut state,
            &id,
            Err(ClientShellEndpointError {
                code: Some("directory_busy".into()),
                message: "busy".into(),
            }),
        );

        assert!(state
            .investigation()
            .unwrap()
            .browser
            .expanded
            .contains("/work/src"));
    }

    fn drain_manual_refresh(state: &mut ClientShellState) -> Vec<Method> {
        let mut sent = Vec::new();
        for _ in 0..6 {
            let batch = tick(state, 0);
            if batch.is_empty() {
                break;
            }
            for (id, method) in batch {
                let result = match &method {
                    Method::PaneDirectoryList(list) => {
                        let path = list.path.clone().unwrap();
                        if path == "/work" {
                            dir_listing(&path, &ROOT_ENTRIES)
                        } else {
                            dir_listing(&path, &SRC_ENTRIES)
                        }
                    }
                    Method::PaneFileRead(_) => {
                        sent.push(method);
                        answer(
                            state,
                            &id,
                            Err(ClientShellEndpointError {
                                code: Some("not_found".into()),
                                message: "gone".into(),
                            }),
                        );
                        continue;
                    }
                    _ => ResponseResult::PaneDirectoryIndex(
                        crate::api::schema::PaneDirectoryIndexing {
                            location: PaneLocation {
                                kind: PaneLocationKind::Container,
                                ssh_host: Some("devbox".into()),
                                container: Some("app".into()),
                            },
                            repo_root: Some("/work".into()),
                            entries: Vec::new(),
                            truncated: false,
                            git_error: None,
                        },
                    ),
                };
                sent.push(method);
                answer(state, &id, Ok(result));
            }
        }
        sent
    }

    #[test]
    fn f5_and_the_menu_entry_relist_every_shown_directory_and_the_index_one_at_a_time() {
        use crossterm::event::KeyModifiers as M;
        for via_menu in [false, true] {
            let mut state = refreshable_state();
            state.investigation_mut().unwrap().last_poll = std::time::Instant::now();
            if via_menu {
                let mut outcome = ClientShellInput::default();
                state.investigation_menu_action(REFRESH_FILES, &mut outcome);
            } else {
                press(&mut state, KeyCode::F(5), M::empty());
            }
            let tab = state.investigation().unwrap();
            assert!(tab.last_poll.elapsed() >= std::time::Duration::from_secs(9));
            assert!(
                matches!(
                    tab.browser.directories.get("/work"),
                    Some(crate::client::shell::file_viewer::model::DirectoryState::Loaded(_))
                ),
                "no Loading flicker"
            );

            let sent = drain_manual_refresh(&mut state);

            assert!(
                matches!(sent[0], Method::PaneFileRead(_)),
                "the open document is read at once"
            );
            let paths: Vec<_> = sent.iter().filter_map(list_path).collect();
            assert_eq!(paths, ["/work", "/work/src"]);
            assert!(matches!(sent.last(), Some(Method::PaneDirectoryIndex(_))));
            assert_eq!(sent.len(), 4);
            let tab = state.investigation().unwrap();
            assert!(tab.browser.expanded.contains("/work/src"));
            assert_eq!(selected_path(&state).as_deref(), Some("/work/src/main.rs"));
        }
    }

    #[test]
    fn host_tabs_poll_listings_through_directory_list_every_second() {
        let mut state = state();

        let document = tick(&mut state, 2);
        assert!(
            matches!(&document[..], [(_, Method::FileRead(_))]),
            "{document:?}"
        );
        let listing = tick(&mut state, 2);
        let [(_, method)] = &listing[..] else {
            panic!("{listing:?}");
        };
        assert!(matches!(method, Method::DirectoryList(_)));
        assert_eq!(list_path(method), Some("/repo"));
        assert!(tick(&mut state, 3)
            .iter()
            .all(|(_, m)| list_path(m).is_none()));
    }

    #[test]
    fn rebinding_refresh_moves_it_to_the_configured_key_and_disables_f5() {
        use crossterm::event::KeyModifiers as M;
        let mut state = state();
        state
            .config
            .file_workbench_keys
            .insert("refresh".into(), "ctrl+r".into());

        press(&mut state, KeyCode::F(5), M::empty());
        assert!(state
            .investigation()
            .unwrap()
            .browser
            .refresh
            .queue
            .is_empty());
        assert!(!state.investigation().unwrap().browser.refresh.index_due);

        press(&mut state, KeyCode::Char('r'), M::CONTROL);
        assert!(!state
            .investigation()
            .unwrap()
            .browser
            .refresh
            .queue
            .is_empty());
    }

    #[test]
    fn the_refresh_header_button_refreshes_and_never_overlaps_the_files_menu_button() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        for width in [120u16, 60, 36] {
            let mut state = refreshable_state();
            state.set_pane_surface(crate::client::shell::tests::surface());
            state.compose(width, 32).expect("composed investigation");
            let buttons = state.investigation_hits.buttons.clone();
            let actions = buttons.iter().find(|(_, i)| i.is_none()).unwrap().0;
            let refresh = buttons
                .iter()
                .find(|(_, i)| *i == Some(REFRESH_FILES))
                .map(|(r, _)| *r);
            if width == 36 {
                assert!(refresh.is_none(), "no room beside the title");
                continue;
            }
            let refresh = refresh.expect("[F5 refresh] is shown");
            assert!(refresh.right() < actions.x, "{refresh:?} {actions:?}");

            let mut outcome = ClientShellInput::default();
            state.handle_investigation_mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: refresh.x + 1,
                    row: refresh.y,
                    modifiers: crossterm::event::KeyModifiers::empty(),
                },
                &mut outcome,
            );

            assert!(state.investigation().unwrap().browser.refresh.index_due);
        }
    }

    #[test]
    fn the_refresh_menu_entry_still_works_inside_an_environment() {
        let mut state = refreshable_state();
        let mut outcome = ClientShellInput::default();

        state.investigation_menu_action(REFRESH_FILES, &mut outcome);

        assert!(state.investigation().unwrap().notice.is_empty());
        assert!(!state
            .investigation()
            .unwrap()
            .browser
            .refresh
            .queue
            .is_empty());
    }

    fn refresh_error(code: &str) -> Result<ResponseResult, ClientShellEndpointError> {
        Err(ClientShellEndpointError {
            code: Some(code.into()),
            message: code.into(),
        })
    }

    fn index_answer(
        repo_root: Option<&str>,
        git_error: Option<&str>,
    ) -> Result<ResponseResult, ClientShellEndpointError> {
        Ok(ResponseResult::PaneDirectoryIndex(
            crate::api::schema::PaneDirectoryIndexing {
                location: PaneLocation {
                    kind: PaneLocationKind::Container,
                    ssh_host: Some("devbox".into()),
                    container: Some("app".into()),
                },
                repo_root: repo_root.map(str::to_owned),
                entries: Vec::new(),
                truncated: false,
                git_error: git_error.map(str::to_owned),
            },
        ))
    }

    /// Ticks `seconds` ahead, answering every other request unchanged, until
    /// `wanted` asks for something; returns that request's id.
    fn request_until(
        state: &mut ClientShellState,
        seconds: u64,
        wanted: impl Fn(&Method) -> bool,
    ) -> String {
        for _ in 0..6 {
            for (id, method) in tick(state, seconds) {
                if wanted(&method) {
                    return id;
                }
                let result = match &method {
                    Method::PaneDirectoryList(list) if list.path.as_deref() == Some("/work") => {
                        Ok(dir_listing("/work", &ROOT_ENTRIES))
                    }
                    Method::PaneDirectoryList(list) => {
                        Ok(dir_listing(list.path.as_deref().unwrap(), &SRC_ENTRIES))
                    }
                    _ => refresh_error("not_found"),
                };
                answer(state, &id, result);
            }
        }
        panic!("no matching request");
    }

    fn pending_index_refresh(state: &mut ClientShellState) -> String {
        state.refresh_investigation_files();
        request_until(state, 0, |m| matches!(m, Method::PaneDirectoryIndex(_)))
    }

    #[test]
    fn a_failed_or_empty_index_refresh_keeps_the_old_index_and_clears_its_flight() {
        for result in [
            refresh_error("endpoint_timeout"),
            index_answer(Some("/work"), Some("git failed")),
            index_answer(None, None),
        ] {
            let mut state = refreshable_state();
            let id = pending_index_refresh(&mut state);
            assert!(
                state
                    .investigation()
                    .unwrap()
                    .browser
                    .refresh
                    .index_in_flight
            );

            answer(&mut state, &id, result);

            let browser = &state.investigation().unwrap().browser;
            assert!(!browser.refresh.index_in_flight);
            assert!(matches!(
                browser.index,
                crate::client::shell::file_viewer::model::FileIndex::Ready { .. }
            ));
        }
    }

    #[test]
    fn the_index_refresh_is_rate_limited_but_f5_bypasses_the_limit() {
        let is_index = |m: &Method| matches!(m, Method::PaneDirectoryIndex(_));
        let mut state = refreshable_state();
        let id = pending_index_refresh(&mut state);
        answer(&mut state, &id, index_answer(Some("/work"), None));

        // Another change soon after: the index waits out the 30 seconds.
        state.investigation_mut().unwrap().browser.refresh.index_due = true;
        for seconds in [8, 14, 20] {
            for (id, method) in tick(&mut state, seconds) {
                assert!(!is_index(&method), "{seconds}s: {method:?}");
                match list_path(&method) {
                    Some(path) => answer(&mut state, &id, Ok(dir_listing(path, &ROOT_ENTRIES))),
                    None => answer(&mut state, &id, refresh_error("not_found")),
                }
            }
        }
        assert!(state.investigation().unwrap().browser.refresh.index_due);
        let id = request_until(&mut state, 60, is_index);
        answer(&mut state, &id, index_answer(Some("/work"), None));

        // F5 asks at once, however recent the last request was.
        let id = pending_index_refresh(&mut state);
        answer(&mut state, &id, index_answer(Some("/work"), None));
        let id = pending_index_refresh(&mut state);
        answer(&mut state, &id, index_answer(Some("/work"), None));
    }

    #[test]
    fn a_background_refresh_answer_leaves_a_scrolled_pinned_document_and_a_selection_alone() {
        let mut state = refreshable_state();
        let text = "a\nb\nc\nd\ne\nf\n";
        let tab = state.investigation_mut().unwrap();
        let mut pinned = Document::new("/work/src/main.rs".into());
        pinned.update(text.into(), "cksum:1:1".into());
        pinned.cursor = 10;
        pinned.scroll = 0;
        tab.documents.push(pinned);
        tab.active_file = Some(0);
        tab.focus = Focus::Body;
        let mut grown = ROOT_ENTRIES.to_vec();
        grown.push(("new.txt", DirectoryEntryKind::File));

        let (id, _) = tick(&mut state, 6).remove(0);
        answer(&mut state, &id, Ok(dir_listing("/work", &ROOT_ENTRIES)));
        assert_eq!(state.investigation().unwrap().documents[0].scroll, 0);
        let (id, _) = tick(&mut state, 12).remove(0);
        answer(&mut state, &id, Ok(dir_listing("/work/src", &SRC_ENTRIES)));
        let (id, _) = tick(&mut state, 18).remove(0);
        answer(&mut state, &id, Ok(dir_listing("/work", &grown)));
        assert_eq!(state.investigation().unwrap().documents[0].scroll, 0);

        // The unpinned preview keeps its selection through the same answers.
        let mut state = refreshable_state();
        let tab = state.investigation_mut().unwrap();
        let preview = tab.preview.as_mut().unwrap();
        preview.update(text.into(), "cksum:1:2".into());
        preview.anchor = Some(0);
        preview.cursor = 4;
        let (id, _) = tick(&mut state, 6).remove(0);
        answer(&mut state, &id, Ok(dir_listing("/work", &ROOT_ENTRIES)));
        assert_eq!(
            state
                .investigation()
                .unwrap()
                .preview
                .as_ref()
                .unwrap()
                .anchor,
            Some(0)
        );
        let (id, _) = tick(&mut state, 12).remove(0);
        let more = [
            ("main.rs", DirectoryEntryKind::File),
            ("lib.rs", DirectoryEntryKind::File),
        ];
        answer(&mut state, &id, Ok(dir_listing("/work/src", &more)));
        assert_eq!(
            state
                .investigation()
                .unwrap()
                .preview
                .as_ref()
                .unwrap()
                .anchor,
            Some(0)
        );
    }

    #[test]
    fn a_successful_root_refresh_clears_only_its_own_failure_notice() {
        let is_root = |m: &Method| list_path(m) == Some("/work");
        let mut state = refreshable_state();
        let id = request_until(&mut state, 6, is_root);
        answer(&mut state, &id, refresh_error("permission_denied"));
        assert!(state.investigation().unwrap().browser.notice.is_some());
        let id = request_until(&mut state, 12, is_root);

        let (repaint, _) =
            state.handle_endpoint_result("boot-1", &id, Ok(dir_listing("/work", &ROOT_ENTRIES)));

        assert!(repaint);
        assert_eq!(state.investigation().unwrap().browser.notice, None);

        let id = request_until(&mut state, 18, is_root);
        answer(&mut state, &id, refresh_error("permission_denied"));
        state.investigation_mut().unwrap().browser.notice = Some("copied".into());
        let id = request_until(&mut state, 24, is_root);
        answer(&mut state, &id, Ok(dir_listing("/work", &ROOT_ENTRIES)));
        assert_eq!(
            state.investigation().unwrap().browser.notice.as_deref(),
            Some("copied")
        );
    }

    #[test]
    fn collapsing_a_folder_stops_polling_the_folders_expanded_beneath_it() {
        let mut state = refreshable_state();
        let browser = &mut state.investigation_mut().unwrap().browser;
        browser.directories.insert(
            "/work/src/deep".into(),
            loaded_listing("/work/src/deep", &[]),
        );
        browser.expanded.insert("/work/src/deep".into());
        assert_eq!(
            browser.refresh_targets(),
            ["/work", "/work/src", "/work/src/deep"]
        );

        browser.expanded.remove("/work/src");

        assert_eq!(browser.refresh_targets(), ["/work"]);
        assert!(browser.expanded.contains("/work/src/deep"), "not pruned");
    }

    #[test]
    fn f5_leaves_the_content_search_results_alone() {
        let mut state = refreshable_state();
        let tab = state.investigation_mut().unwrap();
        tab.content_search = true;
        tab.search_ready = true;
        tab.omitted.insert(1);

        state.refresh_investigation_files();

        let tab = state.investigation().unwrap();
        assert!(tab.search_ready);
        assert!(tab.search_due.is_none());
        assert!(tab.omitted.contains(&1));
    }

    #[test]
    fn download_opens_in_an_off_host_tab_and_reads_through_the_locations_pane() {
        let mut state = offhost_state();
        state.investigation_mut().unwrap().browser.location_pane = Some("pane_2".into());
        state
            .investigation_mut()
            .unwrap()
            .selected_files
            .insert("/etc/hosts".into());
        state.set_download_dir("~/Downloads".into());
        let mut outcome = ClientShellInput::default();

        state.investigation_menu_action(DOWNLOAD_ITEM, &mut outcome);

        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Input {
                purpose: InputPurpose::Download,
                ..
            })
        ));
        assert_eq!(state.investigation().unwrap().notice, "");
        press(
            &mut state,
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );
        let queued = state.take_download_requests();
        assert_eq!(
            queued[0].place,
            crate::client::shell::WorkbenchDropPlace::Pane("pane_2".into())
        );
        assert_ne!(
            queued[0].location,
            crate::client::shell::file_viewer::model::FileLocation::to_pane_location(None)
        );
    }

    #[test]
    fn download_without_a_live_pane_says_which_location_has_none() {
        let mut state = offhost_state();
        set_panes(&mut state, &[("pane_9", None, None)]);
        state.set_download_dir("~/d".into());
        state
            .investigation_mut()
            .unwrap()
            .selected_files
            .insert("/etc/hosts".into());
        let mut outcome = ClientShellInput::default();
        state.investigation_menu_action(DOWNLOAD_ITEM, &mut outcome);

        press(
            &mut state,
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );

        let queued = state.take_download_requests();
        assert_eq!(
            queued[0].place,
            crate::client::shell::WorkbenchDropPlace::NoPane(
                "no pane running in app@devbox".into()
            )
        );
    }

    #[test]
    fn an_off_host_tree_drop_goes_through_a_live_pane_of_its_location() {
        let mut state = offhost_state();
        state.investigation_mut().unwrap().browser.location_pane = Some("pane_2".into());

        let target = state.workbench_drop_target().expect("tree accepts drops");

        assert_eq!(
            target.place,
            crate::client::shell::WorkbenchDropPlace::Pane("pane_2".into())
        );
    }

    #[test]
    fn an_off_host_tree_without_a_live_pane_names_the_missing_location() {
        let mut state = offhost_state();
        set_panes(&mut state, &[("pane_9", None, None)]);

        let target = state.workbench_drop_target().expect("tree accepts drops");

        assert_eq!(
            target.place,
            crate::client::shell::WorkbenchDropPlace::NoPane(
                "no pane running in app@devbox".into()
            )
        );
    }
}

// ---- saving selected entries on this machine (download design §5) ----

mod download {
    use super::*;
    use crate::client::shell::{ClientDownloadRequest, WorkbenchDropPlace};

    fn select_files(state: &mut ClientShellState, paths: &[&str]) {
        let tab = state.investigation_mut().unwrap();
        tab.selected_files = paths.iter().map(|p| (*p).to_owned()).collect();
    }

    fn open(state: &mut ClientShellState) {
        let mut outcome = ClientShellInput::default();
        state.investigation_menu_action(DOWNLOAD_ITEM, &mut outcome);
    }

    fn dialog_text(state: &ClientShellState) -> Option<String> {
        match &state.investigation().unwrap().dialog {
            Some(Dialog::Input { input, .. }) => Some(input.to_string()),
            _ => None,
        }
    }

    fn requests(state: &mut ClientShellState) -> Vec<ClientDownloadRequest> {
        state.take_download_requests()
    }

    #[test]
    fn file_and_directory_menus_offer_download_and_it_works_off_host() {
        assert!(MenuTarget::File
            .labels(false, FilesPresentation::Full, FilesDock::Left)
            .contains(&DOWNLOAD_ITEM));
        assert!(MenuTarget::File
            .labels(true, FilesPresentation::Full, FilesDock::Left)
            .contains(&DOWNLOAD_ITEM));
        assert!(MenuTarget::Directory
            .labels(false, FilesPresentation::Full, FilesDock::Left)
            .contains(&DOWNLOAD_ITEM));
        assert!(!HOST_ONLY.contains(&DOWNLOAD_ITEM));
    }

    #[test]
    fn the_menu_item_opens_an_input_that_starts_at_the_download_folder_for_every_selected_path() {
        let mut state = state();
        state.set_download_dir("/home/me/Downloads".into());
        select_files(&mut state, &["/repo/a.txt", "/repo/src"]);

        open(&mut state);

        match &state.investigation().unwrap().dialog {
            Some(Dialog::Input {
                title,
                input,
                purpose,
                sources,
            }) => {
                assert_eq!(*purpose, InputPurpose::Download);
                assert_eq!(input.to_string(), "/home/me/Downloads");
                assert_eq!(sources, &["/repo/a.txt", "/repo/src"]);
                assert_eq!(title, "download 2 items to");
            }
            other => panic!("expected the download input, got {other:?}"),
        }
    }

    #[test]
    fn without_a_known_download_folder_the_input_is_empty() {
        let mut state = state();
        select_files(&mut state, &["/repo/a.txt"]);

        open(&mut state);

        assert_eq!(dialog_text(&state).as_deref(), Some(""));
        assert!(state.investigation().unwrap().notice.is_empty());
    }

    #[test]
    fn a_server_without_download_methods_is_refused_before_the_dialog() {
        let mut state = state();
        select_files(&mut state, &["/repo/a.txt"]);
        state.set_endpoint_methods(Some(vec!["pane.close".into()]));

        open(&mut state);

        let tab = state.investigation().unwrap();
        assert!(tab.dialog.is_none());
        assert_eq!(
            tab.notice,
            "this server cannot send files; update Herdr there"
        );
    }

    #[test]
    fn a_folder_that_is_not_absolute_keeps_the_dialog_open_with_the_reason() {
        let mut state = state();
        state.set_download_dir("/home/me/Downloads".into());
        select_files(&mut state, &["/repo/a.txt"]);
        open(&mut state);
        let tab = state.investigation_mut().unwrap();
        if let Some(Dialog::Input { input, .. }) = &mut tab.dialog {
            *input = TextEditor::new("Downloads", true);
        }

        press(
            &mut state,
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );

        assert_eq!(dialog_text(&state).as_deref(), Some("Downloads"));
        assert_eq!(
            state.investigation().unwrap().notice,
            "enter an absolute folder"
        );
        assert!(requests(&mut state).is_empty());
    }

    #[test]
    fn nothing_selected_opens_nothing() {
        let mut state = state();
        let tab = state.investigation_mut().unwrap();
        tab.browser.rows.clear();
        tab.preview = None;

        open(&mut state);

        assert!(state.investigation().unwrap().dialog.is_none());
    }

    #[test]
    fn names_that_would_collide_on_this_machine_are_refused_before_the_dialog() {
        for (paths, names) in [
            (["/a/x", "/b/x"], "x, x"),
            (["/a/Makefile", "/a/makefile"], "Makefile, makefile"),
        ] {
            let mut state = state();
            select_files(&mut state, &paths);

            open(&mut state);

            let tab = state.investigation().unwrap();
            assert!(tab.dialog.is_none(), "{paths:?}");
            assert_eq!(
                tab.notice,
                format!("two selected items would get the same name on this machine: {names}")
            );
        }
    }

    #[test]
    fn too_many_or_too_long_paths_are_refused_before_the_dialog() {
        let long = format!("/{}", "x".repeat(4_100));
        let many: Vec<String> = (0..1_025).map(|i| format!("/repo/f{i}")).collect();
        for paths in [vec![long], many] {
            let mut state = state();
            let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
            select_files(&mut state, &paths);

            open(&mut state);

            let tab = state.investigation().unwrap();
            assert!(tab.dialog.is_none());
            assert!(!tab.notice.is_empty());
        }
    }

    #[test]
    fn pasting_while_the_dialog_is_open_types_into_the_input_and_never_uploads() {
        let mut state = state();
        state.set_download_dir("/d".into());
        select_files(&mut state, &["/repo/a.txt"]);
        open(&mut state);

        assert!(state.workbench_drop_target().is_none());
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Paste("/more".into())]);

        // The editor opens with its text selected, so a paste replaces it.
        assert_eq!(dialog_text(&state).as_deref(), Some("/more"));
        assert!(requests(&mut state).is_empty());
    }

    #[test]
    fn enter_queues_one_request_for_the_host_with_the_text_as_typed() {
        let mut state = state();
        state.set_download_dir("~/Downloads".into());
        select_files(&mut state, &["/repo/a.txt", "/repo/src"]);
        open(&mut state);
        let tab_id = state.investigation().unwrap().id;

        press(
            &mut state,
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );

        let queued = requests(&mut state);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].tab, tab_id);
        assert_eq!(queued[0].place, WorkbenchDropPlace::Host);
        assert_eq!(queued[0].paths, ["/repo/a.txt", "/repo/src"]);
        assert_eq!(queued[0].destination, "~/Downloads");
        assert_eq!(
            queued[0].location,
            crate::client::shell::file_viewer::model::FileLocation::to_pane_location(None)
        );
        let tab = state.investigation().unwrap();
        assert!(tab.dialog.is_none());
        assert_eq!(tab.notice, "preparing download…");
    }

    #[test]
    fn escape_queues_nothing() {
        let mut state = state();
        select_files(&mut state, &["/repo/a.txt"]);
        open(&mut state);

        press(
            &mut state,
            KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        );

        assert!(requests(&mut state).is_empty());
        assert!(state.investigation().unwrap().dialog.is_none());
    }

    #[test]
    fn the_change_root_input_is_a_root_purpose_and_enter_closes_it_without_a_download() {
        let mut state = state();
        let mut outcome = ClientShellInput::default();
        state.investigation_menu_action(CHANGE_ROOT, &mut outcome);

        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Input {
                purpose: InputPurpose::Root,
                ..
            })
        ));
        press(
            &mut state,
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );
        assert!(requests(&mut state).is_empty());
        assert!(state.investigation().unwrap().dialog.is_none());
    }
}

// ---- dropping files onto the tree (spec Q1, Q4, Q10, Q18) ----

mod drops {
    use super::*;
    use crate::api::schema::{DirectoryEntry, DirectoryEntryKind, Method, PaneUploadCollision};
    use crate::client::shell::file_viewer::model::{DirectoryListing, DirectoryState};
    use crate::client::shell::{ClientUploadDecision, ClientUploadQuestion, WorkbenchDropPlace};

    fn listing(entries: &[(&str, DirectoryEntryKind)]) -> DirectoryState {
        DirectoryState::Loaded(DirectoryListing {
            canonical: "/repo".into(),
            entries: entries
                .iter()
                .map(|(name, kind)| DirectoryEntry {
                    name: (*name).into(),
                    kind: *kind,
                    target_kind: None,
                    size: None,
                    mtime: None,
                    hidden: false,
                    ignored: Some(false),
                })
                .collect(),
            truncated: false,
            omitted: None,
            unlistable: None,
            git_error: None,
        })
    }

    /// The tree of `/repo` holding `src/` and `a.txt`, with `path` selected.
    fn tree_with_selected(path: Option<&str>) -> ClientShellState {
        let mut state = state();
        let tab = state.investigation_mut().unwrap();
        tab.browser.directories.insert(
            "/repo".into(),
            listing(&[
                ("src", DirectoryEntryKind::Dir),
                ("a.txt", DirectoryEntryKind::File),
            ]),
        );
        tab.browser.rebuild(false);
        match path {
            Some(path) => {
                tab.browser.selected = tab
                    .browser
                    .rows
                    .iter()
                    .position(|row| row.entry.as_ref().is_some_and(|e| e.path == path))
                    .expect("row of the path");
            }
            None => tab.browser.rows.clear(),
        }
        state
    }

    fn requests(outcome: &ClientShellInput) -> Vec<Method> {
        outcome
            .actions
            .iter()
            .filter_map(|action| match action {
                ClientShellAction::Endpoint { request, .. } => Some(request.method.clone()),
                _ => None,
            })
            .collect()
    }

    fn question(job: u64, conflicts: &[&str]) -> ClientUploadQuestion {
        ClientUploadQuestion {
            job,
            destination: "/repo/src".into(),
            summary: "2 items · 0.0 MB".into(),
            conflicts: conflicts.iter().map(|name| (*name).to_owned()).collect(),
            note: None,
        }
    }

    fn tab_id(state: &ClientShellState) -> u64 {
        state.investigation().unwrap().id
    }

    #[test]
    fn a_drop_on_a_selected_folder_goes_into_that_folder() {
        let state = tree_with_selected(Some("/repo/src"));

        let target = state.workbench_drop_target().expect("tree accepts drops");

        assert_eq!(target.destination, "/repo/src");
        assert_eq!(target.place, WorkbenchDropPlace::Host);
    }

    #[test]
    fn a_drop_on_a_selected_file_goes_into_its_parent() {
        let state = tree_with_selected(Some("/repo/a.txt"));

        assert_eq!(state.workbench_drop_target().unwrap().destination, "/repo");
    }

    #[test]
    fn a_drop_with_nothing_selected_goes_into_the_root() {
        let state = tree_with_selected(None);

        assert_eq!(state.workbench_drop_target().unwrap().destination, "/repo");
    }

    #[test]
    fn only_the_focused_tree_without_a_dialog_takes_drops() {
        let mut state = tree_with_selected(None);
        state.investigation_mut().unwrap().focus = Focus::Query;
        assert!(state.workbench_drop_target().is_none(), "a text field");

        let mut state = tree_with_selected(None);
        state.investigation_mut().unwrap().dialog =
            Some(Dialog::menu(MenuTarget::Tree, MenuPlacement::Selection));
        assert!(state.workbench_drop_target().is_none(), "an open dialog");

        let mut state = tree_with_selected(None);
        state.investigation_mut().unwrap().content_search = true;
        assert!(state.workbench_drop_target().is_none(), "search results");

        let mut state = tree_with_selected(None);
        state.workbench.visible = false;
        assert!(state.workbench_drop_target().is_none(), "terminals shown");
    }

    #[test]
    fn a_paste_into_a_text_field_still_types_the_paths() {
        let mut state = tree_with_selected(None);
        state.investigation_mut().unwrap().focus = Focus::Query;

        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Paste(
            "/tmp/a.txt".into(),
        )]);

        assert_eq!(state.investigation().unwrap().query.as_str(), "/tmp/a.txt");
    }

    #[test]
    fn the_shown_workbench_keeps_image_drops_away_from_the_hidden_pane() {
        let state = tree_with_selected(None);

        assert!(state.clipboard_image_target().is_none());
    }

    #[test]
    fn upload_conflicts_offer_replace_keep_both_and_cancel_mapped_to_collisions() {
        for (key, expected) in [
            (KeyCode::Char('1'), Some(PaneUploadCollision::Replace)),
            (KeyCode::Char('2'), Some(PaneUploadCollision::Rename)),
            (KeyCode::Char('3'), None),
        ] {
            let mut state = tree_with_selected(None);
            let tab = tab_id(&state);
            assert!(state.open_workbench_upload_dialog(tab, question(4, &["a.txt"])));

            press(&mut state, key, crossterm::event::KeyModifiers::NONE);

            assert_eq!(
                state.take_upload_decisions(),
                [ClientUploadDecision::Collision {
                    job: 4,
                    collision: expected
                }],
                "{key:?}"
            );
            assert!(state.investigation().unwrap().dialog.is_none());
        }
    }

    #[test]
    fn an_upload_without_conflicts_publishes_with_fail_so_a_late_conflict_asks_again() {
        let mut state = tree_with_selected(None);
        let tab = tab_id(&state);
        state.open_workbench_upload_dialog(tab, question(4, &[]));

        press(
            &mut state,
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );

        assert_eq!(
            state.take_upload_decisions(),
            [ClientUploadDecision::Collision {
                job: 4,
                collision: Some(PaneUploadCollision::Fail)
            }]
        );
        assert_eq!(state.investigation().unwrap().notice, "uploading…");
    }

    #[test]
    fn a_question_asked_again_after_a_late_conflict_offers_all_three_choices() {
        let mut state = tree_with_selected(None);
        let tab = tab_id(&state);
        state.open_workbench_upload_dialog(
            tab,
            ClientUploadQuestion {
                note: Some("already exists in the destination: a.txt".into()),
                ..question(4, &[])
            },
        );

        let Some(Dialog::Upload(dialog)) = &state.investigation().unwrap().dialog else {
            panic!("upload dialog");
        };
        assert_eq!(dialog.choices()[0], UploadChoice::Replace);
    }

    #[test]
    fn dismissing_the_upload_dialog_any_way_cancels_the_upload() {
        let mut state = tree_with_selected(None);
        let tab = tab_id(&state);
        state.open_workbench_upload_dialog(tab, question(4, &["a.txt"]));
        press(
            &mut state,
            KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        );
        assert_eq!(
            state.take_upload_decisions(),
            [ClientUploadDecision::Collision {
                job: 4,
                collision: None
            }]
        );

        let mut state = tree_with_selected(None);
        let tab = tab_id(&state);
        state.open_workbench_upload_dialog(tab, question(5, &[]));
        state.remove_investigation(0);
        assert_eq!(
            state.take_upload_decisions(),
            [ClientUploadDecision::Collision {
                job: 5,
                collision: None
            }]
        );
        assert!(state.take_upload_decisions().is_empty(), "asked once");
    }

    #[test]
    fn a_tab_with_another_dialog_open_refuses_the_question() {
        let mut state = tree_with_selected(None);
        let tab = tab_id(&state);
        state.investigation_mut().unwrap().dialog =
            Some(Dialog::menu(MenuTarget::Tree, MenuPlacement::Selection));

        assert!(!state.open_workbench_upload_dialog(tab, question(4, &[])));
        assert!(state.take_upload_decisions().is_empty());
    }

    #[test]
    fn the_upload_dialog_shows_the_destination_and_conflicts() {
        let mut state = tree_with_selected(None);
        let tab = tab_id(&state);
        state.open_workbench_upload_dialog(tab, question(4, &["a.txt"]));

        state.set_pane_surface(crate::client::shell::tests::surface());
        let frame = state.compose(100, 30).expect("composed investigation");
        let text: String = frame
            .cells
            .chunks(frame.width as usize)
            .map(|cells| cells.iter().map(|c| c.symbol.as_str()).collect::<String>() + "\n")
            .collect();

        assert!(text.contains("/repo/src"), "{text}");
        assert!(text.contains("a.txt"), "{text}");
        assert!(text.contains("[keep both]"), "{text}");
    }

    #[test]
    fn a_finished_upload_lists_the_folder_again_and_selects_the_first_name() {
        let mut state = tree_with_selected(None);
        let tab = tab_id(&state);

        assert!(state.workbench_upload_done(tab, "/repo".into(), vec!["b (1).txt".into()]));
        let mut outcome = ClientShellInput::default();
        state.tick_investigation(std::time::Instant::now(), &mut outcome);

        assert!(requests(&outcome).iter().any(|method| matches!(method,
            Method::DirectoryList(params) if params.path.as_deref() == Some("/repo"))));
        assert_eq!(
            state
                .investigation()
                .unwrap()
                .browser
                .pending_select
                .as_deref(),
            Some("/repo/b (1).txt")
        );
        let mut again = ClientShellInput::default();
        state.tick_investigation(std::time::Instant::now(), &mut again);
        assert!(!requests(&again).iter().any(|method| matches!(method,
            Method::DirectoryList(params) if params.path.as_deref() == Some("/repo"))));
    }

    #[test]
    fn upload_notices_reach_only_their_tab() {
        let mut state = tree_with_selected(None);
        let tab = tab_id(&state);

        assert!(state.set_workbench_notice(tab, "upload failed: disk full".into()));
        assert!(!state.set_workbench_notice(tab + 100, "elsewhere".into()));

        assert_eq!(
            state.investigation().unwrap().notice,
            "upload failed: disk full"
        );
    }
}

mod parent_row {
    use super::*;
    use crate::api::schema::{DirectoryEntry, DirectoryEntryKind};
    use crate::client::shell::file_viewer::model::DirectoryState;
    use crossterm::event::{KeyModifiers as M, MouseButton, MouseEvent, MouseEventKind};

    /// `/repo` holding `src/` and `a.txt`, selection on the first entry.
    fn tree_state() -> ClientShellState {
        let mut state = state();
        let tab = state.investigation_mut().unwrap();
        if let Some(DirectoryState::Loaded(listing)) = tab.browser.directories.get_mut("/repo") {
            listing.entries.insert(
                0,
                DirectoryEntry {
                    name: "src".into(),
                    kind: DirectoryEntryKind::Dir,
                    target_kind: None,
                    size: None,
                    mtime: None,
                    hidden: false,
                    ignored: Some(false),
                },
            );
        }
        tab.browser.rebuild(false);
        state
    }

    fn labels(state: &ClientShellState) -> Vec<String> {
        let tab = state.investigation().unwrap();
        tab.browser.rows.iter().map(|r| r.label.clone()).collect()
    }

    fn click(state: &mut ClientShellState, index: usize) {
        state.set_pane_surface(crate::client::shell::tests::surface());
        state.compose(120, 32).expect("composed investigation");
        let rect = state
            .investigation_hits
            .rows
            .iter()
            .find(|(_, i)| *i == index)
            .map(|(r, _)| *r)
            .expect("row is drawn");
        state.handle_investigation_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + 1,
                row: rect.y,
                modifiers: M::empty(),
            },
            &mut ClientShellInput::default(),
        );
    }

    #[test]
    fn an_empty_root_selects_the_parent_row_instead_of_its_notice() {
        let mut state = state();
        let tab = state.investigation_mut().unwrap();
        if let Some(DirectoryState::Loaded(listing)) = tab.browser.directories.get_mut("/repo") {
            listing.entries.clear();
        }
        tab.browser.rebuild(false);
        let tab = state.investigation().unwrap();
        assert!(
            tab.browser.rows.len() > 1,
            "empty root shows .. and a notice"
        );
        assert!(tab.browser.rows[tab.browser.selected].parent_link);
    }

    fn rooted_at_parent(state: &ClientShellState) -> bool {
        let tab = state.investigation().unwrap();
        tab.browser.root.as_deref() == Some("/")
            && tab.browser.pending_select.as_deref() == Some("/repo")
    }

    #[test]
    fn a_dotdot_row_tops_the_tree_while_the_root_has_a_parent() {
        let state = tree_state();

        assert_eq!(labels(&state)[0], "..");
        let tab = state.investigation().unwrap();
        assert_eq!(
            tab.browser.selected, 1,
            "selection starts on the first entry"
        );
        assert_eq!(tab.browser.selected_entry().unwrap().path, "/repo/src");
    }

    #[test]
    fn no_dotdot_row_at_the_filesystem_root_or_while_searching() {
        let mut state = tree_state();
        let tab = state.investigation_mut().unwrap();
        tab.browser.query = crate::client::shell::text_editor::TextEditor::new("a", false);
        tab.browser.rebuild(false);
        assert!(!labels(&state).contains(&"..".to_owned()));

        let tab = state.investigation_mut().unwrap();
        tab.browser.query = Default::default();
        tab.browser.root = Some("/".into());
        tab.browser.rebuild(false);
        assert!(!labels(&state).contains(&"..".to_owned()));
    }

    #[test]
    fn arrow_keys_reach_the_dotdot_row_without_selecting_any_path() {
        let mut state = tree_state();

        press(&mut state, KeyCode::Up, M::empty());

        let tab = state.investigation().unwrap();
        assert_eq!(tab.browser.selected, 0);
        assert!(tab.browser.selected_entry().is_none());
        assert!(tab.paths().is_empty());
    }

    #[test]
    fn enter_on_dotdot_raises_the_root_and_selects_the_old_root() {
        let mut state = tree_state();
        press(&mut state, KeyCode::Up, M::empty());

        press(&mut state, KeyCode::Enter, M::empty());

        assert!(rooted_at_parent(&state));
    }

    #[test]
    fn a_double_click_on_dotdot_raises_the_root_but_a_single_click_only_selects() {
        let mut state = tree_state();

        click(&mut state, 0);
        assert_eq!(state.investigation().unwrap().browser.selected, 0);
        assert_eq!(
            state.investigation().unwrap().browser.root.as_deref(),
            Some("/repo")
        );

        click(&mut state, 0);
        assert!(rooted_at_parent(&state));
    }

    #[test]
    fn dotdot_is_left_out_of_marking_and_file_operations() {
        let mut state = tree_state();
        press(&mut state, KeyCode::Up, M::empty());

        press(&mut state, KeyCode::Insert, M::empty());
        assert!(state.investigation().unwrap().selected_files.is_empty());

        press(&mut state, KeyCode::Char('a'), M::CONTROL);
        let marked = &state.investigation().unwrap().selected_files;
        assert!(!marked.is_empty() && marked.iter().all(|p| p.starts_with("/repo/")));
        state.investigation_mut().unwrap().selected_files.clear();

        for label in [COPY_PATH, COPY_FILES, CUT_FILES, RENAME, TRASH, DELETE] {
            let mut outcome = ClientShellInput::default();
            state.investigation_menu_action(label, &mut outcome);
            let tab = state.investigation().unwrap();
            assert!(tab.dialog.is_none(), "menu {label}");
            assert!(outcome.actions.is_empty(), "menu {label}");
        }
        assert!(state.workbench.clipboard.is_none());
    }

    #[test]
    fn open_as_root_makes_the_selected_directory_the_root() {
        let mut state = tree_state();

        state.investigation_menu_action(OPEN_AS_ROOT, &mut ClientShellInput::default());

        let tab = state.investigation().unwrap();
        assert_eq!(tab.browser.root.as_deref(), Some("/repo/src"));
        assert_eq!(tab.browser.pending_select, None);
    }

    #[test]
    fn open_as_root_makes_the_folder_of_a_selected_file_the_root() {
        let mut state = tree_state();
        let tab = state.investigation_mut().unwrap();
        let mut nested = match tab.browser.directories.get("/repo") {
            Some(DirectoryState::Loaded(listing)) => listing.clone(),
            _ => unreachable!("the tree state lists /repo"),
        };
        nested.entries.retain(|entry| entry.name == "a.txt");
        tab.browser
            .directories
            .insert("/repo/src".into(), DirectoryState::Loaded(nested));
        tab.browser.expanded.insert("/repo/src".into());
        tab.browser.pending_select = Some("/repo/src/a.txt".into());
        tab.browser.rebuild(true);
        assert_eq!(
            tab.browser.selected_entry().unwrap().path,
            "/repo/src/a.txt"
        );

        state.investigation_menu_action(OPEN_AS_ROOT, &mut ClientShellInput::default());

        let tab = state.investigation().unwrap();
        assert_eq!(tab.browser.root.as_deref(), Some("/repo/src"));
    }

    #[test]
    fn open_as_root_on_dotdot_only_says_what_to_select() {
        let mut state = tree_state();
        press(&mut state, KeyCode::Up, M::empty());

        state.investigation_menu_action(OPEN_AS_ROOT, &mut ClientShellInput::default());

        let tab = state.investigation().unwrap();
        assert_eq!(tab.browser.root.as_deref(), Some("/repo"));
        assert_eq!(tab.notice, "select a file or folder to open as root");
    }
}

mod hover {
    use super::*;
    use crate::api::schema::{DirectoryEntry, DirectoryEntryKind};
    use crate::client::shell::file_viewer::model::DirectoryState;
    use crossterm::event::{KeyModifiers as M, MouseButton, MouseEvent, MouseEventKind};

    /// `/repo` holding `src/` and `a.txt`, composed so row hits exist.
    fn composed() -> ClientShellState {
        let mut state = state();
        let tab = state.investigation_mut().unwrap();
        if let Some(DirectoryState::Loaded(listing)) = tab.browser.directories.get_mut("/repo") {
            listing.entries.insert(
                0,
                DirectoryEntry {
                    name: "src".into(),
                    kind: DirectoryEntryKind::Dir,
                    target_kind: None,
                    size: None,
                    mtime: None,
                    hidden: false,
                    ignored: Some(false),
                },
            );
        }
        tab.browser.rebuild(false);
        state.set_pane_surface(crate::client::shell::tests::surface());
        state.compose(120, 32).expect("composed investigation");
        state
    }

    fn move_to(state: &mut ClientShellState, column: u16, row: u16) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        state.handle_investigation_mouse(
            MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row,
                modifiers: M::empty(),
            },
            &mut outcome,
        );
        outcome
    }

    fn row_rect(state: &ClientShellState, index: usize) -> Rect {
        state
            .investigation_hits
            .rows
            .iter()
            .find(|(_, i)| *i == index)
            .map(|(r, _)| *r)
            .expect("row is drawn")
    }

    #[test]
    fn moving_over_a_row_hovers_it_without_moving_the_selection_or_asking_the_server() {
        let mut state = composed();
        let selected = state.investigation().unwrap().browser.selected;
        let target = if selected == 2 { 1 } else { 2 };
        let rect = row_rect(&state, target);

        let outcome = move_to(&mut state, rect.x + 1, rect.y);

        let tab = state.investigation().unwrap();
        assert_eq!(tab.hovered, Some(target));
        assert_eq!(tab.browser.selected, selected);
        assert!(outcome.repaint);
        assert!(outcome.actions.is_empty());
    }

    #[test]
    fn moving_within_the_same_row_does_not_repaint() {
        let mut state = composed();
        let rect = row_rect(&state, 2);
        move_to(&mut state, rect.x + 1, rect.y);

        let outcome = move_to(&mut state, rect.x + 3, rect.y);

        assert!(!outcome.repaint);
        assert_eq!(state.investigation().unwrap().hovered, Some(2));
    }

    #[test]
    fn leaving_the_list_clears_the_hover() {
        let mut state = composed();
        let rect = row_rect(&state, 2);
        move_to(&mut state, rect.x + 1, rect.y);

        let outcome = move_to(&mut state, 0, 0);

        assert_eq!(state.investigation().unwrap().hovered, None);
        assert!(outcome.repaint);
    }

    #[test]
    fn scrolling_clears_the_hover() {
        let mut state = composed();
        let rect = row_rect(&state, 2);
        move_to(&mut state, rect.x + 1, rect.y);

        state.handle_investigation_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: rect.x + 1,
                row: rect.y,
                modifiers: M::empty(),
            },
            &mut ClientShellInput::default(),
        );

        assert_eq!(state.investigation().unwrap().hovered, None);
    }

    #[test]
    fn moving_the_selection_by_key_clears_the_hover() {
        let mut state = composed();
        let rect = row_rect(&state, 2);
        move_to(&mut state, rect.x + 1, rect.y);

        press(&mut state, KeyCode::Down, M::empty());

        assert_eq!(state.investigation().unwrap().hovered, None);
    }

    #[test]
    fn switching_between_tree_and_content_search_clears_the_hover() {
        let mut state = composed();
        let rect = row_rect(&state, 2);
        move_to(&mut state, rect.x + 1, rect.y);

        press(&mut state, KeyCode::Char('f'), M::CONTROL | M::SHIFT);

        assert_eq!(state.investigation().unwrap().hovered, None);
    }

    #[test]
    fn a_hovered_row_is_drawn_apart_from_plain_and_selected_rows() {
        let mut state = composed();
        let selected = state.investigation().unwrap().browser.selected;
        let target = if selected == 2 { 1 } else { 2 };
        let rect = row_rect(&state, target);
        let style_at = |state: &mut ClientShellState, rect: Rect| {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 32));
            let layout = state.layout(120, 32);
            state.render_investigation(&mut buffer, layout);
            buffer[(rect.x + rect.width - 1, rect.y)].style()
        };
        let before = style_at(&mut state, rect);
        move_to(&mut state, rect.x + 1, rect.y);

        let hovered = style_at(&mut state, rect);
        let selected_rect = row_rect(&state, selected);
        let selected_style = style_at(&mut state, selected_rect);

        assert_ne!(before.bg, hovered.bg);
        assert_ne!(hovered.bg, selected_style.bg);
    }

    fn menu_rect(state: &ClientShellState, index: usize) -> Rect {
        state
            .investigation_hits
            .menu
            .iter()
            .find(|(_, i)| *i == index)
            .map(|(r, _)| *r)
            .expect("menu row is drawn")
    }

    fn click(
        state: &mut ClientShellState,
        button: MouseButton,
        column: u16,
        row: u16,
    ) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        state.handle_investigation_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(button),
                column,
                row,
                modifiers: M::empty(),
            },
            &mut outcome,
        );
        outcome
    }

    /// Index of the first tree row whose entry passes `test`.
    fn row_where(state: &ClientShellState, test: impl Fn(bool) -> bool) -> usize {
        state
            .investigation()
            .unwrap()
            .browser
            .rows
            .iter()
            .position(|row| row.entry.as_ref().is_some_and(|entry| test(entry.is_dir)))
            .expect("row exists")
    }

    fn open_menu(state: &mut ClientShellState, target: MenuTarget) {
        state.investigation_mut().unwrap().dialog =
            Some(Dialog::menu(target, MenuPlacement::Selection));
    }

    fn shown_labels(state: &ClientShellState) -> Vec<&'static str> {
        let tab = state.investigation().unwrap();
        let labels = tab.menu_labels(
            state.workbench.placement.presentation,
            state.workbench.placement.dock,
        );
        state
            .investigation_hits
            .menu
            .iter()
            .map(|(_, index)| labels[*index])
            .collect()
    }

    fn symbol(frame: &crate::protocol::FrameData, x: u16, y: u16) -> String {
        frame.cells[y as usize * frame.width as usize + x as usize]
            .symbol
            .clone()
    }

    #[test]
    fn right_clicking_a_file_row_selects_it_and_opens_the_file_menu_framed_at_the_clicked_cell() {
        let mut state = composed();
        let file = row_where(&state, |is_dir| !is_dir);
        state.investigation_mut().unwrap().browser.selected = row_where(&state, |is_dir| is_dir);
        let row = row_rect(&state, file);
        let (x, y) = (row.x + 3, row.y);

        click(&mut state, MouseButton::Right, x, y);
        let frame = state.compose(120, 32).expect("composed menu");

        let tab = state.investigation().unwrap();
        assert_eq!(tab.browser.selected, file);
        assert_eq!(tab.focus, Focus::Results);
        assert!(matches!(
            tab.dialog,
            Some(Dialog::Menu {
                target: MenuTarget::File,
                ..
            })
        ));
        assert_eq!(
            shown_labels(&state),
            MenuTarget::File.labels(false, FilesPresentation::Full, FilesDock::Left)
        );
        assert_eq!(symbol(&frame, x, y), "┌");
        assert_eq!(menu_rect(&state, 0).x, x + 1);
        assert_eq!(menu_rect(&state, 0).y, y + 1);
    }

    #[test]
    fn right_clicking_a_directory_row_opens_the_directory_menu() {
        let mut state = composed();
        let dir = row_where(&state, |is_dir| is_dir);
        let row = row_rect(&state, dir);

        click(&mut state, MouseButton::Right, row.x + 1, row.y);
        state.compose(120, 32).expect("composed menu");

        assert_eq!(
            shown_labels(&state),
            MenuTarget::Directory.labels(false, FilesPresentation::Full, FilesDock::Left)
        );
        assert!(shown_labels(&state).contains(&DOWNLOAD_ITEM));
    }

    #[test]
    fn right_clicking_a_marked_row_keeps_the_marks_and_another_row_drops_them() {
        let mut state = composed();
        let file = row_where(&state, |is_dir| !is_dir);
        let dir = row_where(&state, |is_dir| is_dir);
        state
            .investigation_mut()
            .unwrap()
            .selected_files
            .insert("/repo/a.txt".into());
        let row = row_rect(&state, file);
        click(&mut state, MouseButton::Right, row.x + 1, row.y);
        assert_eq!(state.investigation().unwrap().selected_files.len(), 1);

        state.investigation_mut().unwrap().dialog = None;
        let row = row_rect(&state, dir);
        click(&mut state, MouseButton::Right, row.x + 1, row.y);
        assert!(state.investigation().unwrap().selected_files.is_empty());
    }

    #[test]
    fn right_clicking_the_tree_below_its_rows_opens_the_tree_menu() {
        let mut state = composed();
        let list = state.investigation_hits.list;
        let last = state.investigation_hits.rows.last().expect("rows").0;
        assert!(last.bottom() < list.bottom(), "the list has empty space");

        click(
            &mut state,
            MouseButton::Right,
            list.x + 2,
            list.bottom() - 1,
        );
        state.compose(120, 32).expect("composed menu");

        assert_eq!(
            shown_labels(&state),
            MenuTarget::Tree.labels(false, FilesPresentation::Full, FilesDock::Left)
        );
    }

    #[test]
    fn right_clicking_the_preview_opens_the_preview_menu() {
        let mut state = composed();
        let body = state.investigation_hits.body;

        click(&mut state, MouseButton::Right, body.x + 8, body.y);
        state.compose(120, 32).expect("composed menu");

        assert_eq!(state.investigation().unwrap().focus, Focus::Body);
        assert_eq!(
            shown_labels(&state),
            MenuTarget::Preview.labels(false, FilesPresentation::Full, FilesDock::Left)
        );
    }

    #[test]
    fn right_clicking_the_search_field_in_content_search_opens_the_search_menu() {
        let mut state = composed();
        state.investigation_mut().unwrap().content_search = true;
        state.compose(120, 32).expect("composed search");
        let field = state.investigation_hits.fields[0].0;

        click(&mut state, MouseButton::Right, field.x + 1, field.y);
        state.compose(120, 32).expect("composed menu");

        assert_eq!(
            shown_labels(&state),
            MenuTarget::Search.labels(true, FilesPresentation::Full, FilesDock::Left)
        );
    }

    #[test]
    fn a_content_search_result_lists_the_match_toggle_and_a_tree_file_does_not() {
        assert!(MenuTarget::File
            .labels(true, FilesPresentation::Full, FilesDock::Left)
            .contains(&"toggle matches in current file"));
        assert!(!MenuTarget::File
            .labels(false, FilesPresentation::Full, FilesDock::Left)
            .contains(&"toggle matches in current file"));
    }

    #[test]
    fn f10_opens_the_menu_of_the_selected_row_under_its_left_edge() {
        let mut state = composed();
        let dir = row_where(&state, |is_dir| is_dir);
        state.investigation_mut().unwrap().browser.selected = dir;
        state.investigation_mut().unwrap().focus = Focus::Results;
        state.compose(120, 32).expect("composed tree");
        let row = row_rect(&state, dir);

        press(&mut state, KeyCode::F(10), M::empty());
        state.compose(120, 32).expect("composed menu");

        assert_eq!(
            shown_labels(&state),
            MenuTarget::Directory.labels(false, FilesPresentation::Full, FilesDock::Left)
        );
        let first = menu_rect(&state, 0);
        assert_eq!((first.x, first.y), (row.x + 1, row.bottom() + 1));
    }

    #[test]
    fn f10_on_the_focused_preview_opens_the_preview_menu() {
        let mut state = composed();
        state.investigation_mut().unwrap().focus = Focus::Body;

        press(&mut state, KeyCode::F(10), M::empty());

        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Menu {
                target: MenuTarget::Preview,
                anchor: MenuPlacement::Selection,
                ..
            })
        ));
    }

    #[test]
    fn hovering_a_scrolled_menu_keeps_the_viewport_so_a_click_hits_the_hovered_item() {
        let mut state = composed();
        let len = MenuTarget::Directory
            .labels(false, FilesPresentation::Full, FilesDock::Left)
            .len();
        open_menu(&mut state, MenuTarget::Directory);
        state.compose(120, 10).expect("composed menu");
        let rows = state.investigation_hits.menu.len();
        let scroll = len - rows;
        assert!(scroll > 1, "the menu must not fit in {rows} rows");
        if let Some(Dialog::Menu {
            selected,
            scroll: stored,
            ..
        }) = state.investigation_mut().unwrap().dialog.as_mut()
        {
            *selected = len - 1;
            *stored = scroll;
        }
        state.compose(120, 10).expect("composed scrolled menu");
        let hovered = scroll + 1;
        let second = menu_rect(&state, hovered);

        move_to(&mut state, second.x + 1, second.y);
        state.compose(120, 10).expect("recomposed menu");

        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Menu { selected, scroll: kept, .. }) if selected == hovered && kept == scroll
        ));
        assert_eq!(menu_rect(&state, hovered), second);
        click(&mut state, MouseButton::Left, second.x + 1, second.y);

        assert!(state.investigation().unwrap().dialog.is_none());
    }

    #[test]
    fn shrinking_the_terminal_keeps_the_selected_menu_item_visible() {
        let mut state = composed();
        let last = MenuTarget::Directory
            .labels(false, FilesPresentation::Full, FilesDock::Left)
            .len()
            - 1;
        open_menu(&mut state, MenuTarget::Directory);
        if let Some(Dialog::Menu { selected, .. }) =
            state.investigation_mut().unwrap().dialog.as_mut()
        {
            *selected = last;
        }
        state.compose(120, 40).expect("composed menu");
        state.compose(120, 8).expect("composed smaller menu");
        assert!(state
            .investigation_hits
            .menu
            .iter()
            .any(|(_, index)| *index == last));
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Menu { selected, scroll, .. }) if selected == last && scroll > 0
        ));
    }

    #[test]
    fn keyboard_navigation_scrolls_the_menu_to_keep_the_selection_visible() {
        let mut state = composed();
        let len = MenuTarget::Directory
            .labels(false, FilesPresentation::Full, FilesDock::Left)
            .len();
        open_menu(&mut state, MenuTarget::Directory);
        state.compose(120, 10).expect("composed menu");
        let rows = state.investigation_hits.menu.len();
        assert!(rows < len);
        for _ in 0..len {
            press(&mut state, KeyCode::Down, M::empty());
        }
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Menu { selected, scroll, .. }) if selected == len - 1 && scroll == len - rows
        ));
        for _ in 0..len {
            press(&mut state, KeyCode::Up, M::empty());
        }
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Menu {
                selected: 0,
                scroll: 0,
                ..
            })
        ));
    }

    #[test]
    fn moving_over_a_workbench_menu_row_highlights_it() {
        let mut state = composed();
        open_menu(&mut state, MenuTarget::Tree);
        state.compose(120, 32).expect("composed menu");
        let rect = menu_rect(&state, 2);

        let outcome = move_to(&mut state, rect.x + 1, rect.y);

        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Menu { selected: 2, .. })
        ));
        assert!(outcome.repaint);
    }

    #[test]
    fn moving_over_tree_rows_while_the_menu_is_open_hovers_no_row() {
        let mut state = composed();
        open_menu(&mut state, MenuTarget::Tree);
        state.compose(120, 32).expect("composed menu");
        let menu = state.investigation_hits.menu.clone();
        let row = state
            .investigation_hits
            .rows
            .iter()
            .map(|(rect, _)| *rect)
            .find(|rect| {
                !menu
                    .iter()
                    .any(|(item, _)| item.y == rect.y && item.x <= rect.x + 1)
            })
            .expect("a row beside the menu");

        move_to(&mut state, row.x + 1, row.y);

        assert_eq!(state.investigation().unwrap().hovered, None);
    }

    #[test]
    fn escape_closes_the_workbench_menu() {
        let mut state = composed();
        open_menu(&mut state, MenuTarget::Tree);
        state.compose(120, 32).expect("composed menu");

        let outcome = press(&mut state, KeyCode::Esc, M::empty());

        assert!(state.investigation().unwrap().dialog.is_none());
        assert!(outcome.actions.is_empty());
    }

    fn select_label(state: &mut ClientShellState, label: &str) -> usize {
        let index = MenuTarget::Preview
            .labels(false, FilesPresentation::Full, FilesDock::Left)
            .iter()
            .position(|item| *item == label)
            .expect("listed");
        if let Some(Dialog::Menu { selected, .. }) =
            state.investigation_mut().unwrap().dialog.as_mut()
        {
            *selected = index;
        }
        index
    }

    #[test]
    fn enter_runs_the_selected_workbench_menu_item_and_closes_the_menu() {
        let mut state = composed();
        let horizontal = state.investigation().unwrap().horizontal;
        open_menu(&mut state, MenuTarget::Preview);
        select_label(&mut state, TOGGLE_SPLIT_DIRECTION);
        state.compose(120, 32).expect("composed menu");

        let outcome = press(&mut state, KeyCode::Enter, M::empty());

        let tab = state.investigation().unwrap();
        assert!(tab.dialog.is_none());
        assert_eq!(tab.horizontal, !horizontal);
        assert!(outcome.actions.is_empty());
    }

    #[test]
    fn clicking_a_workbench_menu_row_runs_that_item_and_closes_the_menu() {
        let mut state = composed();
        let horizontal = state.investigation().unwrap().horizontal;
        open_menu(&mut state, MenuTarget::Preview);
        state.compose(120, 32).expect("composed menu");
        let index = MenuTarget::Preview
            .labels(false, FilesPresentation::Full, FilesDock::Left)
            .iter()
            .position(|item| *item == TOGGLE_SPLIT_DIRECTION)
            .expect("listed");
        let rect = menu_rect(&state, index);

        click(&mut state, MouseButton::Left, rect.x + 1, rect.y);

        let tab = state.investigation().unwrap();
        assert!(tab.dialog.is_none());
        assert_eq!(tab.horizontal, !horizontal);
    }

    fn input_text(state: &ClientShellState) -> Option<String> {
        match &state.investigation().unwrap().dialog {
            Some(Dialog::Input { input, .. }) => Some(input.to_string()),
            _ => None,
        }
    }

    fn wheel(state: &mut ClientShellState, kind: MouseEventKind, column: u16, row: u16) {
        state.handle_investigation_mouse(
            MouseEvent {
                kind,
                column,
                row,
                modifiers: M::empty(),
            },
            &mut ClientShellInput::default(),
        );
    }

    fn open_menu_cursor(state: &ClientShellState) -> Option<(usize, usize)> {
        match state.investigation().unwrap().dialog {
            Some(Dialog::Menu {
                selected, scroll, ..
            }) => Some((selected, scroll)),
            _ => None,
        }
    }

    #[test]
    fn every_label_of_every_menu_runs_an_action() {
        let mut labels: Vec<(MenuTarget, &str)> = Vec::new();
        for target in [
            MenuTarget::File,
            MenuTarget::Directory,
            MenuTarget::Tree,
            MenuTarget::Search,
            MenuTarget::Preview,
        ] {
            for content_search in [false, true] {
                for presentation in [FilesPresentation::Split, FilesPresentation::Full] {
                    for dock in [FilesDock::Left, FilesDock::Right, FilesDock::Bottom] {
                        for label in target.labels(content_search, presentation, dock) {
                            if !labels.contains(&(target, *label)) {
                                labels.push((target, *label));
                            }
                        }
                    }
                }
            }
        }
        for (target, label) in labels {
            let mut state = state();
            open_menu(&mut state, target);

            let ran = state.investigation_menu_action(label, &mut ClientShellInput::default());

            assert!(ran, "{target:?} menu item {label:?} has no action");
        }
        let mut state = state();
        assert!(!state.investigation_menu_action("no such item", &mut ClientShellInput::default()));
    }

    #[test]
    fn the_tree_menu_shows_ignored_rows_and_the_search_menu_searches_ignored_files() {
        let tree = MenuTarget::Tree.labels(false, FilesPresentation::Full, FilesDock::Left);
        assert!(tree.contains(&SHOW_IGNORED));
        assert!(!tree.contains(&INCLUDE_IGNORED));
        assert!(MenuTarget::Search
            .labels(true, FilesPresentation::Full, FilesDock::Left)
            .contains(&INCLUDE_IGNORED));
        let mut state = state();

        open_menu(&mut state, MenuTarget::Tree);
        state.investigation_menu_action(SHOW_IGNORED, &mut ClientShellInput::default());
        let tab = state.investigation().unwrap();
        assert!(tab.browser.show_ignored);
        assert!(!tab.ignored);

        open_menu(&mut state, MenuTarget::Search);
        state.investigation_menu_action(INCLUDE_IGNORED, &mut ClientShellInput::default());
        let tab = state.investigation().unwrap();
        assert!(tab.ignored);
        assert!(tab.browser.show_ignored);
    }

    #[test]
    fn new_entries_and_pastes_from_empty_tree_space_go_into_the_root_not_the_selected_folder() {
        let mut state = composed();
        let dir = row_where(&state, |is_dir| is_dir);
        let tab = state.investigation_mut().unwrap();
        tab.browser.selected = dir;
        let folder = tab.browser.selected_entry().unwrap().path.clone();
        state.workbench.clipboard = Some(FileClipboard {
            endpoint: ClientEndpointId::Local,
            boot: "boot-1".into(),
            paths: vec!["/repo/a.txt".into()],
            cut: false,
        });

        for (label, root, selected) in [
            (NEW_FILE, "/repo/".to_owned(), format!("{folder}/")),
            (NEW_DIRECTORY, "/repo/".to_owned(), format!("{folder}/")),
            (PASTE_FILES, "/repo".to_owned(), folder.clone()),
        ] {
            open_menu(&mut state, MenuTarget::Tree);
            state.investigation_menu_action(label, &mut ClientShellInput::default());
            assert_eq!(input_text(&state), Some(root), "{label} from the tree");

            open_menu(&mut state, MenuTarget::Directory);
            state.investigation_menu_action(label, &mut ClientShellInput::default());
            assert_eq!(input_text(&state), Some(selected), "{label} from a folder");
        }
    }

    #[test]
    fn right_clicking_a_file_row_previews_that_file() {
        let mut state = composed();
        state.investigation_mut().unwrap().preview = None;
        let file = row_where(&state, |is_dir| !is_dir);
        let row = row_rect(&state, file);

        click(&mut state, MouseButton::Right, row.x + 1, row.y);

        let tab = state.investigation().unwrap();
        assert_eq!(
            tab.document().map(|document| document.path.as_str()),
            Some("/repo/a.txt")
        );
        assert!(tab.dialog.is_some());
    }

    #[test]
    fn f10_in_a_search_field_opens_the_search_menu_under_that_field() {
        use crate::api::schema::FileMatch;
        let mut state = composed();
        let tab = state.investigation_mut().unwrap();
        tab.content_search = true;
        tab.matches.push(FileMatch {
            path: "/repo/a.txt".into(),
            revision: "old".into(),
            start: 0,
            end: 1,
            line: 1,
            text: "hello".into(),
            matched: "h".into(),
            replacement: None,
        });
        tab.focus = Focus::Include;
        state.compose(120, 32).expect("composed search");
        let field = state
            .investigation_hits
            .fields
            .iter()
            .find(|(_, focus)| *focus == Focus::Include)
            .map(|(rect, _)| *rect)
            .expect("include field drawn");

        press(&mut state, KeyCode::F(10), M::empty());
        state.compose(120, 32).expect("composed menu");

        assert_eq!(
            shown_labels(&state),
            MenuTarget::Search.labels(true, FilesPresentation::Full, FilesDock::Left)
        );
        let first = menu_rect(&state, 0);
        assert_eq!((first.x, first.y), (field.x + 1, field.bottom() + 1));
    }

    #[test]
    fn the_wheel_over_an_overflowing_menu_scrolls_it_by_one_and_keeps_the_selection_inside() {
        let mut state = composed();
        let len = MenuTarget::Directory
            .labels(false, FilesPresentation::Full, FilesDock::Left)
            .len();
        open_menu(&mut state, MenuTarget::Directory);
        state.compose(120, 10).expect("composed menu");
        let rows = state.investigation_hits.menu.len();
        assert!(rows + 1 < len, "the menu must not fit in {rows} rows");
        let row = menu_rect(&state, 0);

        wheel(&mut state, MouseEventKind::ScrollDown, row.x + 1, row.y);
        assert_eq!(open_menu_cursor(&state), Some((1, 1)));
        state.compose(120, 10).expect("scrolled menu");
        assert_eq!(state.investigation_hits.menu[0].1, 1);

        let row = menu_rect(&state, 1);
        wheel(&mut state, MouseEventKind::ScrollUp, row.x + 1, row.y);
        assert_eq!(open_menu_cursor(&state), Some((1, 0)));
    }

    #[test]
    fn clicking_outside_the_workbench_menu_closes_it_without_acting() {
        let mut state = composed();
        let selected = state.investigation().unwrap().browser.selected;
        open_menu(&mut state, MenuTarget::Tree);
        state.compose(120, 32).expect("composed menu");
        let area = state.layout(120, 32).pane_surface;
        let point = (area.right() - 1, area.bottom() - 1);
        assert!(!state
            .investigation_hits
            .menu
            .iter()
            .any(|(rect, _)| crate::client::shell::contains(*rect, point)));

        let outcome = click(&mut state, MouseButton::Left, point.0, point.1);

        let tab = state.investigation().unwrap();
        assert!(tab.dialog.is_none());
        assert_eq!(tab.browser.selected, selected);
        assert!(outcome.actions.is_empty());
        assert!(outcome.repaint);
    }
}

mod help_open {
    use super::*;
    use crate::client::shell::state::ClientHelpOverlay;
    use crate::client::shell::text_editor::TextEditor;
    use crossterm::event::{KeyModifiers as M, MouseButton, MouseEvent, MouseEventKind};

    fn with_help(search_focused: bool) -> ClientShellState {
        let mut state = state();
        state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
            query: TextEditor::default(),
            search_focused,
            scroll: 0,
        }));
        state.set_pane_surface(crate::client::shell::tests::surface());
        state.compose(120, 32).expect("composed with help");
        state
    }

    fn help_open(state: &ClientShellState) -> bool {
        matches!(state.overlay, Some(ClientShellOverlay::Help(_)))
    }

    fn help_scroll(state: &ClientShellState) -> usize {
        match state.overlay {
            Some(ClientShellOverlay::Help(ref help)) => help.scroll,
            _ => panic!("help is closed"),
        }
    }

    #[test]
    fn arrow_keys_move_the_files_selection_and_help_stays_open() {
        let mut state = with_help(false);
        let before = state.investigation().unwrap().browser.selected;
        press(&mut state, KeyCode::Up, M::empty());
        assert_ne!(state.investigation().unwrap().browser.selected, before);
        assert!(help_open(&state));
        assert_eq!(help_scroll(&state), 0);
    }

    #[test]
    fn escape_closes_help_and_leaves_the_files_selection() {
        let mut state = with_help(false);
        let before = state.investigation().unwrap().browser.selected;
        press(&mut state, KeyCode::Esc, M::empty());
        assert!(state.overlay.is_none());
        assert_eq!(state.investigation().unwrap().browser.selected, before);
    }

    #[test]
    fn question_mark_closes_help() {
        let mut state = with_help(false);
        press(&mut state, KeyCode::Char('?'), M::SHIFT);
        assert!(state.overlay.is_none());
    }

    #[test]
    fn the_help_binding_after_the_prefix_closes_help() {
        let mut state = with_help(false);
        let (code, modifiers) = state.config.keybinds.prefix[0];
        press(&mut state, code, modifiers);
        assert_eq!(state.mode, ClientShellMode::Prefix);
        press(&mut state, KeyCode::Char('?'), M::SHIFT);
        assert!(state.overlay.is_none());
    }

    #[test]
    fn typing_goes_to_the_help_search_while_it_is_focused() {
        let mut state = with_help(true);
        let before = state.investigation().unwrap().browser.selected;
        press(&mut state, KeyCode::Down, M::empty());
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Text(
            crate::input::TextCommit::new("ab"),
        )]);
        match state.overlay {
            Some(ClientShellOverlay::Help(ref help)) => assert_eq!(help.query.as_str(), "ab"),
            _ => panic!("help closed"),
        }
        assert_eq!(state.investigation().unwrap().browser.selected, before);
    }

    #[test]
    fn a_click_on_a_row_outside_the_popup_selects_it_and_help_stays_open() {
        let mut state = with_help(false);
        let popup = state.hits.help_popup;
        let (rect, index) = state
            .investigation_hits
            .rows
            .iter()
            .copied()
            .find(|(r, i)| {
                *i != state.investigation().unwrap().browser.selected
                    && !crate::client::shell::contains(popup, (r.x + 1, r.y))
            })
            .expect("a row clear of the popup");
        state.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + 1,
                row: rect.y,
                modifiers: M::empty(),
            },
            &mut ClientShellInput::default(),
        );
        assert!(help_open(&state));
        assert_eq!(state.investigation().unwrap().browser.selected, index);
    }

    #[test]
    fn a_click_on_a_terminal_tab_outside_the_popup_closes_help_and_focuses_the_tab() {
        let mut state = with_help(false);
        let popup = state.hits.help_popup;
        let (rect, tab_id) = state
            .hits
            .tabs
            .iter()
            .find(|(r, _)| !crate::client::shell::contains(popup, (r.x, r.y)))
            .cloned()
            .expect("a terminal tab clear of the popup");
        let mut click = |kind| {
            let mut outcome = ClientShellInput::default();
            state.handle_mouse(
                MouseEvent {
                    kind,
                    column: rect.x,
                    row: rect.y,
                    modifiers: M::empty(),
                },
                &mut outcome,
            );
            outcome
        };
        click(MouseEventKind::Down(MouseButton::Left));
        let released = click(MouseEventKind::Up(MouseButton::Left));
        assert!(!help_open(&state));
        assert!(!state.workbench.visible);
        assert!(released.actions.iter().any(|action| matches!(
            action,
            crate::client::shell::ClientShellAction::Endpoint { request, .. }
                if matches!(
                    &request.method,
                    crate::api::schema::Method::TabFocus(target) if target.tab_id == tab_id
                )
        )));
    }

    #[test]
    fn the_wheel_inside_the_popup_scrolls_help() {
        let mut state = with_help(false);
        let popup = state.hits.help_popup;
        state.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: popup.x + 2,
                row: popup.y + 2,
                modifiers: M::empty(),
            },
            &mut ClientShellInput::default(),
        );
        assert!(help_scroll(&state) > 0);
    }

    #[test]
    fn a_workbench_dialog_opened_through_help_closes_help() {
        let mut state = with_help(false);
        press(&mut state, KeyCode::F(10), M::empty());
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Menu { .. })
        ));
        assert!(state.overlay.is_none());
    }

    #[test]
    fn a_plan_response_over_help_closes_help_and_the_next_input_reaches_the_dialog() {
        use crate::api::schema::ResponseResult;
        let mut state = with_help(false);
        state.investigation_mut().unwrap().focus = Focus::Results;
        let outcome = press(&mut state, KeyCode::Delete, M::empty());
        assert!(help_open(&state));
        assert!(state.investigation().unwrap().dialog.is_none());
        let id = outcome
            .actions
            .iter()
            .find_map(|action| match action {
                crate::client::shell::ClientShellAction::Endpoint { request, .. } => {
                    Some(request.id.clone())
                }
                _ => None,
            })
            .expect("plan request sent");
        let (repaint, _) = state.handle_endpoint_result(
            "boot-1",
            &id,
            Ok(ResponseResult::FilesPlanned {
                outcomes: Vec::new(),
            }),
        );
        assert!(repaint);
        assert!(!help_open(&state));
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Confirm { .. })
        ));
        let outcome = press(&mut state, KeyCode::Enter, M::empty());
        assert!(outcome.actions.iter().any(|action| matches!(
            action,
            crate::client::shell::ClientShellAction::Endpoint { .. }
        )));
    }

    #[test]
    fn a_key_pressed_while_help_hides_a_dialog_only_reveals_it() {
        let mut state = with_help(false);
        state.investigation_mut().unwrap().dialog =
            Some(Dialog::menu(MenuTarget::Tree, MenuPlacement::Selection));
        let outcome = press(&mut state, KeyCode::Enter, M::empty());
        assert!(!help_open(&state));
        assert!(outcome.actions.is_empty());
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Menu { selected: 0, .. })
        ));
    }

    #[test]
    fn clicking_the_help_filter_focuses_it_and_typing_fills_the_query() {
        let mut state = with_help(false);
        let filter = state.hits.help_filter;
        assert!(filter.width > 0);
        state.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: filter.x + 2,
                row: filter.y,
                modifiers: M::empty(),
            },
            &mut ClientShellInput::default(),
        );
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Text(
            crate::input::TextCommit::new("ab"),
        )]);
        match state.overlay {
            Some(ClientShellOverlay::Help(ref help)) => {
                assert!(help.search_focused);
                assert_eq!(help.query.as_str(), "ab");
            }
            _ => panic!("help closed"),
        }
    }

    #[test]
    fn without_the_workbench_arrows_still_scroll_help() {
        let mut state = with_help(false);
        state.workbench.visible = false;
        press(&mut state, KeyCode::Down, M::empty());
        assert_eq!(help_scroll(&state), 1);
    }
}

#[test]
fn open_files_steps_from_hidden_to_focused_files_to_hidden_and_reshows_the_same_tab() {
    const COLS: u16 = 160;
    const ROWS: u16 = 32;
    let mut state = state();
    state.workbench.placement.presentation = FilesPresentation::Split;
    state.remember_files_placement();
    state.show_files(0);
    state.set_pane_surface(crate::client::shell::tests::surface());
    state.compose(COLS, ROWS).expect("composed split");
    let open_files = |state: &mut ClientShellState| {
        let mut outcome = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenFiles),
            &mut outcome,
        );
        outcome
    };
    assert!(state.files_has_focus());

    let hide = open_files(&mut state);
    assert!(!state.workbench.visible);
    assert!(!state.investigation_active());
    assert!(!state.workbench.files_focused);
    assert!(state.layout(COLS, ROWS).files_region.is_none());
    assert!(!hide.actions.iter().any(|action| matches!(
        action,
        crate::client::shell::ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::DirectoryList(_))
    )));

    open_files(&mut state);
    assert!(state.files_has_focus());
    assert_eq!(state.workbench.tabs.len(), 1);
    assert_eq!(state.workbench.active, Some(0));
    assert!(state.layout(COLS, ROWS).files_region.is_some());

    state.show_terminal_side();
    assert!(state.investigation_active());
    assert!(!state.files_has_focus());

    open_files(&mut state);
    assert!(state.files_has_focus());
    assert!(state.layout(COLS, ROWS).files_region.is_some());

    open_files(&mut state);
    assert!(!state.investigation_active());
}

#[test]
fn open_files_in_full_screen_toggles_between_the_focused_workbench_and_hidden() {
    let mut state = state();
    state.set_pane_surface(crate::client::shell::tests::surface());
    state.compose(160, 32).expect("composed full screen");
    let open_files = |state: &mut ClientShellState| {
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenFiles),
            &mut ClientShellInput::default(),
        );
    };
    state.workbench.files_focused = false;
    assert!(state.files_has_focus());

    open_files(&mut state);
    assert!(!state.investigation_active());

    open_files(&mut state);
    assert!(state.files_has_focus());
    assert_eq!(state.workbench.active, Some(0));

    state.show_terminal_side();
    assert!(!state.investigation_active());
}

mod split {
    use super::*;
    use crossterm::event::{KeyModifiers as M, MouseButton, MouseEvent, MouseEventKind};

    const COLS: u16 = 160;
    const ROWS: u16 = 32;

    /// The workbench docked beside the test pane, just opened so the files
    /// have focus, and composed so hits exist.
    fn composed() -> ClientShellState {
        let mut state = state();
        state.workbench.placement.presentation = FilesPresentation::Split;
        state.remember_files_placement();
        state.show_files(0);
        state.set_pane_surface(crate::client::shell::tests::surface());
        state.compose(COLS, ROWS).expect("composed split");
        state
    }

    /// `composed`, with a terminal brought forward beside the files.
    fn terminal_focused() -> ClientShellState {
        let mut state = composed();
        state.show_terminal_side();
        state.compose(COLS, ROWS).expect("composed split");
        state
    }

    fn reaches_pane(outcome: &ClientShellInput) -> bool {
        outcome.requests.iter().any(|request| {
            matches!(
                request,
                ClientMessage::ClientShellPaneInput { pane_id, .. } if pane_id == "pane_1"
            )
        })
    }

    fn open_files(state: &mut ClientShellState) {
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenFiles),
            &mut ClientShellInput::default(),
        );
    }

    fn mouse(
        state: &mut ClientShellState,
        kind: MouseEventKind,
        x: u16,
        y: u16,
    ) -> ClientShellInput {
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: M::empty(),
        })])
    }

    /// The pane surface with the workbench out of the way.
    fn whole_surface(state: &ClientShellState, cols: u16, rows: u16) -> Rect {
        let layout = state.layout(cols, rows);
        match layout.files_region {
            Some(files) => files.union(layout.files_divider).union(layout.pane_surface),
            None => layout.pane_surface,
        }
    }

    #[test]
    fn split_docks_the_files_region_on_the_chosen_edge_at_its_ratio() {
        let mut state = state();
        state.workbench.placement.presentation = FilesPresentation::Split;
        let whole = whole_surface(&state, COLS, ROWS);
        let side = ((whole.width - 1) * 3 + 5) / 10;

        let left = state.layout(COLS, ROWS);
        let files = left.files_region.unwrap();
        assert_eq!(files, Rect::new(whole.x, whole.y, side, whole.height));
        assert_eq!(left.files_divider.x, files.right());
        assert_eq!(left.pane_surface.x, files.right() + 1);
        assert_eq!(left.pane_surface.right(), whole.right());

        state.workbench.placement.dock = FilesDock::Right;
        let right = state.layout(COLS, ROWS);
        let files = right.files_region.unwrap();
        assert_eq!(files.right(), whole.right());
        assert_eq!(files.width, side);
        assert_eq!(right.pane_surface.x, whole.x);
        assert_eq!(right.files_divider.x, right.pane_surface.right());

        state.workbench.placement.dock = FilesDock::Bottom;
        let bottom = state.layout(COLS, ROWS);
        let files = bottom.files_region.unwrap();
        assert_eq!(files.bottom(), whole.bottom());
        assert_eq!(files.width, whole.width);
        assert_eq!(files.height, ((whole.height - 1) * 4 + 5) / 10);
        assert_eq!(bottom.files_divider.y, bottom.pane_surface.bottom());
    }

    #[test]
    fn full_screen_and_a_hidden_workbench_leave_the_surface_whole() {
        let mut state = state();
        assert!(state.layout(COLS, ROWS).files_region.is_none());
        state.workbench.placement.presentation = FilesPresentation::Split;
        state.workbench.visible = false;
        assert!(state.layout(COLS, ROWS).files_region.is_none());
    }

    #[test]
    fn a_narrow_surface_falls_back_to_full_screen_and_returns_with_room_to_spare() {
        let mut state = state();
        state.config.mobile_width_threshold = 0;
        state.workbench.placement.presentation = FilesPresentation::Split;
        let sidebar = whole_surface(&state, COLS, ROWS).x;
        let fits = sidebar + placement::MIN_TERMINAL_COLS + placement::MIN_FILES_COLS + 1;

        let mut docked_at = |cols: u16| {
            state.compose(cols, ROWS);
            state.layout(cols, ROWS).files_region.is_some()
        };
        assert!(docked_at(fits));
        assert!(!docked_at(fits - 1));
        assert!(!docked_at(fits));
        let returns = fits + placement::RETURN_MARGIN_COLS;
        assert!(!docked_at(returns - 1));
        assert!(docked_at(returns));
        assert_eq!(
            state.workbench.placement.presentation,
            FilesPresentation::Split
        );
    }

    #[test]
    fn surface_size_is_the_terminal_side_in_split_and_the_whole_surface_in_full() {
        let mut state = state();
        let whole = state.layout(COLS, ROWS).pane_surface;
        assert_eq!(state.surface_size(COLS, ROWS).cols, whole.width);

        state.workbench.placement.presentation = FilesPresentation::Split;
        let terminal = state.layout(COLS, ROWS).pane_surface;
        let size = state.surface_size(COLS, ROWS);
        assert_eq!((size.cols, size.rows), (terminal.width, terminal.height));
        assert!(size.cols < whole.width);
    }

    #[test]
    fn split_draws_terminals_and_files_with_a_divider_between() {
        let mut state = composed();
        let layout = state.layout(COLS, ROWS);
        let files = layout.files_region.unwrap();
        let frame = state.compose(COLS, ROWS).unwrap().frame;
        let cell = |x: u16, y: u16| {
            frame.cells[usize::from(y) * usize::from(COLS) + usize::from(x)]
                .symbol
                .clone()
        };
        assert_eq!(cell(layout.pane_surface.x, layout.pane_surface.y), "L");
        assert_eq!(cell(layout.files_divider.x, layout.files_divider.y), "│");
        assert_eq!(state.hits.files_divider, layout.files_divider);
        assert_eq!(state.investigation_hits.region, files);
        assert!(!state.hits.panes.is_empty());
    }

    #[test]
    fn workbench_hits_are_cleared_by_a_frame_that_draws_no_workbench() {
        let mut state = composed();
        assert!(!state.investigation_hits.rows.is_empty());
        state.workbench.tabs[0].browser.workspace_id = Some("ws_other".into());
        state.compose(COLS, ROWS);
        assert!(state.investigation_hits.rows.is_empty());
        assert_eq!(state.investigation_hits.region, Rect::default());
    }

    #[test]
    fn a_click_on_the_terminal_side_focuses_the_pane_and_keeps_the_files_region() {
        let mut state = composed();
        assert!(state.workbench.files_focused);
        let pane = state.hits.panes[0].inner_rect;

        let outcome = mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            pane.x + 1,
            pane.y,
        );

        assert!(outcome.actions.iter().any(|action| matches!(
            action,
            ClientShellAction::Endpoint { request, .. }
                if matches!(
                    &request.method,
                    crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
                )
        )));
        assert!(state.workbench.visible);
        assert!(!state.workbench.files_focused);
        assert!(state.layout(COLS, ROWS).files_region.is_some());
    }

    #[test]
    fn keys_edit_the_files_while_the_docked_region_has_focus() {
        let mut state = composed();
        let outcome = press(&mut state, KeyCode::Char('/'), M::empty());
        assert!(!reaches_pane(&outcome));
        assert_eq!(state.investigation().unwrap().focus, Focus::Paths);
        let outcome = press(&mut state, KeyCode::Char('x'), M::empty());
        assert!(!reaches_pane(&outcome));
        assert_eq!(state.investigation().unwrap().browser.query.as_str(), "x");
    }

    #[test]
    fn keys_text_and_pastes_reach_the_terminal_once_it_has_focus_beside_the_files() {
        let mut state = terminal_focused();
        assert!(!state.host_keyboard_report_all_requested());

        let outcome = press(&mut state, KeyCode::Char('/'), M::empty());
        assert!(reaches_pane(&outcome));
        assert_eq!(state.investigation().unwrap().focus, Focus::Results);
        let outcome = state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Text(
            crate::input::TextCommit::new("é"),
        )]);
        assert!(reaches_pane(&outcome));
        let outcome = state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Paste(
            "echo hi".into(),
        )]);
        assert!(reaches_pane(&outcome));
        assert!(state.clipboard_image_target().is_some());
        assert!(state.workbench_drop_target().is_none());
        assert!(state.layout(COLS, ROWS).files_region.is_some());
    }

    #[test]
    fn a_doubled_prefix_beside_the_files_sends_the_literal_prefix_to_the_terminal() {
        let mut state = terminal_focused();
        press(&mut state, KeyCode::Char('b'), M::CONTROL);
        assert_eq!(state.mode, ClientShellMode::Prefix);
        let outcome = press(&mut state, KeyCode::Char('b'), M::CONTROL);
        assert!(reaches_pane(&outcome));
    }

    #[test]
    fn a_prefix_binding_beside_the_files_runs_without_reaching_the_terminal() {
        let mut state = terminal_focused();
        let config: Config =
            toml::from_str("[keys]\nopen_files = \"prefix+f\"\n").expect("open_files parses");
        state.config = ClientShellConfig::from_config(&config);

        press(&mut state, KeyCode::Char('b'), M::CONTROL);
        let outcome = press(&mut state, KeyCode::Char('f'), M::empty());

        assert!(!reaches_pane(&outcome));
        assert_eq!(state.mode, ClientShellMode::Terminal);
        assert!(state.files_has_focus());
    }

    #[test]
    fn esc_with_nothing_selected_hands_the_keys_to_the_terminal_and_keeps_the_files_shown() {
        let mut state = composed();
        state.investigation_mut().unwrap().selected_files =
            std::iter::once("/repo/a.txt".to_owned()).collect();

        press(&mut state, KeyCode::Esc, M::empty());
        assert!(state.workbench.files_focused);
        assert!(state.investigation().unwrap().selected_files.is_empty());

        press(&mut state, KeyCode::Esc, M::empty());
        assert!(!state.workbench.files_focused);
        assert!(state.layout(COLS, ROWS).files_region.is_some());
        assert!(reaches_pane(&press(
            &mut state,
            KeyCode::Char('x'),
            M::empty()
        )));
    }

    #[test]
    fn esc_in_full_screen_leaves_the_workbench_with_the_keys() {
        let mut state = state();
        state.set_pane_surface(crate::client::shell::tests::surface());
        state.compose(COLS, ROWS);
        press(&mut state, KeyCode::Esc, M::empty());
        assert!(state.files_has_focus());
        assert!(!reaches_pane(&press(
            &mut state,
            KeyCode::Char('x'),
            M::empty()
        )));
    }

    #[test]
    fn open_files_focuses_a_region_shown_beside_a_focused_terminal_before_hiding_it() {
        let mut state = terminal_focused();

        open_files(&mut state);
        assert!(state.workbench.visible && state.workbench.files_focused);

        open_files(&mut state);
        assert!(!state.workbench.visible && !state.workbench.files_focused);

        open_files(&mut state);
        assert!(state.workbench.visible && state.workbench.files_focused);
    }

    #[test]
    fn a_press_in_the_files_region_takes_the_keys_back_from_the_terminal() {
        let mut state = terminal_focused();
        let files = state.layout(COLS, ROWS).files_region.unwrap();
        mouse(
            &mut state,
            MouseEventKind::ScrollDown,
            files.x + 2,
            files.y + 2,
        );
        assert!(!state.workbench.files_focused);

        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            files.x + 2,
            files.y + 2,
        );
        assert!(state.workbench.files_focused);
        assert!(!reaches_pane(&press(
            &mut state,
            KeyCode::Char('x'),
            M::empty()
        )));
    }

    #[test]
    fn close_tab_beside_the_files_closes_the_terminal_tab_not_the_investigation() {
        let mut state = terminal_focused();
        let mut outcome = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::CloseTab),
            &mut outcome,
        );
        assert_eq!(state.workbench.tabs.len(), 1);
        assert!(state.workbench.visible);
    }

    #[test]
    fn help_passing_input_to_the_files_closes_when_a_press_focuses_the_terminal() {
        let mut state = composed();
        state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
            query: TextEditor::default(),
            search_focused: false,
            scroll: 0,
        }));
        state.compose(COLS, ROWS);
        let pane = state.layout(COLS, ROWS).pane_surface;
        let point = (pane.right() - 1, pane.bottom() - 1);
        assert!(!crate::client::shell::contains(
            state.hits.help_popup,
            point
        ));

        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            point.0,
            point.1,
        );

        assert!(state.overlay.is_none());
        assert!(!state.workbench.files_focused);
    }

    #[test]
    fn a_text_selection_begun_on_a_terminal_keeps_extending_into_the_files_region() {
        let mut state = terminal_focused();
        let pane = state.hits.panes[0].inner_rect;
        let files = state.layout(COLS, ROWS).files_region.unwrap();
        let cursor_before = state
            .investigation()
            .unwrap()
            .preview
            .as_ref()
            .unwrap()
            .cursor;

        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            pane.x + 2,
            pane.y,
        );
        mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            pane.x + 6,
            pane.y,
        );
        assert!(state.selection.as_ref().is_some_and(|s| s.is_in_progress()));
        mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            files.x + 2,
            files.y + 2,
        );
        assert!(state.selection.as_ref().is_some_and(|s| s.is_dragging()));
        mouse(
            &mut state,
            MouseEventKind::Up(MouseButton::Left),
            files.x + 2,
            files.y + 2,
        );

        // Finished and, with copy on select, handed to the clipboard.
        assert!(state.selection.as_ref().is_none_or(|s| s.is_finalized()));
        assert!(!state.workbench.files_focused);
        let tab = state.investigation().unwrap();
        assert_eq!(tab.preview.as_ref().unwrap().cursor, cursor_before);
    }

    #[test]
    fn a_drag_begun_in_the_files_region_never_reaches_the_terminals() {
        let mut state = composed();
        let pane = state.hits.panes[0].inner_rect;
        let body = state.investigation_hits.body;
        assert!(!body.is_empty());

        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            body.x + 8,
            body.y,
        );
        let dragged = mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            pane.x + 4,
            pane.y + 1,
        );
        let released = mouse(
            &mut state,
            MouseEventKind::Up(MouseButton::Left),
            pane.x + 4,
            pane.y + 1,
        );

        assert!(state.selection.is_none());
        assert!(!reaches_pane(&dragged) && !reaches_pane(&released));
        assert!(dragged.actions.is_empty() && released.actions.is_empty());
        assert!(state.workbench.files_focused);
        assert!(!state.workbench.files_drag);
    }

    #[test]
    fn a_divider_drag_across_the_terminals_starts_no_text_selection() {
        let mut state = composed();
        let divider = state.hits.files_divider;
        let pane = state.hits.panes[0].inner_rect;

        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            divider.x,
            divider.y + 3,
        );
        mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            pane.x + 20,
            pane.y + 3,
        );
        mouse(
            &mut state,
            MouseEventKind::Up(MouseButton::Left),
            pane.x + 20,
            pane.y + 3,
        );

        assert!(state.selection.is_none());
        assert!(state.chrome_drag.is_none());
    }

    #[test]
    fn with_help_open_a_press_in_the_files_region_focuses_them_whatever_had_the_keys() {
        let mut state = terminal_focused();
        state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
            query: TextEditor::default(),
            search_focused: false,
            scroll: 0,
        }));
        state.compose(COLS, ROWS);
        assert!(!state.workbench.files_focused);
        let files = state.layout(COLS, ROWS).files_region.unwrap();
        let point = (files.x + 1, files.bottom() - 2);
        assert!(!crate::client::shell::contains(
            state.hits.help_popup,
            point
        ));

        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            point.0,
            point.1,
        );

        assert!(state.workbench.files_focused);
        assert!(matches!(state.overlay, Some(ClientShellOverlay::Help(_))));
        assert!(state.help_passes_input_to_workbench());
        assert!(!reaches_pane(&press(&mut state, KeyCode::Down, M::empty())));
    }

    #[test]
    fn a_press_on_the_terminals_while_a_files_dialog_is_open_keeps_the_dialog_and_the_focus() {
        let mut state = terminal_focused();
        state.investigation_menu_action(RENAME, &mut ClientShellInput::default());
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Input { .. })
        ));
        state.compose(COLS, ROWS);
        let pane = state.layout(COLS, ROWS).pane_surface;

        let outcome = mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            pane.right() - 1,
            pane.bottom() - 1,
        );

        assert!(!state.workbench.files_focused);
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Input { .. })
        ));
        assert!(!reaches_pane(&outcome));
        assert!(outcome.actions.is_empty());
    }

    #[test]
    fn a_right_click_in_the_files_region_opens_the_workbench_menu() {
        let mut state = composed();
        let files = state.layout(COLS, ROWS).files_region.unwrap();

        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Right),
            files.x + 2,
            files.y,
        );

        assert!(state.overlay.is_none());
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Menu {
                target: MenuTarget::Tree,
                ..
            })
        ));
    }

    #[test]
    fn dragging_the_divider_moves_it_clamps_and_resizes_the_terminals() {
        let mut state = composed();
        let whole = whole_surface(&state, COLS, ROWS);
        let divider = state.hits.files_divider;

        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            divider.x,
            divider.y + 3,
        );
        let outcome = mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            whole.x + 50,
            divider.y + 3,
        );
        assert!(outcome.resize);
        assert_eq!(state.layout(COLS, ROWS).files_divider.x, whole.x + 50);

        mouse(
            &mut state,
            MouseEventKind::Drag(MouseButton::Left),
            whole.right() - 1,
            divider.y + 3,
        );
        assert_eq!(
            state.layout(COLS, ROWS).pane_surface.width,
            placement::MIN_TERMINAL_COLS
        );
        mouse(
            &mut state,
            MouseEventKind::Up(MouseButton::Left),
            whole.right() - 1,
            divider.y + 3,
        );
        assert!(state.chrome_drag.is_none());
        assert!(state.investigation().unwrap().dialog.is_none());
        let saved = state.workbench.tabs[0].placement.expect("ratio saved");
        assert_eq!(
            saved.side_permille,
            Some(state.workbench.placement.side_permille)
        );
    }

    #[test]
    fn a_chosen_dock_is_saved_with_the_workspace_entries() {
        let mut state = composed();
        state.investigation_menu_action(DOCK_BOTTOM, &mut ClientShellInput::default());
        state.investigation_menu_action(SHOW_FULL_SCREEN, &mut ClientShellInput::default());

        let saved = state.persisted_investigations();
        let placement = saved[0].placement.expect("placement saved");
        assert_eq!(placement.dock, FilesDock::Bottom);
        assert_eq!(placement.presentation, FilesPresentation::Full);
    }

    /// `composed`, with its tab moved to another workspace than the
    /// focused `ws_1`, shown docked `dock`.
    fn other_workspace_shown(dock: FilesDock) -> ClientShellState {
        let mut state = composed();
        state.workbench.tabs[0].browser.workspace_id = Some("ws_other".into());
        state.workbench.placement.dock = dock;
        state.remember_files_placement();
        state
    }

    #[test]
    fn a_workspace_opening_its_first_investigation_starts_from_the_shown_placement() {
        let mut state = other_workspace_shown(FilesDock::Right);

        state.open_file_viewer_for(Some("ws_1".into()), &mut ClientShellInput::default());
        let opened = state.workbench.tabs.last().expect("opened tab");
        assert_eq!(opened.browser.workspace_id.as_deref(), Some("ws_1"));
        assert_eq!(opened.placement.map(|p| p.dock), Some(FilesDock::Right));
    }

    #[test]
    fn restoring_an_entry_in_the_background_keeps_the_shown_placement() {
        let mut state = other_workspace_shown(FilesDock::Right);
        let mut sibling = super::state().workbench.tabs.remove(0);
        sibling.placement = Some(SavedFilesPlacement {
            dock: FilesDock::Bottom,
            ..Default::default()
        });
        state.workbench.tabs.push(sibling);
        state.workbench.saved = vec![saved_in("local", "ws_1", "/repo")];

        let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
        state.tick_investigation(later, &mut ClientShellInput::default());
        assert_eq!(state.workbench.tabs.len(), 3, "the entry was restored");
        assert_eq!(state.workbench.active, Some(0));
        assert_eq!(state.workbench.placement.dock, FilesDock::Right);
    }

    #[test]
    fn a_chosen_dock_reaches_pending_entries_of_the_workspace() {
        let mut state = composed();
        state.workbench.saved = vec![
            saved_in("local", "ws_1", "/repo"),
            saved_in("local", "ws_other", "/other"),
        ];
        state.investigation_menu_action(DOCK_BOTTOM, &mut ClientShellInput::default());
        assert_eq!(
            state.workbench.saved[0].placement.map(|p| p.dock),
            Some(FilesDock::Bottom)
        );
        assert_eq!(state.workbench.saved[1].placement, None);

        state.remove_investigation(0);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
        state.tick_investigation(later, &mut ClientShellInput::default());
        assert_eq!(state.workbench.tabs.len(), 1, "the entry was restored");
        assert!(state.toggle_investigation(&mut ClientShellInput::default()));
        assert_eq!(state.workbench.placement.dock, FilesDock::Bottom);
    }

    #[test]
    fn showing_a_tab_shows_its_workspace_placement() {
        let mut state = composed();
        state.hide_files();
        state.workbench.tabs[0].placement = Some(SavedFilesPlacement {
            dock: FilesDock::Bottom,
            ..Default::default()
        });

        state.show_files(0);
        assert_eq!(state.workbench.placement.dock, FilesDock::Bottom);
        assert_eq!(
            state.workbench.placement.presentation,
            FilesPresentation::Split
        );
    }

    #[test]
    fn the_tree_menu_offers_the_other_presentation_and_docks() {
        let labels = MenuTarget::Tree.labels(false, FilesPresentation::Split, FilesDock::Left);
        assert!(labels.contains(&SHOW_FULL_SCREEN));
        assert!(!labels.contains(&SHOW_SPLIT));
        assert!(labels.contains(&DOCK_RIGHT) && labels.contains(&DOCK_BOTTOM));
        assert!(!labels.contains(&DOCK_LEFT));

        let labels = MenuTarget::Tree.labels(false, FilesPresentation::Full, FilesDock::Bottom);
        assert!(labels.contains(&SHOW_SPLIT));
        assert!(!labels.contains(&SHOW_FULL_SCREEN));
        assert!(labels.contains(&DOCK_LEFT) && labels.contains(&DOCK_RIGHT));
        assert!(!labels.contains(&DOCK_BOTTOM));
    }

    #[test]
    fn menu_items_switch_presentation_and_dock() {
        let mut state = composed();
        let mut outcome = ClientShellInput::default();
        state.investigation_menu_action(DOCK_BOTTOM, &mut outcome);
        assert_eq!(state.workbench.placement.dock, FilesDock::Bottom);
        assert!(outcome.repaint);

        state.investigation_menu_action(SHOW_FULL_SCREEN, &mut outcome);
        assert_eq!(
            state.workbench.placement.presentation,
            FilesPresentation::Full
        );
        state.investigation_menu_action(DOCK_RIGHT, &mut outcome);
        assert_eq!(state.workbench.placement.dock, FilesDock::Right);
        assert_eq!(
            state.workbench.placement.presentation,
            FilesPresentation::Split
        );
        state.investigation_menu_action(SHOW_FULL_SCREEN, &mut outcome);
        state.investigation_menu_action(SHOW_SPLIT, &mut outcome);
        assert_eq!(
            state.workbench.placement.presentation,
            FilesPresentation::Split
        );
    }

    #[test]
    fn choosing_full_screen_from_the_menu_resizes_the_terminals() {
        let mut state = composed();
        let files = state.layout(COLS, ROWS).files_region.unwrap();
        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Right),
            files.x + 2,
            files.y,
        );
        state.compose(COLS, ROWS);
        let labels = MenuTarget::Tree.labels(false, FilesPresentation::Split, FilesDock::Left);
        let item = labels.iter().position(|l| *l == SHOW_FULL_SCREEN).unwrap();
        let (row, _) = *state
            .investigation_hits
            .menu
            .iter()
            .find(|(_, index)| *index == item)
            .expect("full screen item drawn");

        let outcome = mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            row.x + 1,
            row.y,
        );

        assert!(outcome.resize);
        assert!(state.investigation_full_screen());
    }

    #[test]
    fn moving_the_dock_with_the_same_terminal_size_repaints_without_resizing() {
        let mut state = composed();
        let files = state.layout(COLS, ROWS).files_region.unwrap();
        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Right),
            files.x + 2,
            files.y,
        );
        state.compose(COLS, ROWS);
        let labels = MenuTarget::Tree.labels(false, FilesPresentation::Split, FilesDock::Left);
        let item = labels.iter().position(|l| *l == DOCK_RIGHT).unwrap();
        let (row, _) = *state
            .investigation_hits
            .menu
            .iter()
            .find(|(_, index)| *index == item)
            .expect("dock right item drawn");

        let outcome = mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            row.x + 1,
            row.y,
        );

        assert!(outcome.repaint);
        assert!(!outcome.resize);
        assert_eq!(state.workbench.placement.dock, FilesDock::Right);
    }

    #[test]
    fn split_never_pauses_the_terminal_stream_but_full_screen_does() {
        let mut state = state();
        state.workbench.placement.presentation = FilesPresentation::Split;
        let mut out = ClientShellInput::default();
        state.reconcile_investigation_surface(7, "boot-1", true, false, &mut out);
        assert!(state.file_surface_pause().is_none());

        state.workbench.placement.presentation = FilesPresentation::Full;
        state.reconcile_investigation_surface(7, "boot-1", true, false, &mut out);
        assert!(state.file_surface_pause().is_some());
    }

    #[test]
    fn a_choice_of_split_ignores_an_earlier_narrow_fallback() {
        let mut state = state();
        state.config.mobile_width_threshold = 0;
        state.workbench.placement.presentation = FilesPresentation::Split;
        let sidebar = whole_surface(&state, COLS, ROWS).x;
        let fits = sidebar + placement::MIN_TERMINAL_COLS + placement::MIN_FILES_COLS + 1;
        state.compose(fits - 1, ROWS);
        assert!(state.workbench.placement.narrow());
        let mut outcome = ClientShellInput::default();
        state.investigation_menu_action(SHOW_FULL_SCREEN, &mut outcome);
        state.investigation_menu_action(SHOW_SPLIT, &mut outcome);
        assert!(state.layout(fits, ROWS).files_region.is_some());
    }
}

mod split_patches {
    use super::*;

    fn patch() -> crate::protocol::PaneSurfacePatch {
        let surface = crate::client::shell::tests::surface();
        let mut pane = surface.panes[0].clone();
        pane.content_revision += 1;
        crate::protocol::PaneSurfacePatch {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            base_surface_revision: surface.surface_revision,
            surface_revision: surface.surface_revision + 1,
            rows: vec![crate::protocol::PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![surface.frame.cells[0].clone()],
            }],
            panes: vec![pane],
            cursor: None,
        }
    }

    /// The test surface resized to the terminal side, as the server sends
    /// once it has followed the dock.
    fn docked_surface(state: &ClientShellState) -> crate::protocol::PaneSurfaceFrame {
        let mut surface = crate::client::shell::tests::surface();
        let area = state.layout(160, 32).pane_surface;
        let buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, area.width, area.height));
        surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
        surface
    }

    #[test]
    fn terminal_output_beside_the_focused_files_keeps_the_files_cursor() {
        let mut state = state();
        state.workbench.placement.presentation = FilesPresentation::Split;
        state.remember_files_placement();
        state.show_files(0);
        state.investigation_mut().unwrap().focus = Focus::Paths;
        state.set_pane_surface(docked_surface(&state));
        let frame = state.compose(160, 32).expect("composed split").frame;
        assert!(state.investigation_hits.docked);
        assert!(frame.cursor.is_some());

        let mut terminal_cursor = patch();
        terminal_cursor.cursor = Some(crate::protocol::CursorState {
            x: 0,
            y: 0,
            visible: true,
            shape: Default::default(),
        });
        let ClientPaneSurfacePatchOutcome::Applied(Some(applied)) =
            state.apply_pane_surface_patch(terminal_cursor)
        else {
            panic!("patch fell back to a recompose");
        };
        assert_eq!(applied.cursor, frame.cursor);
    }

    #[test]
    fn terminal_output_beside_unfocused_files_shows_the_terminal_cursor() {
        let mut state = state();
        state.workbench.placement.presentation = FilesPresentation::Split;
        state.set_pane_surface(docked_surface(&state));
        state.compose(160, 32).expect("composed split");
        let terminal = state.layout(160, 32).pane_surface;

        let mut terminal_cursor = patch();
        terminal_cursor.cursor = Some(crate::protocol::CursorState {
            x: 1,
            y: 0,
            visible: true,
            shape: Default::default(),
        });
        let ClientPaneSurfacePatchOutcome::Applied(Some(applied)) =
            state.apply_pane_surface_patch(terminal_cursor)
        else {
            panic!("patch fell back to a recompose");
        };
        assert_eq!(applied.cursor.map(|c| c.x), Some(terminal.x + 1));
    }
}

mod files_menu {
    use super::*;
    use crate::client::shell::{ClientContextMenuOverlay, ClientContextMenuTarget};
    use crossterm::event::{KeyModifiers as M, MouseButton, MouseEvent, MouseEventKind};

    const COLS: u16 = 160;
    const ROWS: u16 = 32;

    /// The workbench docked beside the test pane with the files focused,
    /// composed so hits exist.
    fn composed() -> ClientShellState {
        let mut state = state();
        state.workbench.placement.presentation = FilesPresentation::Split;
        state.remember_files_placement();
        state.show_files(0);
        state.set_pane_surface(crate::client::shell::tests::surface());
        state.compose(COLS, ROWS).expect("composed split");
        state
    }

    fn mouse(state: &mut ClientShellState, button: MouseButton, x: u16, y: u16) {
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(button),
            column: x,
            row: y,
            modifiers: M::empty(),
        })]);
    }

    fn menu(state: &ClientShellState) -> Option<(MenuTarget, MenuPlacement)> {
        match state.investigation()?.dialog {
            Some(Dialog::Menu { target, anchor, .. }) => Some((target, anchor)),
            _ => None,
        }
    }

    fn menu_button(state: &ClientShellState) -> Rect {
        state
            .investigation_hits
            .buttons
            .iter()
            .find(|(_, label)| label.is_none())
            .map(|(rect, _)| *rect)
            .expect("menu button drawn")
    }

    fn menu_box(state: &mut ClientShellState) -> Rect {
        state.compose(COLS, ROWS).expect("composed menu");
        state.investigation_hits.menu_area
    }

    #[test]
    fn right_clicking_the_title_line_opens_the_files_wide_menu_at_the_clicked_cell() {
        let mut state = composed();
        let title = state.investigation_hits.title;
        let (x, y) = (title.x + 4, title.y);

        mouse(&mut state, MouseButton::Right, x, y);

        assert_eq!(
            menu(&state),
            Some((MenuTarget::Tree, MenuPlacement::Pointer { x, y }))
        );
        let area = menu_box(&mut state);
        assert_eq!((area.x, area.y), (x, y));
    }

    #[test]
    fn the_title_line_shows_the_menu_button_without_the_f10_hint() {
        let mut state = composed();
        let frame = state.compose(COLS, ROWS).expect("composed split");
        let title = state.investigation_hits.title;
        let row: String = frame.cells[title.y as usize * frame.width as usize..]
            [..frame.width as usize]
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect();

        assert!(!row.contains("F10"), "{row}");
        let button = menu_button(&state);
        assert_eq!(button.width, 3);
        assert_eq!(button.right(), title.right());
        assert!(row.contains('≡'), "{row}");
    }

    #[test]
    fn clicking_the_menu_button_opens_the_files_wide_menu_beside_it() {
        let mut state = composed();
        let button = menu_button(&state);

        mouse(&mut state, MouseButton::Left, button.x + 1, button.y);

        assert_eq!(
            menu(&state),
            Some((MenuTarget::Tree, MenuPlacement::Header))
        );
        let area = menu_box(&mut state);
        assert_eq!(area.y, button.bottom(), "drawn under the button");
        assert!(area.x <= button.x && area.right() <= COLS);
    }

    #[test]
    fn right_clicking_a_files_tab_activates_it_and_opens_its_files_wide_menu() {
        let mut state = composed();
        let sibling = super::state().workbench.tabs.remove(0);
        state.workbench.tabs.push(sibling);
        state.compose(COLS, ROWS).expect("composed two tabs");
        let (rect, _) = *state
            .investigation_hits
            .tabs
            .iter()
            .find(|(_, index)| *index == 1)
            .expect("second files tab drawn");

        mouse(&mut state, MouseButton::Right, rect.x + 1, rect.y);

        assert_eq!(state.workbench.active, Some(1));
        assert_eq!(
            menu(&state),
            Some((
                MenuTarget::Tree,
                MenuPlacement::Pointer {
                    x: rect.x + 1,
                    y: rect.y
                }
            ))
        );
        assert!(state.overlay.is_none(), "no terminal tab menu");
    }

    #[test]
    fn right_clicking_a_files_tab_keeps_an_open_confirmation() {
        let mut state = composed();
        state.investigation_mut().unwrap().dialog = Some(Dialog::Send {
            path: "/repo/a.txt".into(),
            pane: "pane_1".into(),
            boot: "boot-1".into(),
        });
        let (rect, _) = state.investigation_hits.tabs[0];

        mouse(&mut state, MouseButton::Right, rect.x + 1, rect.y);

        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Send { .. })
        ));
    }

    #[test]
    fn right_clicking_a_terminal_tab_still_opens_the_terminal_tab_menu() {
        let mut state = composed();
        let (rect, tab_id) = state.investigation_hits.terminal_tabs[0].clone();

        mouse(&mut state, MouseButton::Right, rect.x + 1, rect.y);

        assert!(matches!(
            &state.overlay,
            Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
                target: ClientContextMenuTarget::Tab { tab_id: id, .. },
                ..
            })) if *id == tab_id
        ));
        assert!(menu(&state).is_none());
    }

    #[test]
    fn shift_f10_opens_the_files_wide_menu_at_the_menu_button() {
        let mut state = composed();
        let button = menu_button(&state);

        press(&mut state, KeyCode::F(10), M::SHIFT);

        assert_eq!(
            menu(&state),
            Some((MenuTarget::Tree, MenuPlacement::Header))
        );
        assert_eq!(menu_box(&mut state).y, button.bottom());
    }

    #[test]
    fn shift_f10_follows_a_configured_files_menu_key() {
        let mut state = composed();
        state
            .config
            .file_workbench_keys
            .insert("files_menu".into(), "alt+f".into());

        press(&mut state, KeyCode::Char('f'), M::ALT);

        assert_eq!(
            menu(&state),
            Some((MenuTarget::Tree, MenuPlacement::Header))
        );
    }

    #[test]
    fn f10_still_opens_the_menu_of_the_selection() {
        let mut state = composed();
        let tab = state.investigation_mut().unwrap();
        tab.focus = Focus::Results;
        tab.browser.selected = tab
            .browser
            .rows
            .iter()
            .position(|row| row.entry.as_ref().is_some_and(|entry| !entry.is_dir))
            .expect("file row");

        press(&mut state, KeyCode::F(10), M::empty());

        assert_eq!(
            menu(&state),
            Some((MenuTarget::File, MenuPlacement::Selection))
        );
    }

    /// Opens the files-wide menu from the title line's right end, so the
    /// menu runs across the divider of the left dock.
    fn open_menu_across_the_divider(state: &mut ClientShellState) -> Rect {
        let title = state.investigation_hits.title;
        mouse(state, MouseButton::Right, title.right() - 2, title.y);
        let area = menu_box(state);
        let divider = state.hits.files_divider;
        assert!(area.x < divider.x && divider.x < area.right() - 1);
        divider
    }

    fn menu_row(state: &ClientShellState, label: &str) -> Rect {
        let labels = MenuTarget::Tree.labels(false, FilesPresentation::Split, FilesDock::Left);
        state
            .investigation_hits
            .menu
            .iter()
            .find(|(_, index)| labels[*index] == label)
            .map(|(rect, _)| *rect)
            .expect("menu row drawn")
    }

    #[test]
    fn a_files_menu_crossing_the_divider_is_drawn_over_it() {
        let mut state = composed();
        let divider = open_menu_across_the_divider(&mut state);
        let row = menu_row(&state, DOCK_RIGHT);

        let frame = state.compose(COLS, ROWS).expect("composed menu").frame;

        let at = usize::from(row.y) * usize::from(frame.width) + usize::from(divider.x);
        assert_ne!(frame.cells[at].symbol, "│");
    }

    #[test]
    fn a_press_on_the_divider_reaches_an_open_menu_instead_of_dragging() {
        let mut state = composed();
        let divider = open_menu_across_the_divider(&mut state);
        let row = menu_row(&state, DOCK_RIGHT);

        mouse(&mut state, MouseButton::Left, divider.x, row.y);

        assert!(state.chrome_drag.is_none());
        assert_eq!(state.workbench.placement.dock, FilesDock::Right);
    }

    #[test]
    fn a_press_on_the_divider_with_no_menu_open_starts_a_divider_drag() {
        let mut state = composed();
        let divider = state.hits.files_divider;

        mouse(&mut state, MouseButton::Left, divider.x, divider.y + 3);

        assert!(matches!(
            state.chrome_drag,
            Some(crate::client::shell::ClientChromeDrag::FilesDivider)
        ));
    }
}
