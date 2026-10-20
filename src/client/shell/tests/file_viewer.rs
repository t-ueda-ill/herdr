use super::*;
use crate::api::schema::{
    DirectoryEntry, DirectoryEntryKind, DirectoryIndexParams, DirectoryListParams, Method,
    ResponseResult,
};
use crate::protocol::ClientPaneInputEvent;

fn entry(name: &str, kind: DirectoryEntryKind) -> DirectoryEntry {
    DirectoryEntry {
        name: name.into(),
        kind,
        target_kind: None,
        size: None,
        mtime: None,
        hidden: name.starts_with('.'),
        ignored: Some(false),
    }
}

fn ignored(mut entry: DirectoryEntry) -> DirectoryEntry {
    entry.ignored = Some(true);
    entry
}

fn link_to(name: &str, target: DirectoryEntryKind) -> DirectoryEntry {
    let mut entry = entry(name, DirectoryEntryKind::Symlink);
    entry.target_kind = Some(target);
    entry
}

fn listing(path: &str, canonical: &str, entries: Vec<DirectoryEntry>) -> ResponseResult {
    rooted_listing("/repo", path, canonical, entries)
}

/// A listing answered for the live focused pane `pane_1`, whose directory is `root`.
fn rooted_listing(
    root: &str,
    path: &str,
    canonical: &str,
    entries: Vec<DirectoryEntry>,
) -> ResponseResult {
    ResponseResult::DirectoryList {
        root: root.into(),
        path: path.into(),
        canonical: canonical.into(),
        entries,
        truncated: false,
        git_error: None,
        pane_id: Some("pane_1".into()),
        omitted: None,
        unlistable: None,
    }
}

fn repo_listing() -> ResponseResult {
    listing(
        "/repo",
        "/repo",
        vec![
            entry("src", DirectoryEntryKind::Dir),
            ignored(entry("target", DirectoryEntryKind::Dir)),
            link_to("docs", DirectoryEntryKind::Dir),
            entry(".env", DirectoryEntryKind::File),
            entry("my file.txt", DirectoryEntryKind::File),
            entry("README.md", DirectoryEntryKind::File),
        ],
    )
}

fn src_listing() -> ResponseResult {
    listing(
        "/repo/src",
        "/repo/src",
        vec![
            entry("client", DirectoryEntryKind::Dir),
            entry("main.rs", DirectoryEntryKind::File),
        ],
    )
}

fn repo_index() -> ResponseResult {
    ResponseResult::DirectoryIndex {
        repo_root: Some("/repo".into()),
        entries: vec![
            ".env".into(),
            "README.md".into(),
            "my file.txt".into(),
            "src/client/shell.rs".into(),
            "src/main.rs".into(),
        ],
        truncated: false,
        git_error: None,
    }
}

fn shell_state() -> ClientShellState {
    shell_state_with(
        ClientShellConfig::from_config(&Config::default()),
        snapshot(),
    )
}

fn shell_state_with(
    config: ClientShellConfig,
    projection: ClientShellSnapshot,
) -> ClientShellState {
    let mut state = ClientShellState::new(config);
    // Characterize the legacy path picker against servers without file.read.
    state.set_endpoint_methods(Some(
        crate::api::schema::Method::OPERATIONS
            .iter()
            .map(|op| op.name)
            .filter(|name| !name.starts_with("file.") && !name.starts_with("files."))
            .map(str::to_owned)
            .collect(),
    ));
    state.set_snapshot(Box::new(projection));
    state.set_pane_surface(surface());
    state
}

fn endpoint_requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
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

fn pane_events(outcome: &ClientShellInput) -> Vec<ClientPaneInputEvent> {
    pane_events_for(outcome, "pane_1")
}

fn pane_events_for(outcome: &ClientShellInput, target: &str) -> Vec<ClientPaneInputEvent> {
    outcome
        .requests
        .iter()
        .flat_map(|request| match request {
            ClientMessage::ClientShellPaneInput { pane_id, events } if pane_id == target => {
                events.clone()
            }
            _ => Vec::new(),
        })
        .collect()
}

fn any_pane_events(outcome: &ClientShellInput) -> bool {
    outcome
        .requests
        .iter()
        .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. }))
}

fn with_listing(
    mut result: ResponseResult,
    edit: impl FnOnce(&mut Option<String>, &mut Option<u64>, &mut Option<u64>),
) -> ResponseResult {
    if let ResponseResult::DirectoryList {
        pane_id,
        omitted,
        unlistable,
        ..
    } = &mut result
    {
        edit(pane_id, omitted, unlistable);
    }
    result
}

fn index_of(repo_root: &str, entries: &[&str]) -> ResponseResult {
    ResponseResult::DirectoryIndex {
        repo_root: Some(repo_root.into()),
        entries: entries.iter().map(|entry| (*entry).to_owned()).collect(),
        truncated: false,
        git_error: None,
    }
}

/// Answers the only `directory.list` request in `outcome` and returns the
/// outcome of that answer, so a follow-up request can be read from it.
fn answer_listing(
    state: &mut ClientShellState,
    outcome_actions: &[ClientShellAction],
    result: ResponseResult,
) -> Vec<ClientShellAction> {
    let lists: Vec<String> = outcome_actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. }
                if matches!(request.method, Method::DirectoryList(_)) =>
            {
                Some(request.id.clone())
            }
            _ => None,
        })
        .collect();
    let [id] = &lists[..] else {
        panic!("expected exactly one directory.list request: {lists:?}");
    };
    answer(state, id, Ok(result))
}

fn listed_paths(actions: &[ClientShellAction]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::DirectoryList(params) => params.path.clone(),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn clipboard_writes(outcome: &ClientShellInput) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::ClipboardWrite(bytes) => {
                Some(String::from_utf8(bytes.clone()).expect("utf-8 clipboard text"))
            }
            _ => None,
        })
        .collect()
}

/// Opens the viewer and returns the ids of its root listing and index requests.
fn open(state: &mut ClientShellState) -> (String, Option<String>) {
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenFiles),
        &mut outcome,
    );
    let requests = endpoint_requests(&outcome);
    let list = requests
        .iter()
        .find(|(_, method)| matches!(method, Method::DirectoryList(_)))
        .map(|(id, _)| id.clone())
        .expect("opening lists the root through directory.list");
    let index = requests
        .iter()
        .find(|(_, method)| matches!(method, Method::DirectoryIndex(_)))
        .map(|(id, _)| id.clone());
    (list, index)
}

/// Opens the viewer and answers its root listing and index.
fn open_repo(state: &mut ClientShellState) {
    let (list, index) = open(state);
    answer(state, &list, Ok(repo_listing()));
    if let Some(index) = index {
        answer(state, &index, Ok(repo_index()));
    }
}

fn answer(
    state: &mut ClientShellState,
    request_id: &str,
    result: Result<ResponseResult, ClientShellEndpointError>,
) -> Vec<ClientShellAction> {
    state.handle_endpoint_result("boot-1", request_id, result).1
}

