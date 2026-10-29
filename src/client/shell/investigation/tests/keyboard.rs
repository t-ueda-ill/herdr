//! Files is usable from the keyboard alone: list movement with the arrows,
//! the herdr list keys and vim keys, and the files region joining the pane
//! focus, zoom, swap and resize keys (.local/prd/files-keyboard.md).
use super::*;
use crate::api::schema::{
    DirectoryEntry, DirectoryEntryKind, FileMatch, Method, PaneDirection, PaneFocusDirectionParams,
};
use crate::client::shell::file_viewer::model::{DirectoryListing, DirectoryState};
use crossterm::event::KeyModifiers as M;

const COLS: u16 = 160;
const ROWS: u16 = 32;

fn entry(name: &str, kind: DirectoryEntryKind) -> DirectoryEntry {
    DirectoryEntry {
        name: name.into(),
        kind,
        target_kind: None,
        size: Some(1),
        mtime: None,
        hidden: false,
        ignored: Some(false),
    }
}

fn listing(path: &str, entries: Vec<DirectoryEntry>) -> DirectoryState {
    DirectoryState::Loaded(DirectoryListing {
        canonical: path.into(),
        entries,
        truncated: false,
        omitted: None,
        unlistable: None,
        git_error: None,
    })
}

/// Full screen over one directory and forty files, keys on the list.
fn listed() -> ClientShellState {
    let mut state = state();
    let tab = state.investigation_mut().unwrap();
    let mut entries = vec![entry("src", DirectoryEntryKind::Dir)];
    entries.extend((0..40).map(|i| entry(&format!("f{i:02}.txt"), DirectoryEntryKind::File)));
    tab.browser
        .directories
        .insert("/repo".into(), listing("/repo", entries));
    tab.browser.directories.insert(
        "/repo/src".into(),
        listing("/repo/src", vec![entry("lib.rs", DirectoryEntryKind::File)]),
    );
    tab.browser.rebuild(false);
    tab.focus = Focus::Results;
    state.set_pane_surface(crate::client::shell::tests::surface());
    state.compose(COLS, ROWS).expect("composed");
    state
}

fn selected(state: &ClientShellState) -> usize {
    state.investigation().unwrap().browser.selected
}

fn select(state: &mut ClientShellState, index: usize) {
    state.investigation_mut().unwrap().browser.selected = index;
}

fn last_row(state: &ClientShellState) -> usize {
    state.investigation().unwrap().browser.rows.len() - 1
}

fn row_of(state: &ClientShellState, name: &str) -> usize {
    state
        .investigation()
        .unwrap()
        .browser
        .rows
        .iter()
        .position(|row| {
            row.entry
                .as_ref()
                .is_some_and(|e| e.path == format!("/repo/{name}"))
        })
        .expect("row listed")
}

fn char_key(state: &mut ClientShellState, c: char) -> ClientShellInput {
    let modifiers = if c.is_ascii_uppercase() {
        M::SHIFT
    } else {
        M::empty()
    };
    press(state, KeyCode::Char(c), modifiers)
}

fn prefixed(state: &mut ClientShellState, code: KeyCode, modifiers: M) -> ClientShellInput {
    press(state, KeyCode::Char('b'), M::CONTROL);
    press(state, code, modifiers)
}

fn list_height(state: &ClientShellState) -> usize {
    usize::from(state.investigation_hits.list.height)
}

#[test]
fn j_and_k_move_the_selection_like_down_and_up() {
    let mut state = listed();
    select(&mut state, 3);
    char_key(&mut state, 'j');
    char_key(&mut state, 'j');
    char_key(&mut state, 'k');
    assert_eq!(selected(&state), 4);
}

#[test]
fn l_expands_and_h_collapses_a_directory_like_the_arrows() {
    let mut state = listed();
    let src = row_of(&state, "src");
    select(&mut state, src);
    char_key(&mut state, 'l');
    assert!(state
        .investigation()
        .unwrap()
        .browser
        .expanded
        .contains("/repo/src"));
    char_key(&mut state, 'h');
    assert!(!state
        .investigation()
        .unwrap()
        .browser
        .expanded
        .contains("/repo/src"));
}

