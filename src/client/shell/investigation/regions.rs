//! The files region among the panes: the pane focus, zoom, swap and resize
//! keys treat a docked region as the pane on its edge, so files is reached,
//! left, placed and sized without a mouse.
use super::*;
use crate::input::KeybindAction as A;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Toward {
    Left,
    Right,
    Up,
    Down,
}

impl Toward {
    fn of(action: A) -> Option<Self> {
        Some(match action {
            A::FocusPaneLeft | A::SwapPaneLeft | A::ResizePaneLeft => Self::Left,
            A::FocusPaneRight | A::SwapPaneRight | A::ResizePaneRight => Self::Right,
            A::FocusPaneUp | A::SwapPaneUp | A::ResizePaneUp => Self::Up,
            A::FocusPaneDown | A::SwapPaneDown | A::ResizePaneDown => Self::Down,
            _ => return None,
        })
    }
    /// The edge the files region is docked to.
    fn of_dock(dock: FilesDock) -> Self {
        match dock {
            FilesDock::Left => Self::Left,
            FilesDock::Right => Self::Right,
            FilesDock::Bottom => Self::Down,
        }
    }
    fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }
}

impl ClientShellState {
    /// Handles a pane key that concerns the files region. False leaves it to
    /// the panes.
    pub(super) fn files_region_action(
        &mut self,
        action: A,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.investigation_active() {
            return false;
        }
        let files_focused = self.files_has_focus();
        // Why the layout keys wait for a dialog: placing the region through
        // the menu handler would close it, losing what was typed.
        let dialog_open = self.investigation().is_some_and(|tab| tab.dialog.is_some());
        let docked = self.docked_files_region().is_some();
        let dock = Toward::of_dock(self.workbench.placement.dock);
        let toward = Toward::of(action);
        match action {
            A::FocusPaneLeft | A::FocusPaneRight | A::FocusPaneUp | A::FocusPaneDown => {
                if files_focused {
                    // Why consumed in full screen: the panes are not on
                    // screen, and moving their focus there would go unseen.
                    if docked && toward == Some(dock.opposite()) {
                        self.show_terminal_side();
                        outcome.repaint = true;
                    }
                    return true;
                }
                // Why a zoomed tab is left to the server: its hidden panes
                // still lie beside the focused one there.
                if docked
                    && toward == Some(dock)
                    && !self.focused_tab_zoomed()
                    && !self.pane_beside_focused(dock)
                {
                    self.workbench.files_focused = true;
                    outcome.repaint = true;
                    return true;
                }
                false
            }
            A::Zoom
            | A::SwapPaneLeft
            | A::SwapPaneRight
            | A::SwapPaneUp
            | A::SwapPaneDown
            | A::ResizePaneLeft
            | A::ResizePaneRight
            | A::ResizePaneUp
            | A::ResizePaneDown
                if files_focused && dialog_open =>
            {
                true
            }
            A::Zoom if files_focused => {
                let label = if self.investigation_full_screen() {
                    SHOW_SPLIT
                } else {
                    SHOW_FULL_SCREEN
                };
                self.investigation_menu_action(label, outcome);
                true
            }
            A::SwapPaneLeft | A::SwapPaneRight | A::SwapPaneUp | A::SwapPaneDown
                if files_focused =>
            {
                let label = match toward {
                    Some(Toward::Left) => DOCK_LEFT,
                    Some(Toward::Right) => DOCK_RIGHT,
                    Some(Toward::Down) => DOCK_BOTTOM,
                    // Why nothing: the files region never docks to the top.
                    _ => return true,
                };
                self.investigation_menu_action(label, outcome);
                true
            }
            A::ResizePaneLeft | A::ResizePaneRight | A::ResizePaneUp | A::ResizePaneDown
                if files_focused =>
            {
                if let (Some(toward), Some(surface)) = (toward, self.docked_files_surface()) {
                    // The divider moves the way the key points.
                    let grow = toward == dock.opposite();
                    if (grow || toward == dock)
                        && self.workbench.placement.resize_step(surface, grow)
                    {
                        self.remember_files_placement();
                        self.persist_chrome_preferences(outcome);
                        outcome.repaint = true;
                        outcome.resize = true;
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn focused_tab_zoomed(&self) -> bool {
        self.snapshot.as_deref().is_some_and(|snapshot| {
            snapshot
                .tabs
                .iter()
                .any(|tab| Some(&tab.tab_id) == snapshot.focused_tab_id.as_ref() && tab.zoomed)
        })
    }

    fn docked_files_region(&self) -> Option<Rect> {
        let (cols, rows) = self.last_composed_size?;
        self.layout(cols, rows).files_region
    }

    /// The pane surface with the docked files region and divider included.
    fn docked_files_surface(&self) -> Option<Rect> {
        let (cols, rows) = self.last_composed_size?;
        let layout = self.layout(cols, rows);
        let files = layout.files_region?;
        Some(files.union(layout.files_divider).union(layout.pane_surface))
    }

    /// Whether a pane lies on the `toward` side of the focused one. Why from
    /// the last frame's pane rectangles: the snapshot has no geometry, and
    /// the frame is what the reader sees.
    fn pane_beside_focused(&self, toward: Toward) -> bool {
        let focused_id = self.focused_pane_id();
        let mut panes = self.hits.panes.iter().filter(|hit| !hit.popup);
        let Some(focused) = panes
            .clone()
            .find(|hit| Some(&hit.pane_id) == focused_id.as_ref())
            .map(|hit| hit.rect)
        else {
            return false;
        };
        let rows_overlap = |other: Rect| other.y < focused.bottom() && focused.y < other.bottom();
        let cols_overlap = |other: Rect| other.x < focused.right() && focused.x < other.right();
        panes.any(|hit| {
            let other = hit.rect;
            match toward {
                Toward::Left => other.right() <= focused.x && rows_overlap(other),
                Toward::Right => other.x >= focused.right() && rows_overlap(other),
                Toward::Up => other.bottom() <= focused.y && cols_overlap(other),
                Toward::Down => other.y >= focused.bottom() && cols_overlap(other),
            }
        })
    }
}