fn rejection(code: &str, message: &str) -> Result<ResponseResult, ClientShellEndpointError> {
    Err(ClientShellEndpointError {
        code: Some(code.into()),
        message: message.into(),
    })
}

fn viewer(state: &ClientShellState) -> &ClientFileViewerOverlay {
    match &state.overlay {
        Some(ClientShellOverlay::FileViewer(viewer)) => viewer,
        other => panic!("expected the file viewer, got {other:?}"),
    }
}

/// Visible rows with two spaces of indentation per depth; notices in brackets.
fn rows(state: &ClientShellState) -> Vec<String> {
    viewer(state)
        .rows
        .iter()
        .map(|row| {
            let indent = "  ".repeat(usize::from(row.depth));
            match row.entry {
                Some(_) => format!("{indent}{}", row.label),
                None => format!("{indent}[{}]", row.label),
            }
        })
        .collect()
}

fn selected_label(state: &ClientShellState) -> String {
    let viewer = viewer(state);
    viewer.rows[viewer.selected].label.clone()
}

fn select(state: &mut ClientShellState, label: &str) {
    let position = viewer(state)
        .rows
        .iter()
        .position(|row| row.entry.is_some() && row.label == label)
        .unwrap_or_else(|| panic!("no row {label}: {:?}", rows(state)));
    let Some(ClientShellOverlay::FileViewer(viewer)) = state.overlay.as_mut() else {
        unreachable!();
    };
    viewer.selected = position;
}

fn frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("file viewer frame");
    frame_rows(&frame).join("\n")
}

fn press(state: &mut ClientShellState, code: KeyCode, modifiers: KeyModifiers) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        code, modifiers,
    ))])
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + 1,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

/// A left click reported at `now`, so double-click timing does not depend on
/// how fast the test runs.
fn click_at(state: &mut ClientShellState, rect: Rect, now: std::time::Instant) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    assert!(state.handle_file_viewer_mouse_at(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 1,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        },
        now,
        &mut outcome,
    ));
    outcome
}

fn row_rect(state: &ClientShellState, label: &str) -> Rect {
    let position = viewer(state)
        .rows
        .iter()
        .position(|row| row.entry.is_some() && row.label == label)
        .unwrap_or_else(|| panic!("no row {label}"));
    state
        .hits
        .file_viewer_rows
        .iter()
        .find(|(_, index)| *index == position)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("row {label} was not drawn"))
}

fn button_rect(state: &ClientShellState, button: FileViewerButton) -> Rect {
    state
        .hits
        .file_viewer_buttons
        .iter()
        .find(|(_, hit)| *hit == button)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("button {button:?} was not drawn"))
}

#[test]
fn opening_lists_the_focused_workspace_directory_then_indexes_it() {
    let mut state = shell_state();
    let mut outcome = ClientShellInput::default();

    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenFiles),
        &mut outcome,
    );

    let methods: Vec<Method> = endpoint_requests(&outcome)
        .into_iter()
        .map(|(_, method)| method)
        .collect();
    assert_eq!(
        methods,
        [
            Method::DirectoryList(DirectoryListParams {
                workspace_id: Some("ws_1".into()),
                pane_id: Some("pane_1".into()),
                path: None,
                trust_repository: false,
            }),
            Method::DirectoryIndex(DirectoryIndexParams {
                workspace_id: Some("ws_1".into()),
                pane_id: Some("pane_1".into()),
                path: None,
                trust_repository: false,
            }),
        ]
    );
    assert_eq!(rows(&state), ["[loading…]"]);
    assert!(frame_text(&mut state).contains("indexing…"));
}

#[test]
fn the_tree_marks_directories_and_symlinks_and_hides_dotfiles_and_ignored_entries() {
    let mut state = shell_state();
    let (list, _) = open(&mut state);

    answer(&mut state, &list, Ok(repo_listing()));

    assert_eq!(
        rows(&state),
        ["src/", "docs@", "my file.txt", "README.md"],
        "dotfiles and ignored entries are hidden by default"
    );
    let text = frame_text(&mut state);
    assert!(text.contains("files · /repo"), "{text}");
    assert!(
        text.contains("↵ send · space keep · ^e edit · ^y copy · esc"),
        "{text}"
    );
}

#[test]
fn dot_and_i_toggle_hidden_and_ignored_entries_for_this_session_only() {
    let mut state = shell_state();
    open_repo(&mut state);

    state.handle_input_bytes(b".");
    assert_eq!(
        rows(&state),
        ["src/", "docs@", ".env", "my file.txt", "README.md"]
    );
    state.handle_input_bytes(b"i");
    assert_eq!(
        rows(&state),
        [
            "src/",
            "target/",
            "docs@",
            ".env",
            "my file.txt",
            "README.md"
        ]
    );
    state.handle_input_bytes(b".");
    state.handle_input_bytes(b"i");
    assert_eq!(rows(&state), ["src/", "docs@", "my file.txt", "README.md"]);

    let mut config = Config::default();
    config.ui.file_viewer_show_hidden = true;
    config.ui.file_viewer_show_ignored = true;
    let mut seeded = shell_state_with(ClientShellConfig::from_config(&config), snapshot());
    open_repo(&mut seeded);
    assert!(rows(&seeded).contains(&".env".to_owned()));
    assert!(rows(&seeded).contains(&"target/".to_owned()));
}

#[test]
fn enter_and_arrows_expand_and_collapse_directories() {
    let mut state = shell_state();
    open_repo(&mut state);
    assert_eq!(selected_label(&state), "src/");

    let expand = state.handle_input_bytes(b"\r");

    let requests = endpoint_requests(&expand);
    let [(id, Method::DirectoryList(params))] = &requests[..] else {
        panic!("expanding lists the directory: {requests:?}");
    };
    assert_eq!(params.path.as_deref(), Some("/repo/src"));
    assert_eq!(params.workspace_id.as_deref(), Some("ws_1"));
    assert_eq!(rows(&state)[1], "  [loading…]");
    answer(&mut state, id, Ok(src_listing()));
    assert_eq!(
        rows(&state),
        [
            "src/",
            "  client/",
            "  main.rs",
            "docs@",
            "my file.txt",
            "README.md"
        ]
    );

    press(&mut state, KeyCode::Left, KeyModifiers::empty());
    assert_eq!(rows(&state), ["src/", "docs@", "my file.txt", "README.md"]);
    let again = press(&mut state, KeyCode::Right, KeyModifiers::empty());
    assert!(
        endpoint_requests(&again).is_empty(),
        "a loaded directory expands from what was already listed"
    );
    assert_eq!(rows(&state)[2], "  main.rs");
    state.handle_input_bytes(b"\r");
    assert_eq!(rows(&state), ["src/", "docs@", "my file.txt", "README.md"]);
}

