//! The files keys users can rebind by name in `ui.file_workbench_keys`, and
//! how a key is shown in the footer, the menus and the key list.
use super::*;
use crossterm::event::KeyModifiers as M;

type Combo = (KeyCode, M);

struct NamedKey {
    name: &'static str,
    /// The key a configured one replaces.
    alias: Combo,
    /// The key the router handles for it.
    target: Combo,
    /// Only on the list without a dialog: elsewhere the key types or moves
    /// in a menu.
    list_only: bool,
}

/// A key known by `name`, wherever the files have the keys.
const fn named(name: &'static str, key: Combo) -> NamedKey {
    NamedKey {
        name,
        alias: key,
        target: key,
        list_only: false,
    }
}

/// `alias` standing for `target` on the list.
const fn on_list(name: &'static str, alias: Combo, target: Combo) -> NamedKey {
    NamedKey {
        name,
        alias,
        target,
        list_only: true,
    }
}

/// A key known by `name` on the list.
const fn list_key(name: &'static str, key: Combo) -> NamedKey {
    on_list(name, key, key)
}

const fn bare(code: KeyCode) -> Combo {
    (code, M::empty())
}

const fn plain(c: char) -> Combo {
    bare(KeyCode::Char(c))
}

const fn shifted(c: char) -> Combo {
    (KeyCode::Char(c), M::SHIFT)
}

const fn ctrl(c: char) -> Combo {
    (KeyCode::Char(c), M::CONTROL)
}

const fn alt(c: char) -> Combo {
    (KeyCode::Char(c), M::ALT)
}

// Why `pin` and `read`: the names users configured when Enter pinned a
// preview and Space read one; they now open the editor and pager.
// Why the vim letters stand for the arrows: a configured key replaces only
// the letter, and the arrows keep working.
const NAMED_KEYS: &[NamedKey] = &[
    list_key("pin", bare(KeyCode::Enter)),
    list_key("read", plain(' ')),
    on_list("down", plain('j'), bare(KeyCode::Down)),
    on_list("up", plain('k'), bare(KeyCode::Up)),
    on_list("collapse", plain('h'), bare(KeyCode::Left)),
    on_list("expand", plain('l'), bare(KeyCode::Right)),
    on_list("top", plain('g'), bare(KeyCode::Home)),
    on_list("bottom", shifted('G'), bare(KeyCode::End)),
    list_key("half_page_down", ctrl('d')),
    list_key("half_page_up", ctrl('u')),
    on_list("mark", plain('m'), bare(KeyCode::Insert)),
    list_key("details", shifted('K')),
    list_key("next_file", plain('n')),
    list_key("previous_file", shifted('N')),
    list_key("new_file", plain('a')),
    list_key("new_directory", shifted('A')),
    list_key("copy_path", plain('y')),
    list_key("send_path", plain('t')),
    list_key("change_root", shifted('C')),
    named("find_files", ctrl('p')),
    named("search", (KeyCode::Char('f'), M::CONTROL.union(M::SHIFT))),
    named("replace", ctrl('h')),
    named("toggle_regex", alt('r')),
    named("toggle_case", alt('c')),
    named("toggle_whole_word", alt('w')),
    named("toggle_ignored", alt('i')),
    named("edit", ctrl('e')),
    named("copy", ctrl('c')),
    named("cut", ctrl('x')),
    named("paste", ctrl('v')),
    named("select_all", ctrl('a')),
    named("rename", bare(KeyCode::F(2))),
    // Why before `delete`: a Shift report also matches the bare key.
    named("delete_permanently", (KeyCode::Delete, M::SHIFT)),
    named("delete", bare(KeyCode::Delete)),
    named("actions", bare(KeyCode::F(10))),
    named("files_menu", (KeyCode::F(10), M::SHIFT)),
    named("refresh", bare(KeyCode::F(5))),
];