#[test]
fn gg_and_home_jump_to_the_first_row_and_shift_g_and_end_to_the_last() {
    let mut state = listed();
    select(&mut state, 10);
    char_key(&mut state, 'G');
    assert_eq!(selected(&state), last_row(&state));
    char_key(&mut state, 'g');
    char_key(&mut state, 'g');
    assert_eq!(selected(&state), 0);
    press(&mut state, KeyCode::End, M::empty());
    assert_eq!(selected(&state), last_row(&state));
    press(&mut state, KeyCode::Home, M::empty());
    assert_eq!(selected(&state), 0);
}

#[test]
fn an_unshifted_capital_g_from_legacy_terminals_also_jumps_to_the_last_row() {
    let mut state = listed();
    press(&mut state, KeyCode::Char('G'), M::empty());
    assert_eq!(selected(&state), last_row(&state));
}

#[test]
fn a_lone_g_followed_by_another_key_does_not_jump() {
    let mut state = listed();
    select(&mut state, 10);
    char_key(&mut state, 'g');
    char_key(&mut state, 'j');
    char_key(&mut state, 'g');
    assert_eq!(selected(&state), 11);
}

#[test]
fn a_click_or_a_prefix_key_between_two_gs_breaks_gg() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut state = listed();
    select(&mut state, 10);
    char_key(&mut state, 'g');
    let row = state.investigation_hits.rows[0].0;
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x,
        row: row.y,
        modifiers: M::empty(),
    })]);
    select(&mut state, 10);
    char_key(&mut state, 'g');
    assert_eq!(selected(&state), 10, "after a click");
    prefixed(&mut state, KeyCode::Char('b'), M::empty());
    char_key(&mut state, 'g');
    assert_eq!(selected(&state), 10, "after a prefix key");
}

#[test]
fn page_keys_move_by_the_visible_rows_and_ctrl_d_u_by_half() {
    let mut state = listed();
    let height = list_height(&state);
    assert!(height > 2 && height < 40, "fixture shows part of the list");
    select(&mut state, 0);
    press(&mut state, KeyCode::PageDown, M::empty());
    assert_eq!(selected(&state), height);
    press(&mut state, KeyCode::PageUp, M::empty());
    assert_eq!(selected(&state), 0);
    press(&mut state, KeyCode::Char('d'), M::CONTROL);
    assert_eq!(selected(&state), height / 2);
    press(&mut state, KeyCode::Char('u'), M::CONTROL);
    assert_eq!(selected(&state), 0);
}

#[test]
fn m_marks_and_unmarks_the_selected_file_like_insert() {
    let mut state = listed();
    let file = row_of(&state, "f05.txt");
    select(&mut state, file);
    char_key(&mut state, 'm');
    assert!(state
        .investigation()
        .unwrap()
        .selected_files
        .contains("/repo/f05.txt"));
    char_key(&mut state, 'm');
    assert!(state.investigation().unwrap().selected_files.is_empty());
}

#[test]
fn shift_k_shows_and_hides_the_details_band() {
    let mut state = listed();
    let shown = !state.workbench.hide_details;
    char_key(&mut state, 'K');
    assert_eq!(state.workbench.hide_details, shown);
    char_key(&mut state, 'K');
    assert_eq!(state.workbench.hide_details, !shown);
}

#[test]
fn shift_j_does_not_extend_the_selection() {
    let mut state = listed();
    select(&mut state, 3);
    char_key(&mut state, 'J');
    let tab = state.investigation().unwrap();
    assert!(tab.selected_files.is_empty());
    assert_eq!(tab.browser.selected, 3);
}

#[test]
fn letters_type_into_the_filter_field_instead_of_moving() {
    let mut state = listed();
    state.investigation_mut().unwrap().focus = Focus::Paths;
    for c in ['j', 'k', 'g', 'm', 'n', 'h', 'l'] {
        char_key(&mut state, c);
    }
    assert_eq!(
        state.investigation().unwrap().browser.query.as_str(),
        "jkgmnhl"
    );
}

fn found(path: &str, line: usize) -> FileMatch {
    FileMatch {
        path: path.into(),
        revision: "r".into(),
        start: 0,
        end: 1,
        line,
        text: "x".into(),
        matched: "x".into(),
        replacement: None,
        context: None,
    }
}