#[test]
fn backspace_raises_the_root_and_fetches_its_tree_and_index_again() {
    let mut state = shell_state();
    open_repo(&mut state);

    let raised = state.handle_input_bytes(b"\x7f");

    let requests = endpoint_requests(&raised);
    let [(list_id, Method::DirectoryList(list)), (_, Method::DirectoryIndex(index))] =
        &requests[..]
    else {
        panic!("raising the root lists and indexes it again: {requests:?}");
    };
    assert_eq!(list.path.as_deref(), Some("/"));
    assert_eq!(index.path.as_deref(), Some("/"));
    answer(
        &mut state,
        list_id,
        Ok(listing(
            "/",
            "/",
            vec![
                entry("etc", DirectoryEntryKind::Dir),
                entry("repo", DirectoryEntryKind::Dir),
            ],
        )),
    );

    assert_eq!(rows(&state), ["etc/", "repo/"]);
    assert_eq!(
        selected_label(&state),
        "repo/",
        "the previous root stays selected"
    );
    assert!(frame_text(&mut state).contains("files · / "));
    let top = state.handle_input_bytes(b"\x7f");
    assert!(
        endpoint_requests(&top).is_empty(),
        "the filesystem root has no parent"
    );
}

#[test]
fn a_symlink_back_to_an_ancestor_is_refused_as_a_loop() {
    let mut state = shell_state();
    open_repo(&mut state);
    select(&mut state, "docs@");

    let expand = state.handle_input_bytes(b"\r");

    let requests = endpoint_requests(&expand);
    let [(id, Method::DirectoryList(params))] = &requests[..] else {
        panic!("a symlink to a directory expands through the server: {requests:?}");
    };
    assert_eq!(params.path.as_deref(), Some("/repo/docs"));
    answer(
        &mut state,
        id,
        Ok(listing(
            "/repo/docs",
            "/repo",
            vec![entry("src", DirectoryEntryKind::Dir)],
        )),
    );
    assert_eq!(
        rows(&state),
        ["src/", "docs@", "  [loop]", "my file.txt", "README.md"]
    );

    // A link to a directory above the root contains the root, so it loops too.
    select(&mut state, "src/");
    let expand = state.handle_input_bytes(b"\r");
    answer_listing(
        &mut state,
        &expand.actions,
        listing(
            "/repo/src",
            "/repo/src",
            vec![
                link_to("up", DirectoryEntryKind::Dir),
                link_to("elsewhere", DirectoryEntryKind::Dir),
            ],
        ),
    );
    select(&mut state, "up@");
    let up = state.handle_input_bytes(b"\r");
    answer_listing(
        &mut state,
        &up.actions,
        listing(
            "/repo/src/up",
            "/",
            vec![entry("etc", DirectoryEntryKind::Dir)],
        ),
    );
    select(&mut state, "elsewhere@");
    let elsewhere = state.handle_input_bytes(b"\r");
    answer_listing(
        &mut state,
        &elsewhere.actions,
        listing(
            "/repo/src/elsewhere",
            "/other",
            vec![entry("notes.md", DirectoryEntryKind::File)],
        ),
    );
    assert_eq!(
        rows(&state)[..6],
        [
            "src/",
            "  up@",
            "    [loop]",
            "  elsewhere@",
            "    notes.md",
            "docs@"
        ],
        "a link outside every ancestor expands normally"
    );
}

#[test]
fn inline_notices_report_directory_failures_truncation_and_git_errors_without_toasts() {
    let mut state = shell_state();
    let (list, _) = open(&mut state);
    let mut root_listing = repo_listing();
    if let ResponseResult::DirectoryList {
        truncated,
        git_error,
        omitted,
        ..
    } = &mut root_listing
    {
        *truncated = true;
        *git_error = Some("fatal: detected dubious ownership in repository at '/repo'".into());
        *omitted = Some(512);
    }
    answer(&mut state, &list, Ok(root_listing));
    assert_eq!(
        rows(&state),
        [
            "[untrusted repository]",
            "src/",
            "docs@",
            "my file.txt",
            "README.md",
            "[+512 more]",
        ]
    );

    select(&mut state, "src/");
    let expand = state.handle_input_bytes(b"\r");
    let (id, _) = endpoint_requests(&expand).remove(0);
    answer(
        &mut state,
        &id,
        rejection("permission_denied", "permission denied: /repo/src"),
    );

    assert_eq!(rows(&state)[2], "  [permission denied]");
    assert!(
        state.visible_endpoint_notice.is_none(),
        "directory failures stay inline"
    );
}

#[test]
fn search_ranks_the_repository_index_including_derived_directories() {
    let mut state = shell_state();
    open_repo(&mut state);

    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"mainrs");
    assert_eq!(rows(&state)[0], "src/main.rs");
    assert!(frame_text(&mut state).contains("files · /repo (search)"));

    press(&mut state, KeyCode::Esc, KeyModifiers::empty());
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"client");
    assert_eq!(rows(&state)[0], "src/client/");
    assert!(rows(&state).contains(&"src/client/shell.rs".to_owned()));
}

#[test]
fn esc_in_the_filter_returns_to_the_tree_and_esc_in_the_tree_closes() {
    let mut state = shell_state();
    open_repo(&mut state);
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"readme");
    assert_eq!(rows(&state), ["README.md"]);

    press(&mut state, KeyCode::Esc, KeyModifiers::empty());

    assert!(viewer(&state).query.is_empty());
    assert!(!viewer(&state).search_focused);
    assert_eq!(rows(&state), ["src/", "docs@", "my file.txt", "README.md"]);
    press(&mut state, KeyCode::Esc, KeyModifiers::empty());
    assert!(state.overlay.is_none());
}

#[test]
fn the_filter_takes_no_spaces_and_keeps_enter_space_and_ctrl_keys_as_actions() {
    let mut state = shell_state();
    open_repo(&mut state);
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"readme");

    let keep = state.handle_input_bytes(b" ");
    assert_eq!(viewer(&state).query.as_str(), "readme");
    assert_eq!(
        pane_events(&keep),
        [ClientPaneInputEvent::TextCommit("/repo/README.md ".into())]
    );

    let copy = state.handle_input_bytes(b"\x19");
    assert_eq!(clipboard_writes(&copy), ["/repo/README.md"]);
    assert_eq!(viewer(&state).query.as_str(), "readme");

    let edit = state.handle_input_bytes(b"\x05");
    let requests = endpoint_requests(&edit);
    let [(_, Method::PaneOpenEditor(params))] = &requests[..] else {
        panic!("ctrl+e opens the editor through pane.open_editor: {requests:?}");
    };
    assert_eq!(params.path, "/repo/README.md");
    assert_eq!(params.pane_id.as_deref(), Some("pane_1"));
    assert_eq!(viewer(&state).query.as_str(), "readme");

    state.insert_overlay_text("  md ");
    assert_eq!(viewer(&state).query.as_str(), "readmemd");
    press(&mut state, KeyCode::Backspace, KeyModifiers::empty());
    press(&mut state, KeyCode::Backspace, KeyModifiers::empty());
    let send = state.handle_input_bytes(b"\r");
    assert_eq!(
        pane_events(&send),
        [ClientPaneInputEvent::TextCommit("/repo/README.md ".into())]
    );
    assert!(state.overlay.is_none(), "enter on a file sends and closes");
}

