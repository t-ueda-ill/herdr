//! Chooser opened from the spaces " new" footer: where the new space runs.

use super::render::menu_widget::{MenuItem, MenuLook};
use super::*;

pub(super) const NEW_WORKSPACE_MENU_LOOK: MenuLook = MenuLook {
    min_width: 0,
    indent: 1,
};

/// Each row replays the binding a keyboard user would press, so the chooser
/// never drifts from the keybind paths.
const CHOICES: [(&str, crate::input::KeybindAction); 3] = [
    ("local", crate::input::KeybindAction::NewWorkspace),
    ("ssh host…", crate::input::KeybindAction::OpenSshWorkspace),
    (
        "container…",
        crate::input::KeybindAction::OpenContainerWorkspace,
    ),
];

impl ClientShellState {
    pub(super) fn open_new_workspace_menu(&mut self) {
        self.overlay = Some(ClientShellOverlay::NewWorkspaceMenu(
            ClientNewWorkspaceMenuOverlay {
                highlighted: None,
                scroll: 0,
            },
        ));
    }

    pub(super) fn new_workspace_menu_items(&self) -> Vec<MenuItem<'static>> {
        CHOICES
            .iter()
            .map(|(label, action)| MenuItem {
                enabled: OpenPickerSource::for_action(*action)
                    .is_none_or(|source| self.endpoint_opens_picker(source)),
                ..MenuItem::new(label)
            })
            .collect()
    }

    pub(super) fn activate_new_workspace_menu_item(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some((_, action)) = CHOICES.get(index) else {
            return;
        };
        self.overlay = None;
        self.record_binding(crate::input::KeybindMatch::Action(*action), outcome);
        outcome.repaint = true;
    }
}