#[test]
fn n_and_shift_n_jump_between_files_of_the_content_search_results() {
    let mut state = listed();
    let tab = state.investigation_mut().unwrap();
    tab.content_search = true;
    tab.matches = vec![
        found("/repo/a", 1),
        found("/repo/a", 2),
        found("/repo/b", 1),
        found("/repo/b", 2),
        found("/repo/c", 1),
    ];
    tab.selected_match = 0;
    char_key(&mut state, 'n');
    assert_eq!(state.investigation().unwrap().selected_match, 2);
    char_key(&mut state, 'n');
    assert_eq!(state.investigation().unwrap().selected_match, 4);
    char_key(&mut state, 'n');
    assert_eq!(state.investigation().unwrap().selected_match, 4);
    state.investigation_mut().unwrap().selected_match = 3;
    char_key(&mut state, 'N');
    assert_eq!(state.investigation().unwrap().selected_match, 0);
}

#[test]
fn n_leaves_the_tree_selection_alone() {
    let mut state = listed();
    select(&mut state, 3);
    char_key(&mut state, 'n');
    assert_eq!(selected(&state), 3);
}

#[test]
fn a_configured_key_replaces_only_the_vim_alias_of_an_action() {
    let mut state = listed();
    state
        .config
        .file_workbench_keys
        .insert("down".into(), "ctrl+n".into());
    select(&mut state, 3);
    press(&mut state, KeyCode::Char('n'), M::CONTROL);
    assert_eq!(selected(&state), 4);
    char_key(&mut state, 'j');
    assert_eq!(selected(&state), 4, "j no longer moves");
    press(&mut state, KeyCode::Down, M::empty());
    assert_eq!(selected(&state), 5, "the arrow stays");
    state.investigation_mut().unwrap().focus = Focus::Paths;
    char_key(&mut state, 'j');
    assert_eq!(state.investigation().unwrap().browser.query.as_str(), "j");
}

#[test]
fn a_configured_top_key_replaces_gg_and_keeps_home() {
    let mut state = listed();
    state
        .config
        .file_workbench_keys
        .insert("top".into(), "ctrl+t".into());
    select(&mut state, 10);
    char_key(&mut state, 'g');
    char_key(&mut state, 'g');
    assert_eq!(selected(&state), 10);
    press(&mut state, KeyCode::Char('t'), M::CONTROL);
    assert_eq!(selected(&state), 0);
    select(&mut state, 10);
    press(&mut state, KeyCode::Home, M::empty());
    assert_eq!(selected(&state), 0);
}

#[test]
fn the_files_menu_moves_with_j_and_k() {
    let mut state = listed();
    press(&mut state, KeyCode::F(10), M::empty());
    char_key(&mut state, 'j');
    char_key(&mut state, 'j');
    char_key(&mut state, 'k');
    match &state.investigation().unwrap().dialog {
        Some(Dialog::Menu { selected, .. }) => assert_eq!(*selected, 1),
        other => panic!("menu still open, got {other:?}"),
    }
}

mod regions {
    use super::*;

    /// Split, docked left, beside the one-pane test surface, files focused.
    fn split_files_focused(dock: FilesDock) -> ClientShellState {
        let mut state = listed();
        state.workbench.placement.presentation = FilesPresentation::Split;
        state.workbench.placement.dock = dock;
        state.remember_files_placement();
        state.show_files(0);
        state.compose(COLS, ROWS).expect("composed split");
        assert!(state.layout(COLS, ROWS).files_region.is_some());
        state
    }

    fn split_terminal_focused(dock: FilesDock) -> ClientShellState {
        let mut state = split_files_focused(dock);
        state.show_terminal_side();
        state.compose(COLS, ROWS).expect("composed split");
        state
    }