#[test]
fn enter_on_a_directory_result_returns_to_the_tree_and_expands_it() {
    let mut state = shell_state();
    open_repo(&mut state);
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"src");
    assert_eq!(selected_label(&state), "src/");

    let reveal = state.handle_input_bytes(b"\r");

    assert!(pane_events(&reveal).is_empty(), "a directory is not sent");
    assert!(viewer(&state).query.is_empty());
    assert!(!viewer(&state).search_focused);
    let requests = endpoint_requests(&reveal);
    let [(id, Method::DirectoryList(params))] = &requests[..] else {
        panic!("revealing lists the directory: {requests:?}");
    };
    assert_eq!(params.path.as_deref(), Some("/repo/src"));
    answer(&mut state, id, Ok(src_listing()));
    assert_eq!(rows(&state)[1], "  client/");
    assert_eq!(selected_label(&state), "src/");
}

#[test]
fn sent_paths_are_shell_quoted_absolute_paths_with_one_trailing_space() {
    let mut state = shell_state();
    open_repo(&mut state);
    select(&mut state, "my file.txt");

    let keep = state.handle_input_bytes(b" ");
    assert_eq!(
        pane_events(&keep),
        [ClientPaneInputEvent::TextCommit(
            "'/repo/my file.txt' ".into()
        )]
    );
    assert!(state.overlay.is_some(), "space keeps the viewer open");

    select(&mut state, "src/");
    let directory = state.handle_input_bytes(b" ");
    assert_eq!(
        pane_events(&directory),
        [ClientPaneInputEvent::TextCommit("/repo/src ".into())],
        "space sends directories too"
    );

    let mut alternate = surface();
    alternate.panes[0].alternate_screen_active = true;
    state.set_pane_surface(alternate);
    select(&mut state, "README.md");
    let paste = state.handle_input_bytes(b"\r");
    assert_eq!(
        pane_events(&paste),
        [ClientPaneInputEvent::Paste("/repo/README.md ".into())],
        "an alternate-screen app receives a paste, bracketed when it asked for it"
    );
}

#[test]
fn names_with_control_characters_are_never_sent_but_can_still_be_copied() {
    const REFUSED: &str = "path contains control characters; copy instead";
    let newline = "evil\nrm -rf ~";
    let paste_end = "end\u{1b}[201~paste";
    let mut state = shell_state();
    let (list, index) = open(&mut state);
    answer(
        &mut state,
        &list,
        Ok(listing(
            "/repo",
            "/repo",
            vec![
                entry(newline, DirectoryEntryKind::File),
                entry(paste_end, DirectoryEntryKind::File),
                entry("README.md", DirectoryEntryKind::File),
            ],
        )),
    );
    if let Some(index) = index {
        answer(&mut state, &index, Ok(repo_index()));
    }

    for alternate_screen in [false, true] {
        let mut pane_surface = surface();
        pane_surface.panes[0].alternate_screen_active = alternate_screen;
        state.set_pane_surface(pane_surface);
        for name in [newline, paste_end] {
            for key in [b"\r", b" "] {
                select(&mut state, name);
                if let Some(ClientShellOverlay::FileViewer(viewer)) = state.overlay.as_mut() {
                    viewer.notice = None;
                }
                let refused = state.handle_input_bytes(key);
                let case = format!("{name:?} key {key:?} alternate screen {alternate_screen}");
                assert!(!any_pane_events(&refused), "{case}");
                assert!(state.overlay.is_some(), "{case}");
                assert_eq!(viewer(&state).notice.as_deref(), Some(REFUSED), "{case}");
            }
        }
    }
    let text = frame_text(&mut state);
    assert!(text.contains(REFUSED), "{text}");

    select(&mut state, newline);
    let copy = state.handle_input_bytes(b"\x19");
    assert_eq!(clipboard_writes(&copy), [format!("/repo/{newline}")]);

    state.set_pane_surface(surface());
    select(&mut state, "README.md");
    let typed = state.handle_input_bytes(b" ");
    assert_eq!(
        pane_events(&typed),
        [ClientPaneInputEvent::TextCommit("/repo/README.md ".into())]
    );
    let mut alternate = surface();
    alternate.panes[0].alternate_screen_active = true;
    state.set_pane_surface(alternate);
    let pasted = state.handle_input_bytes(b"\r");
    assert_eq!(
        pane_events(&pasted),
        [ClientPaneInputEvent::Paste("/repo/README.md ".into())]
    );
}

#[test]
fn sending_leaves_copy_mode_first_but_opening_does_not() {
    let mut state = shell_state();
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 30).expect("composed frame");
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut ClientShellInput::default(),
    );
    assert_eq!(state.mode, ClientShellMode::Copy);

    open_repo(&mut state);
    assert_eq!(state.mode, ClientShellMode::Copy, "opening keeps copy mode");
    select(&mut state, "README.md");
    let send = state.handle_input_bytes(b"\r");

    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(state.copy_mode.is_none());
    assert_eq!(
        pane_events(&send),
        [ClientPaneInputEvent::TextCommit("/repo/README.md ".into())]
    );
}

#[test]
fn without_a_live_pane_send_and_edit_are_refused_but_copy_works() {
    let mut projection = snapshot();
    projection.focused_pane_id = None;
    projection.panes.clear();
    let mut state = shell_state_with(
        ClientShellConfig::from_config(&Config::default()),
        projection,
    );
    open_repo(&mut state);
    select(&mut state, "README.md");

    let send = state.handle_input_bytes(b"\r");
    let edit = state.handle_input_bytes(b"\x05");
    let copy = state.handle_input_bytes(b"\x19");

    assert!(pane_events(&send).is_empty());
    assert!(endpoint_requests(&edit).is_empty());
    assert!(state.overlay.is_some());
    assert_eq!(clipboard_writes(&copy), ["/repo/README.md"]);
    state.handle_input_bytes(b"\r");
    assert!(frame_text(&mut state).contains("no live pane to send to"));
}

#[test]
fn ctrl_e_refuses_directories_and_closes_the_viewer_once_the_editor_opens() {
    let mut state = shell_state();
    open_repo(&mut state);

    let refused = state.handle_input_bytes(b"\x05");
    assert!(endpoint_requests(&refused).is_empty());
    assert_eq!(
        viewer(&state).notice.as_deref(),
        Some("edit opens files only")
    );

    select(&mut state, "README.md");
    let edit = state.handle_input_bytes(b"\x05");
    let (id, _) = endpoint_requests(&edit).remove(0);
    answer(
        &mut state,
        &id,
        rejection("editor_failed", "could not start the editor"),
    );
    assert_eq!(
        viewer(&state).notice.as_deref(),
        Some("could not start the editor")
    );
    assert!(state.visible_endpoint_notice.is_none());

    let edit = state.handle_input_bytes(b"\x05");
    let (id, _) = endpoint_requests(&edit).remove(0);
    let pane: crate::api::schema::PaneInfo = serde_json::from_value(serde_json::json!({
        "pane_id": "pane_9", "terminal_id": "t9", "workspace_id": "ws_1", "tab_id": "tab_1",
        "focused": true, "agent_status": "unknown", "revision": 0
    }))
    .expect("pane info");
    answer(&mut state, &id, Ok(ResponseResult::PaneInfo { pane }));
    assert!(state.overlay.is_none());
}