/// Menu items with a files key, by the name of that key.
const MENU_KEYS: &[(&str, &str)] = &[
    (EDIT_FILE, "pin"),
    (VIEW_FILE, "read"),
    (COPY_PATH, "copy_path"),
    (SEND_PATH, "send_path"),
    (COPY_FILES, "copy"),
    (CUT_FILES, "cut"),
    (PASTE_FILES, "paste"),
    (RENAME, "rename"),
    (NEW_FILE, "new_file"),
    (NEW_DIRECTORY, "new_directory"),
    (TRASH, "delete"),
    (DELETE, "delete_permanently"),
    (TOGGLE_DETAILS, "details"),
    (OPEN_AS_ROOT, "change_root"),
    (REFRESH_FILES, "refresh"),
    (TOGGLE_REGEX, "toggle_regex"),
    (TOGGLE_CASE, "toggle_case"),
    (TOGGLE_WHOLE_WORD, "toggle_whole_word"),
    (INCLUDE_IGNORED, "toggle_ignored"),
];

impl ClientShellState {
    /// The key as the router should see it: a configured key becomes its
    /// action's, the alias it replaced does nothing, and an unconfigured
    /// alias becomes its target.
    pub(super) fn investigation_key(
        &self,
        key: &crate::input::TerminalKey,
    ) -> crate::input::TerminalKey {
        let on_list = self
            .investigation()
            .is_some_and(|t| t.focus == Focus::Results && t.dialog.is_none());
        // Why: legacy terminals send a capital without Shift.
        let mut shifted_capital;
        let key = match key.code {
            KeyCode::Char(c) if c.is_ascii_uppercase() && !key.modifiers.contains(M::SHIFT) => {
                shifted_capital = key.clone();
                shifted_capital.modifiers |= M::SHIFT;
                &shifted_capital
            }
            _ => key,
        };
        let matches = |combo| crate::config::terminal_key_matches_combo(key, combo);
        let mut disabled = false;
        let mut aliased = None;
        for named in NAMED_KEYS {
            if named.list_only && !on_list {
                continue;
            }
            match self.config.file_workbench_keys.get(named.name) {
                Some(configured) => {
                    if crate::config::parse_key_combo(configured).is_some_and(matches) {
                        return with_combo(key, named.target);
                    }
                    if matches(named.alias) {
                        disabled = true;
                    }
                }
                // Why `top` passes `g` through: `gg` is two presses, which
                // the router counts.
                None if named.name != "top" && aliased.is_none() && matches(named.alias) => {
                    aliased = Some(named.target);
                }
                None => {}
            }
        }
        if disabled {
            let mut result = key.clone();
            result.code = KeyCode::Null;
            result.generated_text = None;
            return result;
        }
        aliased.map_or_else(|| key.clone(), |target| with_combo(key, target))
    }

    /// How the key named `name` reads, as configured or by default; `None`
    /// when a configured value turned it off without naming a usable key.
    pub(super) fn files_key(&self, name: &str) -> Option<String> {
        if let Some(configured) = self.config.file_workbench_keys.get(name) {
            return crate::config::parse_key_combo(configured).map(combo_label);
        }
        if name == "top" {
            return Some("gg".into());
        }
        NAMED_KEYS
            .iter()
            .find(|named| named.name == name)
            .map(|named| combo_label(named.alias))
    }

    fn files_key_label(&self, name: &str) -> String {
        self.files_key(name).unwrap_or_else(|| "unset".into())
    }

    /// The key shown beside a workbench menu item.
    pub(super) fn files_menu_hint(&self, target: MenuTarget, label: &str) -> Option<String> {
        // Why not in the files menu: there these items act on the root, and
        // their keys on the selection.
        if target == MenuTarget::Tree && [NEW_FILE, NEW_DIRECTORY, PASTE_FILES].contains(&label) {
            return None;
        }
        let keybinds = &self.config.keybinds.keybinds;
        let shell_key = |bindings: &crate::config::ActionKeybinds| bindings.label();
        match label {
            SHOW_FULL_SCREEN | SHOW_SPLIT => shell_key(&keybinds.zoom),
            DOCK_LEFT => shell_key(&keybinds.swap_pane_left),
            DOCK_RIGHT => shell_key(&keybinds.swap_pane_right),
            DOCK_BOTTOM => shell_key(&keybinds.swap_pane_down),
            CLOSE_INVESTIGATION => shell_key(&keybinds.close_tab),
            SHOW_IGNORED => Some("i".into()),
            _ => MENU_KEYS
                .iter()
                .find(|(item, _)| *item == label)
                .and_then(|(_, name)| self.files_key(name)),
        }
    }