    fn focus_requests(outcome: &ClientShellInput) -> Vec<PaneDirection> {
        sent_methods(outcome)
            .into_iter()
            .filter_map(|method| match method {
                Method::PaneFocusDirection(PaneFocusDirectionParams { direction, .. }) => {
                    Some(direction)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn prefix_f_opens_files_with_the_default_keys() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
        prefixed(&mut state, KeyCode::Char('f'), M::empty());
        assert_eq!(state.workbench.tabs.len(), 1);
        assert!(state.files_has_focus());
    }

    #[test]
    fn prefix_toward_the_dock_from_the_edge_pane_enters_the_files_region() {
        for (dock, code) in [
            (FilesDock::Left, KeyCode::Char('h')),
            (FilesDock::Right, KeyCode::Char('l')),
            (FilesDock::Bottom, KeyCode::Char('j')),
        ] {
            let mut state = split_terminal_focused(dock);
            let outcome = prefixed(&mut state, code, M::empty());
            assert!(state.files_has_focus(), "{dock:?}");
            assert!(focus_requests(&outcome).is_empty(), "{dock:?}");
        }
    }

    #[test]
    fn prefix_away_from_the_dock_returns_to_the_terminal() {
        for (dock, code) in [
            (FilesDock::Left, KeyCode::Char('l')),
            (FilesDock::Right, KeyCode::Char('h')),
            (FilesDock::Bottom, KeyCode::Char('k')),
        ] {
            let mut state = split_files_focused(dock);
            let outcome = prefixed(&mut state, code, M::empty());
            assert!(!state.files_has_focus(), "{dock:?}");
            assert!(focus_requests(&outcome).is_empty(), "{dock:?}");
        }
    }

    #[test]
    fn prefix_along_the_dock_keeps_the_files_focused() {
        let mut state = split_files_focused(FilesDock::Left);
        let outcome = prefixed(&mut state, KeyCode::Char('j'), M::empty());
        assert!(state.files_has_focus());
        assert!(focus_requests(&outcome).is_empty());
    }

    #[test]
    fn prefix_toward_the_dock_with_a_pane_in_between_moves_between_panes() {
        let mut state = split_terminal_focused(FilesDock::Left);
        let mut surface = crate::client::shell::tests::surface();
        let mut left = surface.panes[0].clone();
        left.pane_id = "pane_2".into();
        left.focused = false;
        surface.panes[0].rect.x = 5;
        surface.panes[0].inner_rect.x = 5;
        surface.panes.push(left);
        state.set_pane_surface(surface);
        state.compose(COLS, ROWS).expect("composed split");
        let outcome = prefixed(&mut state, KeyCode::Char('h'), M::empty());
        assert!(!state.files_has_focus());
        assert_eq!(focus_requests(&outcome), vec![PaneDirection::Left]);
    }

    #[test]
    fn full_screen_files_ignore_the_pane_focus_keys() {
        let mut state = listed();
        for code in ['h', 'j', 'k', 'l'].map(KeyCode::Char) {
            let outcome = prefixed(&mut state, code, M::empty());
            assert!(state.files_has_focus(), "{code:?}");
            assert!(focus_requests(&outcome).is_empty(), "{code:?}");
        }
    }

    #[test]
    fn a_zoomed_tab_leaves_pane_focus_keys_to_the_server() {
        let mut state = split_terminal_focused(FilesDock::Left);
        let mut snapshot = crate::client::shell::tests::snapshot();
        snapshot.tabs[0].zoomed = true;
        state.set_snapshot(Box::new(snapshot));
        let outcome = prefixed(&mut state, KeyCode::Char('h'), M::empty());
        assert!(!state.files_has_focus());
        assert_eq!(focus_requests(&outcome), vec![PaneDirection::Left]);
    }

    #[test]
    fn navigate_mode_moves_between_the_files_region_and_the_terminal() {
        let mut state = split_files_focused(FilesDock::Left);
        prefixed(&mut state, KeyCode::Char('w'), M::empty());
        assert_eq!(state.mode, ClientShellMode::Navigate);
        char_key(&mut state, 'l');
        assert!(!state.files_has_focus());
        char_key(&mut state, 'h');
        assert!(state.files_has_focus());
    }

    #[test]
    fn resize_mode_resizes_the_files_region_along_its_dock() {
        let mut state = split_files_focused(FilesDock::Left);
        let before = state.workbench.placement.side_permille;
        prefixed(&mut state, KeyCode::Char('r'), M::empty());
        assert_eq!(state.mode, ClientShellMode::Resize);
        let outcome = char_key(&mut state, 'l');
        assert!(sent_methods(&outcome).is_empty());
        let grown = state.workbench.placement.side_permille;
        assert!(grown > before, "{grown} > {before}");
        char_key(&mut state, 'j');
        assert_eq!(state.workbench.placement.side_permille, grown);
        char_key(&mut state, 'h');
        char_key(&mut state, 'h');
        assert!(state.workbench.placement.side_permille < before);
        press(&mut state, KeyCode::Esc, M::empty());
        assert_eq!(state.mode, ClientShellMode::Terminal);
        assert_eq!(
            state
                .investigation()
                .unwrap()
                .placement
                .and_then(|p| p.side_permille),
            Some(state.workbench.placement.side_permille),
            "kept with the workspace's tabs"
        );
    }

    #[test]
    fn resize_mode_stops_at_the_minimum_sizes() {
        let mut state = split_files_focused(FilesDock::Bottom);
        prefixed(&mut state, KeyCode::Char('r'), M::empty());
        for _ in 0..40 {
            char_key(&mut state, 'k');
        }
        state.compose(COLS, ROWS).expect("composed split");
        let layout = state.layout(COLS, ROWS);
        assert_eq!(layout.pane_surface.height, placement::MIN_TERMINAL_ROWS);
        for _ in 0..40 {
            char_key(&mut state, 'j');
        }
        let layout = state.layout(COLS, ROWS);
        assert_eq!(
            layout.files_region.map(|r| r.height),
            Some(placement::MIN_FILES_ROWS)
        );
    }

    #[test]
    fn resize_mode_at_a_minimum_keeps_the_saved_share() {
        let mut state = split_files_focused(FilesDock::Left);
        state.workbench.placement.side_permille = 10;
        state.remember_files_placement();
        prefixed(&mut state, KeyCode::Char('r'), M::empty());
        char_key(&mut state, 'h');
        assert_eq!(state.workbench.placement.side_permille, 10);
    }

    #[test]
    fn layout_keys_keep_an_open_dialog() {
        let mut state = split_files_focused(FilesDock::Left);
        press(&mut state, KeyCode::F(2), M::empty());
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Input { .. })
        ));
        prefixed(&mut state, KeyCode::Char('z'), M::empty());
        prefixed(&mut state, KeyCode::Char('L'), M::SHIFT);
        prefixed(&mut state, KeyCode::Char('r'), M::empty());
        char_key(&mut state, 'l');
        press(&mut state, KeyCode::Esc, M::empty());
        assert!(matches!(
            state.investigation().unwrap().dialog,
            Some(Dialog::Input { .. })
        ));
    }