#[test]
fn without_directory_list_opening_only_raises_the_unsupported_notice() {
    let mut state = shell_state();
    state.set_endpoint_methods(Some(vec!["pane.focus".into()]));
    let mut outcome = ClientShellInput::default();

    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenFiles),
        &mut outcome,
    );

    assert!(outcome.actions.is_empty());
    assert!(state.overlay.is_none());
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("unsupported notice");
    assert_eq!(notice.title, "Action unavailable");
    assert!(notice.body.contains("directory.list"), "{}", notice.body);
}

#[test]
fn without_directory_index_search_filters_the_visible_rows_by_substring() {
    let mut state = shell_state();
    state.set_endpoint_methods(Some(vec![
        "directory.list".into(),
        "pane.open_editor".into(),
    ]));

    let (list, index) = open(&mut state);
    assert!(
        index.is_none(),
        "an endpoint without directory.index is not asked"
    );
    assert!(state.visible_endpoint_notice.is_none());
    answer(&mut state, &list, Ok(repo_listing()));
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"file");

    assert_eq!(rows(&state), ["my file.txt"]);
    let text = frame_text(&mut state);
    assert!(!text.contains("(search)"), "{text}");
    assert!(!text.contains("indexing…"), "{text}");
    state.handle_input_bytes(b"\x7f\x7f\x7f\x7f");
    state.handle_input_bytes(b"mft");
    assert!(
        rows(&state).is_empty() || rows(&state).iter().all(|row| row.starts_with('[')),
        "no fuzzy matching without the index: {:?}",
        rows(&state)
    );
}

#[test]
fn index_notices_report_truncation_and_timeouts() {
    let mut state = shell_state();
    let (list, index) = open(&mut state);
    answer(&mut state, &list, Ok(repo_listing()));
    let ResponseResult::DirectoryIndex {
        repo_root, entries, ..
    } = repo_index()
    else {
        unreachable!();
    };
    answer(
        &mut state,
        &index.expect("index request"),
        Ok(ResponseResult::DirectoryIndex {
            repo_root,
            entries,
            truncated: true,
            git_error: None,
        }),
    );
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"md");
    assert_eq!(
        rows(&state).last().map(String::as_str),
        Some("[index truncated (50,000)]")
    );

    let mut state = shell_state();
    let (list, index) = open(&mut state);
    answer(&mut state, &list, Ok(repo_listing()));
    answer(
        &mut state,
        &index.expect("index request"),
        Ok(ResponseResult::DirectoryIndex {
            repo_root: None,
            entries: Vec::new(),
            truncated: true,
            git_error: Some("git timed out".into()),
        }),
    );
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"read");
    assert_eq!(rows(&state), ["README.md", "[index timed out]"]);
}

#[test]
fn opening_for_another_workspace_focuses_it_first() {
    let mut projection = snapshot();
    let mut second = projection.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.focused = false;
    projection.workspaces.push(second);
    let mut state = shell_state_with(
        ClientShellConfig::from_config(&Config::default()),
        projection,
    );
    let mut outcome = ClientShellInput::default();

    state.open_file_viewer_for(Some("ws_2".into()), &mut outcome);

    let methods: Vec<Method> = endpoint_requests(&outcome)
        .into_iter()
        .map(|(_, method)| method)
        .collect();
    assert!(
        matches!(
            &methods[..],
            [
                Method::WorkspaceFocus(target),
                Method::DirectoryList(DirectoryListParams { workspace_id: Some(list_ws), .. }),
                Method::DirectoryIndex(_),
            ] if target.workspace_id == "ws_2" && list_ws == "ws_2"
        ),
        "{methods:?}"
    );
}

#[test]
fn off_host_panes_mark_the_root_as_a_host_path() {
    let mut projection = snapshot();
    projection.panes[0].container = Some("docker:web".into());
    let mut state = shell_state_with(
        ClientShellConfig::from_config(&Config::default()),
        projection,
    );

    open_repo(&mut state);

    assert!(frame_text(&mut state).contains("files · host:/repo"));
}

#[test]
fn switching_endpoints_closes_the_file_viewer() {
    let mut state = shell_state();
    open_repo(&mut state);
    let profile = crate::client::endpoint::SavedSshEndpoint {
        id: crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef")
            .expect("profile id"),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    let mut projection = snapshot();
    projection.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote, Box::new(projection));

    assert!(state.activate_endpoint_projection(&remote));

    assert!(state.overlay.is_none());
}

#[test]
fn mouse_selects_toggles_sends_focuses_the_filter_and_closes_outside() {
    let mut state = shell_state();
    open_repo(&mut state);
    frame_text(&mut state);

    let expand = {
        let rect = row_rect(&state, "src/");
        click(&mut state, rect)
    };
    assert!(matches!(
        &endpoint_requests(&expand)[..],
        [(_, Method::DirectoryList(params))] if params.path.as_deref() == Some("/repo/src")
    ));
    frame_text(&mut state);

    let first = {
        let rect = row_rect(&state, "README.md");
        click(&mut state, rect)
    };
    assert!(pane_events(&first).is_empty());
    assert_eq!(selected_label(&state), "README.md");
    let copy = {
        let rect = button_rect(&state, FileViewerButton::Copy);
        click(&mut state, rect)
    };
    assert_eq!(clipboard_writes(&copy), ["/repo/README.md"]);

    frame_text(&mut state);
    let filter = state.hits.file_viewer_filter;
    click(&mut state, filter);
    assert!(viewer(&state).search_focused);
    press(&mut state, KeyCode::Esc, KeyModifiers::empty());

    frame_text(&mut state);
    let start = std::time::Instant::now();
    let rect = row_rect(&state, "README.md");
    click_at(&mut state, rect, start);
    let double = click_at(
        &mut state,
        rect,
        start + std::time::Duration::from_millis(300),
    );
    assert_eq!(
        pane_events(&double),
        [ClientPaneInputEvent::TextCommit("/repo/README.md ".into())]
    );
    assert!(state.overlay.is_none(), "double click sends and closes");

    open_repo(&mut state);
    frame_text(&mut state);
    let popup = state.hits.file_viewer_popup;
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: popup.x.saturating_sub(1),
        row: popup.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.overlay.is_none(), "a click outside closes the viewer");
}

fn methods(outcome: &ClientShellInput) -> Vec<Method> {
    endpoint_requests(outcome)
        .into_iter()
        .map(|(_, method)| method)
        .collect()
}

fn two_workspaces() -> ClientShellSnapshot {
    let mut projection = snapshot();
    let mut second = projection.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.active_tab_id = "tab_2".into();
    second.number = 2;
    second.label = "second".into();
    second.focused = false;
    projection.workspaces.push(second);
    projection
}

