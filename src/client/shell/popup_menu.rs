//! Glue between the shell's open popup menu overlays and the shared widget,
//! so pointer and key handling exist once for every menu.

use super::render::menu_widget::{self, MenuItem, MenuKey, MenuPointer};
use super::*;
use crossterm::event::{MouseButton, MouseEventKind};

impl ClientShellState {
    /// Items of the open popup menu, or `None` when no popup menu is open.
    fn open_menu_items(&self) -> Option<Vec<MenuItem<'static>>> {
        match self.overlay.as_ref()? {
            ClientShellOverlay::ContextMenu(menu) => Some(menu.menu_items()),
            ClientShellOverlay::GlobalMenu(_) => Some(
                self.snapshot
                    .as_deref()
                    .map(super::global_menu::global_menu_entries)
                    .unwrap_or_default(),
            ),
            ClientShellOverlay::NewWorkspaceMenu(_) => Some(self.new_workspace_menu_items()),
            _ => None,
        }
    }

    fn open_menu_rows(&self) -> &[(Rect, usize)] {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::ContextMenu(_)) => &self.hits.context_menu_rows,
            Some(ClientShellOverlay::GlobalMenu(_)) => &self.hits.global_menu_rows,
            Some(ClientShellOverlay::NewWorkspaceMenu(_)) => &self.hits.new_workspace_menu_rows,
            _ => &[],
        }
    }

    /// Highlight of the open popup menu; the inner `None` is a menu that
    /// highlights nothing until a key or the pointer picks a row.
    fn open_menu_highlight(&self) -> Option<Option<usize>> {
        match self.overlay.as_ref()? {
            ClientShellOverlay::ContextMenu(menu) => Some(Some(menu.highlighted)),
            ClientShellOverlay::GlobalMenu(menu) => Some(Some(menu.highlighted)),
            ClientShellOverlay::NewWorkspaceMenu(menu) => Some(menu.highlighted),
            _ => None,
        }
    }

    fn set_open_menu_highlight(&mut self, index: usize) {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::ContextMenu(menu)) => menu.highlighted = index,
            Some(ClientShellOverlay::GlobalMenu(menu)) => menu.highlighted = index,
            Some(ClientShellOverlay::NewWorkspaceMenu(menu)) => menu.highlighted = Some(index),
            _ => {}
        }
    }

    fn open_menu_scroll_mut(&mut self) -> Option<&mut usize> {
        match self.overlay.as_mut()? {
            ClientShellOverlay::ContextMenu(menu) => Some(&mut menu.scroll),
            ClientShellOverlay::GlobalMenu(menu) => Some(&mut menu.scroll),
            ClientShellOverlay::NewWorkspaceMenu(menu) => Some(&mut menu.scroll),
            _ => None,
        }
    }

    /// Moves the open menu's window by one wheel notch.
    fn scroll_open_menu(&mut self, up: bool) -> bool {
        let visible = self.open_menu_rows().len();
        let Some(len) = self.open_menu_items().map(|items| items.len()) else {
            return false;
        };
        let Some(highlighted) = self.open_menu_highlight() else {
            return false;
        };
        let Some(scroll) = self.open_menu_scroll_mut() else {
            return false;
        };
        let (next, next_highlighted) =
            menu_widget::wheel_window(up, *scroll, highlighted, visible, len);
        if next == *scroll {
            return false;
        }
        *scroll = next;
        if let Some(index) = next_highlighted {
            self.set_open_menu_highlight(index);
        }
        true
    }

    fn activate_open_menu_item(&mut self, index: usize, outcome: &mut ClientShellInput) {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::ContextMenu(_)) => {
                self.activate_context_menu_item(index, outcome)
            }
            Some(ClientShellOverlay::GlobalMenu(_)) => {
                self.activate_global_menu_item(index, outcome)
            }
            Some(ClientShellOverlay::NewWorkspaceMenu(_)) => {
                self.activate_new_workspace_menu_item(index, outcome)
            }
            _ => {}
        }
    }

    /// Routes a mouse event to the open popup menu. Returns false when no
    /// popup menu is open; every event is swallowed while one is.
    pub(super) fn route_popup_menu_mouse(
        &mut self,
        kind: MouseEventKind,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(
            self.overlay,
            Some(
                ClientShellOverlay::ContextMenu(_)
                    | ClientShellOverlay::GlobalMenu(_)
                    | ClientShellOverlay::NewWorkspaceMenu(_)
            )
        ) {
            return false;
        }
        // Why not through the widget: the launcher toggles the menu it
        // anchors, and it lies outside every row.
        if matches!(self.overlay, Some(ClientShellOverlay::GlobalMenu(_)))
            && kind == MouseEventKind::Down(MouseButton::Left)
            && contains(self.hits.global_launcher, point)
        {
            self.toggle_global_menu();
            outcome.repaint = true;
            return true;
        }
        // Why not build the items up front: only a click needs the enabled
        // flag, and pointer moves arrive far more often.
        let pointer = menu_widget::menu_pointer(kind, self.open_menu_rows(), point, |index| {
            self.open_menu_items()
                .and_then(|items| items.get(index).map(|item| item.enabled))
                .unwrap_or(false)
        });
        match pointer {
            MenuPointer::Highlight(index) => {
                // Why not repaint on every move: pointer travel within one
                // row would redraw the frame per cell, as the workbench
                // menu already avoids.
                if self.open_menu_highlight() != Some(Some(index)) {
                    self.set_open_menu_highlight(index);
                    outcome.repaint = true;
                }
            }
            MenuPointer::Scroll { up } => {
                if self.scroll_open_menu(up) {
                    outcome.repaint = true;
                }
            }
            MenuPointer::Activate(index) => self.activate_open_menu_item(index, outcome),
            MenuPointer::Close => {
                self.overlay = None;
                outcome.repaint = true;
            }
            MenuPointer::Ignore => {}
        }
        true
    }

    /// Routes a key to the open popup menu. Returns false when no popup menu
    /// is open; every key is swallowed while one is.
    pub(super) fn route_popup_menu_key(
        &mut self,
        code: KeyCode,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(items) = self.open_menu_items() else {
            return false;
        };
        let Some(highlighted) = self.open_menu_highlight() else {
            return false;
        };
        // Why not j/k everywhere: the spaces menu never had them, and this
        // step keeps each menu's keys as they were.
        let vim_keys = matches!(self.overlay, Some(ClientShellOverlay::GlobalMenu(_)));
        match menu_widget::menu_key(code, highlighted, &items, vim_keys) {
            MenuKey::Move(index) => {
                self.set_open_menu_highlight(index);
                outcome.repaint = true;
            }
            MenuKey::Activate(index) => self.activate_open_menu_item(index, outcome),
            MenuKey::Close => {
                self.overlay = None;
                outcome.repaint = true;
            }
            MenuKey::Inert | MenuKey::Unhandled => {}
        }
        true
    }
}