    /// The footer's keys for where the tab's keys go, most used first.
    pub(super) fn files_footer_items(&self, tab: &Investigation) -> Vec<String> {
        let item = |name, what| self.files_key(name).map(|key| format!("{key} {what}"));
        let fixed = |text: &str| Some(text.to_owned());
        let items = match tab.focus {
            Focus::Paths => vec![fixed("Enter/Esc list"), fixed("Tab next")],
            Focus::Query | Focus::Include | Focus::Exclude | Focus::Replacement => {
                vec![
                    fixed(if tab.focus == Focus::Replacement {
                        "Enter preview"
                    } else {
                        "Enter search"
                    }),
                    item("toggle_regex", "regex"),
                    item("toggle_case", "case"),
                    item("toggle_whole_word", "word"),
                    fixed("Tab next"),
                ]
            }
            Focus::Results => self.files_list_footer_items(tab),
        };
        items.into_iter().flatten().collect()
    }

    fn files_list_footer_items(&self, tab: &Investigation) -> Vec<Option<String>> {
        let item = |name, what| self.files_key(name).map(|key| format!("{key} {what}"));
        let fixed = |text: &str| Some(text.to_owned());
        if !tab.content_search && !tab.selected_files.is_empty() {
            return vec![
                item("mark", "mark"),
                item("copy", "copy"),
                item("cut", "cut"),
                item("delete", "trash"),
                fixed("Esc clear"),
            ];
        }
        let mut items = vec![item("pin", "edit"), item("read", "view")];
        if let Some(clipboard) = &self.workbench.clipboard {
            items.push(self.files_key("paste").map(|key| {
                format!(
                    "{key} paste ({} {} items)",
                    if clipboard.cut { "cut" } else { "copy" },
                    clipboard.paths.len()
                )
            }));
        }
        if tab.content_search {
            items.extend([
                self.files_key("next_file")
                    .zip(self.files_key("previous_file"))
                    .map(|(next, previous)| format!("{next}/{previous} file")),
                item("replace", "replace"),
            ]);
        } else {
            items.extend([
                fixed("/ filter"),
                item("details", "details"),
                item("new_file", "new"),
                item("rename", "rename"),
                item("delete", "trash"),
            ]);
        }
        items.push(item("actions", "menu"));
        items
    }

    /// The key list, with the files keys first when opened from the list.
    pub(in crate::client::shell) fn help_overlay(&self, files_first: bool) -> ClientHelpOverlay {
        ClientHelpOverlay {
            files: self.files_key_list(),
            files_first,
            ..ClientHelpOverlay::default()
        }
    }