#[test]
fn every_workspace_context_menu_ends_with_browse_files() {
    for (is_git, is_linked_worktree, has_worktree_children) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let menu = ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace {
                workspace_id: "ws_1".into(),
                is_git,
                is_linked_worktree,
                has_worktree_children,
                close_group: false,
                collapsed: false,
            },
            x: 0,
            y: 0,
            highlighted: 0,
        };
        let items = menu.items();
        let last = items.last().expect("workspace menu items");
        assert_eq!(
            last.label, "Browse files...",
            "{is_git} {is_linked_worktree}"
        );
        assert_eq!(last.action, ClientContextMenuAction::BrowseFiles);
    }
}

#[test]
fn browse_files_on_the_focused_workspace_row_opens_the_viewer_there() {
    let mut state = shell_state();
    state.compose(106, 30).expect("composed frame");
    let row = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.workspace_id == "ws_1")
        .map(|hit| hit.rect)
        .expect("workspace row");
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::BrowseFiles)
            .expect("browse files item"),
        _ => panic!("workspace context menu"),
    };
    state.compose(106, 30).expect("context menu");
    let item = state.hits.context_menu_rows[index].0;

    let outcome = click(&mut state, item);

    assert!(
        matches!(
            &methods(&outcome)[..],
            [
                Method::DirectoryList(DirectoryListParams { workspace_id: Some(list_ws), .. }),
                Method::DirectoryIndex(DirectoryIndexParams { workspace_id: Some(index_ws), .. }),
            ] if list_ws == "ws_1" && index_ws == "ws_1"
        ),
        "{:?}",
        methods(&outcome)
    );
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::FileViewer(_))
    ));
}

#[test]
fn browse_files_on_another_workspace_focuses_that_workspace_first() {
    let mut state = shell_state_with(
        ClientShellConfig::from_config(&Config::default()),
        two_workspaces(),
    );
    state.open_workspace_context_menu("ws_2".into(), 0, 0);
    let index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::BrowseFiles)
            .expect("browse files item"),
        _ => panic!("workspace context menu"),
    };
    let mut outcome = ClientShellInput::default();

    state.activate_context_menu_item(index, &mut outcome);

    assert!(
        matches!(
            &methods(&outcome)[..],
            [
                Method::WorkspaceFocus(target),
                Method::DirectoryList(DirectoryListParams { workspace_id: Some(list_ws), .. }),
                Method::DirectoryIndex(_),
            ] if target.workspace_id == "ws_2" && list_ws == "ws_2"
        ),
        "{:?}",
        methods(&outcome)
    );
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::FileViewer(_))
    ));
}

#[test]
fn the_global_menu_offers_open_files_before_the_remote_pickers() {
    let projection = snapshot();
    let labels: Vec<&str> = super::super::global_menu::global_menu_items(&projection)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    let files = labels
        .iter()
        .position(|label| *label == "open files")
        .expect("global menu entry");
    assert_eq!(labels[files + 1], "open container");

    let mut state = shell_state();
    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: files,
    }));
    let outcome = state.handle_input_bytes(b"\r");

    assert!(
        matches!(
            &methods(&outcome)[..],
            [
                Method::DirectoryList(DirectoryListParams { workspace_id: Some(list_ws), .. }),
                Method::DirectoryIndex(_),
            ] if list_ws == "ws_1"
        ),
        "{:?}",
        methods(&outcome)
    );
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::FileViewer(_))
    ));
}

#[test]
fn the_open_files_binding_opens_the_viewer_and_only_through_the_prefix_in_copy_mode() {
    let config: Config =
        toml::from_str("[keys]\nopen_files = [\"f9\", \"prefix+f\"]\n").expect("config");
    let mut state = shell_state_with(ClientShellConfig::from_config(&config), snapshot());
    let direct = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::F(9),
        KeyModifiers::empty(),
    ))]);
    assert!(methods(&direct)
        .iter()
        .any(|method| matches!(method, Method::DirectoryList(_))));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::FileViewer(_))
    ));
    state.overlay = None;

    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 30).expect("composed frame");
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut ClientShellInput::default(),
    );
    assert_eq!(state.mode, ClientShellMode::Copy);

    let ignored = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::F(9), KeyModifiers::empty()),
    )]);
    assert!(
        !methods(&ignored)
            .iter()
            .any(|method| matches!(method, Method::DirectoryList(_))),
        "direct chords stay with copy mode"
    );
    assert!(state.overlay.is_none());
    assert_eq!(state.mode, ClientShellMode::Copy);

    state.handle_input_bytes(b"\x02");
    let prefixed = state.handle_input_bytes(b"f");
    assert!(methods(&prefixed)
        .iter()
        .any(|method| matches!(method, Method::DirectoryList(_))));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::FileViewer(_))
    ));
    assert_eq!(state.mode, ClientShellMode::Copy, "opening keeps copy mode");
}

#[test]
fn clicks_further_apart_than_a_double_click_only_select() {
    let mut state = shell_state();
    open_repo(&mut state);
    frame_text(&mut state);
    let start = std::time::Instant::now();
    let rect = row_rect(&state, "README.md");

    click_at(&mut state, rect, start);
    let late = click_at(
        &mut state,
        rect,
        start + std::time::Duration::from_millis(351),
    );

    assert!(pane_events(&late).is_empty());
    assert!(state.overlay.is_some());
    assert_eq!(selected_label(&state), "README.md");
}