    #[test]
    fn resize_mode_beside_a_focused_terminal_still_resizes_panes() {
        let mut state = split_terminal_focused(FilesDock::Left);
        prefixed(&mut state, KeyCode::Char('r'), M::empty());
        let outcome = char_key(&mut state, 'l');
        assert!(sent_methods(&outcome)
            .iter()
            .any(|m| matches!(m, Method::PaneResize(_))));
    }

    #[test]
    fn prefix_z_switches_focused_files_between_split_and_full_screen() {
        let mut state = split_files_focused(FilesDock::Left);
        let outcome = prefixed(&mut state, KeyCode::Char('z'), M::empty());
        assert!(sent_methods(&outcome).is_empty());
        assert_eq!(
            state.workbench.placement.presentation,
            FilesPresentation::Full
        );
        prefixed(&mut state, KeyCode::Char('z'), M::empty());
        assert_eq!(
            state.workbench.placement.presentation,
            FilesPresentation::Split
        );
    }

    #[test]
    fn prefix_z_beside_a_focused_terminal_still_zooms_the_pane() {
        let mut state = split_terminal_focused(FilesDock::Left);
        let outcome = prefixed(&mut state, KeyCode::Char('z'), M::empty());
        assert!(sent_methods(&outcome)
            .iter()
            .any(|m| matches!(m, Method::PaneZoom(_))));
        assert_eq!(
            state.workbench.placement.presentation,
            FilesPresentation::Split
        );
    }

    #[test]
    fn prefix_shift_h_j_l_move_the_dock_and_shift_k_does_nothing() {
        let mut state = split_files_focused(FilesDock::Left);
        prefixed(&mut state, KeyCode::Char('J'), M::SHIFT);
        assert_eq!(state.workbench.placement.dock, FilesDock::Bottom);
        prefixed(&mut state, KeyCode::Char('L'), M::SHIFT);
        assert_eq!(state.workbench.placement.dock, FilesDock::Right);
        let outcome = prefixed(&mut state, KeyCode::Char('K'), M::SHIFT);
        assert!(sent_methods(&outcome).is_empty());
        assert_eq!(state.workbench.placement.dock, FilesDock::Right);
        prefixed(&mut state, KeyCode::Char('H'), M::SHIFT);
        assert_eq!(state.workbench.placement.dock, FilesDock::Left);
        assert!(state.files_has_focus());
    }
}