    /// Keeps an open key list in step with reloaded files keys.
    pub(in crate::client::shell) fn refresh_help_files(&mut self) {
        let files = self.files_key_list();
        if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
            help.files = files;
        }
    }

    /// Every files key with what it does, for the key list.
    pub(super) fn files_key_list(&self) -> Vec<(String, String)> {
        let key = |name| self.files_key_label(name);
        let pair = |first, second| format!("{} / {}", key(first), key(second));
        let keybinds = &self.config.keybinds.keybinds;
        let shell_key = |bindings: &crate::config::ActionKeybinds| {
            bindings.label().unwrap_or_else(|| "unset".into())
        };
        [
            (key("pin"), "edit file / expand folder"),
            (key("read"), "view file"),
            (pair("down", "up"), "move down / up"),
            (pair("collapse", "expand"), "collapse / expand"),
            (pair("top", "bottom"), "first / last row"),
            (
                pair("half_page_down", "half_page_up"),
                "half page down / up",
            ),
            ("PgDn / PgUp".into(), "page down / up"),
            (pair("next_file", "previous_file"), "next / previous file"),
            ("/".into(), "filter"),
            (key("find_files"), "find files"),
            (key("search"), "search contents"),
            (key("replace"), "replace"),
            (key("toggle_regex"), "regex"),
            (key("toggle_case"), "case sensitivity"),
            (key("toggle_whole_word"), "whole word"),
            (key("toggle_ignored"), "search ignored files"),
            (key("mark"), "mark"),
            (key("select_all"), "mark all"),
            (key("details"), "details"),
            (key("new_file"), "new file"),
            (key("new_directory"), "new directory"),
            (key("copy_path"), "copy path"),
            (key("send_path"), "send path to terminal"),
            (key("change_root"), "open as root"),
            ("Backspace".into(), "widen root to parent"),
            (key("edit"), "edit file"),
            (pair("copy", "cut"), "copy / cut files"),
            (key("paste"), "paste files"),
            (key("rename"), "rename"),
            (key("delete"), "trash"),
            (key("delete_permanently"), "permanently delete"),
            (key("refresh"), "refresh"),
            (". / i".into(), "show dotfiles / ignored"),
            (key("actions"), "actions"),
            (key("files_menu"), "files menu"),
            ("Tab".into(), "next field"),
            ("Esc".into(), "back"),
            (shell_key(&keybinds.zoom), "full screen / split"),
            (shell_key(&keybinds.swap_pane_left), "dock left"),
            (shell_key(&keybinds.swap_pane_down), "dock bottom"),
            (shell_key(&keybinds.swap_pane_right), "dock right"),
            (shell_key(&keybinds.resize_mode), "resize files region"),
        ]
        .into_iter()
        .map(|(key, label)| (key, label.to_owned()))
        .collect()
    }
}

fn with_combo(
    key: &crate::input::TerminalKey,
    (code, modifiers): Combo,
) -> crate::input::TerminalKey {
    let mut remapped = key.clone();
    remapped.code = code;
    remapped.modifiers = modifiers;
    // Why cleared: the router reads the text too, and the pressed key's text
    // would make it act as that key on terminals that send one.
    remapped.generated_text = None;
    remapped.shifted_codepoint = None;
    remapped
}

/// `Ctrl+D`, `Shift+Del`, `K` for Shift+k, `Space`.
pub(in crate::client::shell) fn combo_label((code, modifiers): Combo) -> String {
    let (code, modifiers) = match code {
        // Why: parsing turns shift+tab into BackTab.
        KeyCode::BackTab => (KeyCode::Tab, modifiers | M::SHIFT),
        _ => (code, modifiers),
    };
    let mut label = String::new();
    for (modifier, name) in [
        (M::CONTROL, "Ctrl+"),
        (M::ALT, "Alt+"),
        (M::SUPER, "Super+"),
        (M::HYPER, "Hyper+"),
        (M::META, "Meta+"),
    ] {
        if modifiers.contains(modifier) {
            label.push_str(name);
        }
    }
    let shift = modifiers.contains(M::SHIFT);
    match code {
        // Why a capital rather than Shift+: it is what the key types.
        KeyCode::Char(c) if c.is_ascii_alphabetic() && shift && modifiers == M::SHIFT => {
            label.push(c.to_ascii_uppercase());
        }
        KeyCode::Char(c) => {
            if shift {
                label.push_str("Shift+");
            }
            if c == ' ' {
                label.push_str("Space");
            } else if modifiers.intersects(M::CONTROL | M::ALT) {
                label.extend(c.to_uppercase());
            } else {
                label.push(c);
            }
        }
        other => {
            if shift {
                label.push_str("Shift+");
            }
            label.push_str(&match other {
                KeyCode::Enter => "Enter".into(),
                KeyCode::Esc => "Esc".into(),
                KeyCode::Tab => "Tab".into(),
                KeyCode::Backspace => "Backspace".into(),
                KeyCode::Delete => "Del".into(),
                KeyCode::Insert => "Ins".into(),
                KeyCode::Home => "Home".into(),
                KeyCode::End => "End".into(),
                KeyCode::PageUp => "PgUp".into(),
                KeyCode::PageDown => "PgDn".into(),
                KeyCode::Up => "↑".into(),
                KeyCode::Down => "↓".into(),
                KeyCode::Left => "←".into(),
                KeyCode::Right => "→".into(),
                KeyCode::F(n) => format!("F{n}"),
                other => format!("{other:?}"),
            });
        }
    }
    label
}