#[test]
fn arrows_ctrl_n_p_and_the_wheel_move_over_selectable_rows_and_stop_at_the_ends() {
    let mut state = shell_state();
    open_repo(&mut state);
    frame_text(&mut state);
    assert_eq!(selected_label(&state), "src/");

    press(&mut state, KeyCode::Down, KeyModifiers::empty());
    assert_eq!(selected_label(&state), "docs@");
    press(&mut state, KeyCode::Char('n'), KeyModifiers::CONTROL);
    assert_eq!(selected_label(&state), "my file.txt");
    press(&mut state, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert_eq!(selected_label(&state), "docs@");
    press(&mut state, KeyCode::Up, KeyModifiers::empty());
    press(&mut state, KeyCode::Up, KeyModifiers::empty());
    assert_eq!(selected_label(&state), "src/", "up stops at the first row");

    let popup = state.hits.file_viewer_popup;
    let wheel = |state: &mut ClientShellState, kind| {
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind,
            column: popup.x + 2,
            row: popup.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
    };
    wheel(&mut state, MouseEventKind::ScrollDown);
    assert_eq!(
        selected_label(&state),
        "README.md",
        "the wheel moves three rows"
    );
    press(&mut state, KeyCode::Down, KeyModifiers::empty());
    assert_eq!(
        selected_label(&state),
        "README.md",
        "down stops at the last row"
    );
    wheel(&mut state, MouseEventKind::ScrollUp);
    assert_eq!(selected_label(&state), "src/");
}

#[test]
fn inline_notices_report_empty_hidden_only_unlistable_and_missing_directories() {
    let mut state = shell_state();
    open_repo(&mut state);

    let expand = state.handle_input_bytes(b"\r");
    answer_listing(
        &mut state,
        &expand.actions,
        listing(
            "/repo/src",
            "/repo/src",
            vec![
                entry(".hidden", DirectoryEntryKind::File),
                ignored(entry("ignored.log", DirectoryEntryKind::File)),
            ],
        ),
    );
    assert_eq!(rows(&state)[1], "  [2 hidden]");

    select(&mut state, "docs@");
    let expand = state.handle_input_bytes(b"\r");
    answer_listing(
        &mut state,
        &expand.actions,
        listing("/repo/docs", "/repo/docs", Vec::new()),
    );
    assert_eq!(rows(&state)[3], "  [empty]");

    let mut state = shell_state();
    let (list, _) = open(&mut state);
    answer(
        &mut state,
        &list,
        Ok(with_listing(repo_listing(), |_, _, unlistable| {
            *unlistable = Some(2);
        })),
    );
    assert_eq!(
        rows(&state).last().map(String::as_str),
        Some("[2 unlistable]")
    );

    select(&mut state, "src/");
    let expand = state.handle_input_bytes(b"\r");
    let (id, _) = endpoint_requests(&expand).remove(0);
    answer(
        &mut state,
        &id,
        rejection("not_found", "not found: /repo/src"),
    );
    assert_eq!(rows(&state)[1], "  [not found]");
}

#[test]
fn a_root_from_a_pane_without_a_live_shell_disables_send_and_edit_but_not_copy() {
    let mut state = shell_state();
    let (list, _) = open(&mut state);
    answer(
        &mut state,
        &list,
        Ok(with_listing(repo_listing(), |pane_id, _, _| {
            *pane_id = None
        })),
    );
    select(&mut state, "README.md");

    let keep = state.handle_input_bytes(b" ");
    let send = state.handle_input_bytes(b"\r");
    let edit = state.handle_input_bytes(b"\x05");
    let copy = state.handle_input_bytes(b"\x19");

    assert!(!any_pane_events(&keep));
    assert!(!any_pane_events(&send));
    assert!(endpoint_requests(&edit).is_empty());
    assert!(state.overlay.is_some());
    assert_eq!(clipboard_writes(&copy), ["/repo/README.md"]);
    state.handle_input_bytes(b"\r");
    let text = frame_text(&mut state);
    assert!(text.contains("no live pane to send to"), "{text}");
}

fn second_workspace_with_panes(
    first: (&str, Option<&str>),
    second: (&str, Option<&str>),
) -> ClientShellSnapshot {
    let mut projection = two_workspaces();
    for (pane_id, container) in [first, second] {
        let mut pane = projection.panes[0].clone();
        pane.pane_id = pane_id.into();
        pane.workspace_id = "ws_2".into();
        pane.tab_id = "tab_2".into();
        pane.focused = false;
        pane.container = container.map(str::to_owned);
        projection.panes.push(pane);
    }
    projection
}

#[test]
fn the_host_title_follows_the_pane_the_listing_names() {
    let mut state = shell_state_with(
        ClientShellConfig::from_config(&Config::default()),
        second_workspace_with_panes(("pane_2a", None), ("pane_2b", Some("docker:web"))),
    );
    let mut outcome = ClientShellInput::default();
    state.open_file_viewer_for(Some("ws_2".into()), &mut outcome);

    answer_listing(
        &mut state,
        &outcome.actions,
        with_listing(repo_listing(), |pane_id, _, _| {
            *pane_id = Some("pane_2b".into());
        }),
    );

    let text = frame_text(&mut state);
    assert!(text.contains("files · host:/repo"), "{text}");
}

#[test]
fn sends_wait_until_the_listed_pane_holds_focus() {
    let projection = second_workspace_with_panes(("pane_2a", None), ("pane_2b", None));
    let mut state = shell_state_with(
        ClientShellConfig::from_config(&Config::default()),
        projection.clone(),
    );
    let mut outcome = ClientShellInput::default();
    state.open_file_viewer_for(Some("ws_2".into()), &mut outcome);
    answer_listing(
        &mut state,
        &outcome.actions,
        with_listing(repo_listing(), |pane_id, _, _| {
            *pane_id = Some("pane_2a".into());
        }),
    );
    select(&mut state, "README.md");

    let early = state.handle_input_bytes(b" ");
    let early_edit = state.handle_input_bytes(b"\x05");

    assert!(
        !any_pane_events(&early),
        "the old workspace's pane must not get a path shown for another one"
    );
    assert!(endpoint_requests(&early_edit).is_empty());
    assert_eq!(
        viewer(&state).notice.as_deref(),
        Some("listed pane is not focused")
    );

    let mut focused = projection;
    focused.focused_workspace_id = Some("ws_2".into());
    focused.focused_tab_id = Some("tab_2".into());
    focused.focused_pane_id = Some("pane_2a".into());
    state.set_snapshot(Box::new(focused));
    let keep = state.handle_input_bytes(b" ");
    let edit = state.handle_input_bytes(b"\x05");

    assert_eq!(
        pane_events_for(&keep, "pane_2a"),
        [ClientPaneInputEvent::TextCommit("/repo/README.md ".into())]
    );
    let requests = endpoint_requests(&edit);
    let [(_, Method::PaneOpenEditor(params))] = &requests[..] else {
        panic!("ctrl+e opens the editor once the pane is focused: {requests:?}");
    };
    assert_eq!(params.pane_id.as_deref(), Some("pane_2a"));
    assert_eq!(params.workspace_id.as_deref(), Some("ws_2"));
}

#[test]
fn an_editor_answer_for_an_earlier_viewer_leaves_a_reopened_one_open() {
    let mut state = shell_state();
    open_repo(&mut state);
    select(&mut state, "README.md");
    let edit = state.handle_input_bytes(b"\x05");
    let (id, _) = endpoint_requests(&edit).remove(0);
    press(&mut state, KeyCode::Esc, KeyModifiers::empty());
    open_repo(&mut state);

    let pane: crate::api::schema::PaneInfo = serde_json::from_value(serde_json::json!({
        "pane_id": "pane_9", "terminal_id": "t9", "workspace_id": "ws_1", "tab_id": "tab_1",
        "focused": true, "agent_status": "unknown", "revision": 0
    }))
    .expect("pane info");
    answer(&mut state, &id, Ok(ResponseResult::PaneInfo { pane }));

    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::FileViewer(_))
    ));
}

#[test]
fn the_filter_never_holds_whitespace_from_shifted_or_host_generated_keys() {
    let mut state = shell_state();
    open_repo(&mut state);
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"readme");

    let shifted = press(&mut state, KeyCode::Char(' '), KeyModifiers::SHIFT);
    assert_eq!(viewer(&state).query.as_str(), "readme");
    assert_eq!(
        pane_events(&shifted),
        [ClientPaneInputEvent::TextCommit("/repo/README.md ".into())],
        "shift+space keeps the viewer open like space"
    );

    let host_text = |text: &str, code| {
        RawInputEvent::Key(
            crate::input::TerminalKey::new(code, KeyModifiers::empty())
                .with_generated_text(Some(text.to_owned())),
        )
    };
    state.handle_raw_events(vec![host_text(" .m d\u{3000}", KeyCode::Char('.'))]);
    assert_eq!(viewer(&state).query.as_str(), "readme.md");
    let ime = state.handle_raw_events(vec![host_text("你好", KeyCode::Char(' '))]);
    assert_eq!(viewer(&state).query.as_str(), "readme.md你好");
    assert!(
        !any_pane_events(&ime),
        "an IME commit reported on the space key is text, not the keep action"
    );
    let ideographic = press(&mut state, KeyCode::Char('\u{3000}'), KeyModifiers::empty());
    assert_eq!(viewer(&state).query.as_str(), "readme.md你好");
    assert!(!any_pane_events(&ideographic));

    press(&mut state, KeyCode::Esc, KeyModifiers::empty());
    let tree_ime = state.handle_raw_events(vec![host_text("你好", KeyCode::Char(' '))]);
    assert!(!any_pane_events(&tree_ime));
}

#[test]
fn revealing_a_deep_directory_lists_one_ancestor_at_a_time() {
    let mut state = shell_state();
    let (list, index) = open(&mut state);
    answer(
        &mut state,
        &list,
        Ok(listing(
            "/repo",
            "/repo",
            vec![
                entry("a", DirectoryEntryKind::Dir),
                entry("README.md", DirectoryEntryKind::File),
            ],
        )),
    );
    answer(
        &mut state,
        &index.expect("index request"),
        Ok(index_of(
            "/repo",
            &["a/b/c/d/e/f/g/h/i/j/k.txt", "README.md"],
        )),
    );
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"abcdefghij");
    select(&mut state, "a/b/c/d/e/f/g/h/i/j/");

    let mut actions = state.handle_input_bytes(b"\r").actions;
    let mut path = "/repo".to_owned();
    for name in ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"] {
        path = format!("{path}/{name}");
        assert_eq!(
            listed_paths(&actions),
            [path.clone()],
            "one listing at a time"
        );
        let child = match name {
            "j" => entry("k.txt", DirectoryEntryKind::File),
            _ => {
                let next = char::from(name.as_bytes()[0] + 1);
                entry(&next.to_string(), DirectoryEntryKind::Dir)
            }
        };
        actions = answer_listing(&mut state, &actions, listing(&path, &path, vec![child]));
    }

    assert!(listed_paths(&actions).is_empty());
    assert_eq!(selected_label(&state), "j/");
    assert!(viewer(&state).pending_select.is_none());
}

#[test]
fn revealing_a_hidden_or_ignored_directory_turns_on_the_filter_that_hid_it() {
    let mut state = shell_state();
    let (list, index) = open(&mut state);
    let ResponseResult::DirectoryList { mut entries, .. } = repo_listing() else {
        unreachable!();
    };
    entries.insert(0, entry(".github", DirectoryEntryKind::Dir));
    answer(&mut state, &list, Ok(listing("/repo", "/repo", entries)));
    answer(
        &mut state,
        &index.expect("index request"),
        Ok(index_of(
            "/repo",
            &[".github/workflows/ci.yml", "README.md", "target/gen/out.rs"],
        )),
    );
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"workflows");
    select(&mut state, ".github/workflows/");

    let reveal = state.handle_input_bytes(b"\r");
    assert!(viewer(&state).show_hidden, "the dot directory is shown");
    let reveal = answer_listing(
        &mut state,
        &reveal.actions,
        listing(
            "/repo/.github",
            "/repo/.github",
            vec![entry("workflows", DirectoryEntryKind::Dir)],
        ),
    );
    answer_listing(
        &mut state,
        &reveal,
        listing(
            "/repo/.github/workflows",
            "/repo/.github/workflows",
            vec![entry("ci.yml", DirectoryEntryKind::File)],
        ),
    );
    assert_eq!(selected_label(&state), "workflows/");
    assert!(viewer(&state).pending_select.is_none());

    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"targetgen");
    select(&mut state, "target/gen/");
    let reveal = state.handle_input_bytes(b"\r");
    assert!(
        viewer(&state).show_ignored,
        "the ignored directory is shown"
    );
    let reveal = answer_listing(
        &mut state,
        &reveal.actions,
        listing(
            "/repo/target",
            "/repo/target",
            vec![ignored(entry("gen", DirectoryEntryKind::Dir))],
        ),
    );
    answer_listing(
        &mut state,
        &reveal,
        listing(
            "/repo/target/gen",
            "/repo/target/gen",
            vec![entry("out.rs", DirectoryEntryKind::File)],
        ),
    );
    assert_eq!(selected_label(&state), "gen/");
}

#[test]
fn a_reveal_that_cannot_reach_its_directory_stops_and_says_so() {
    let mut state = shell_state();
    open_repo(&mut state);
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"srcclient");
    select(&mut state, "src/client/");

    let reveal = state.handle_input_bytes(b"\r");
    let (id, _) = endpoint_requests(&reveal).remove(0);
    let after = answer(
        &mut state,
        &id,
        rejection("permission_denied", "permission denied: /repo/src"),
    );

    assert!(listed_paths(&after).is_empty());
    assert!(viewer(&state).pending_select.is_none());
    assert_eq!(rows(&state)[1], "  [permission denied]");
    assert_eq!(
        viewer(&state).notice.as_deref(),
        Some("cannot reveal /repo/src/client")
    );
}

#[test]
fn a_directory_result_outside_the_root_widens_the_root_to_the_repository() {
    let mut state = shell_state();
    let (list, index) = open(&mut state);
    answer(
        &mut state,
        &list,
        Ok(rooted_listing(
            "/repo/src",
            "/repo/src",
            "/repo/src",
            vec![entry("main.rs", DirectoryEntryKind::File)],
        )),
    );
    answer(
        &mut state,
        &index.expect("index request"),
        Ok(index_of("/repo", &["docs/guide.md", "src/main.rs"])),
    );
    state.handle_input_bytes(b"/");
    state.handle_input_bytes(b"docs");
    select(&mut state, "docs/");

    let reveal = state.handle_input_bytes(b"\r");

    let requests = endpoint_requests(&reveal);
    assert!(
        requests.iter().any(|(_, method)| matches!(
            method,
            Method::DirectoryIndex(DirectoryIndexParams { path: Some(path), .. }) if path == "/repo"
        )),
        "the widened root is indexed again: {requests:?}"
    );
    assert_eq!(listed_paths(&reveal.actions), ["/repo"]);
    let docs = answer_listing(
        &mut state,
        &reveal.actions,
        listing(
            "/repo",
            "/repo",
            vec![
                entry("docs", DirectoryEntryKind::Dir),
                entry("src", DirectoryEntryKind::Dir),
            ],
        ),
    );
    assert_eq!(listed_paths(&docs), ["/repo/docs"]);
    answer_listing(
        &mut state,
        &docs,
        listing(
            "/repo/docs",
            "/repo/docs",
            vec![entry("guide.md", DirectoryEntryKind::File)],
        ),
    );

    assert_eq!(rows(&state), ["docs/", "  guide.md", "src/"]);
    assert_eq!(selected_label(&state), "docs/");
    assert!(frame_text(&mut state).contains("files · /repo "));
}
