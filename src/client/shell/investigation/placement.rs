//! Where the files workbench is drawn: beside the terminals (split) or over
//! them (full). Client-only; the server only sees a smaller pane surface.
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

/// Whether the files region shares the pane surface with the terminals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(in crate::client::shell) enum FilesPresentation {
    #[default]
    Split,
    Full,
}

/// Which edge of the pane surface the files region is docked to in split.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(in crate::client::shell) enum FilesDock {
    #[default]
    Left,
    Right,
    Bottom,
}

/// Fewest terminal columns kept beside a left/right files region (Q17a).
pub(in crate::client::shell) const MIN_TERMINAL_COLS: u16 = 40;
/// Fewest files columns in a left/right files region (Q17a).
pub(in crate::client::shell) const MIN_FILES_COLS: u16 = 24;
/// Fewest terminal rows kept above a bottom files region.
pub(in crate::client::shell) const MIN_TERMINAL_ROWS: u16 = 8;
/// Fewest files rows in a bottom files region.
pub(in crate::client::shell) const MIN_FILES_ROWS: u16 = 6;
/// Extra room needed to leave the narrow fallback. Why not return at the
/// minimum: a resize hovering at the threshold would flip the terminal
/// size, and with it every PTY, on each step.
pub(in crate::client::shell) const RETURN_MARGIN_COLS: u16 = 6;
pub(in crate::client::shell) const RETURN_MARGIN_ROWS: u16 = 2;
const DEFAULT_SIDE_PERMILLE: u16 = 300;
const DEFAULT_BOTTOM_PERMILLE: u16 = 400;

/// The pane surface divided between terminals, divider and files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::client::shell) struct FilesCarve {
    pub terminal: Rect,
    pub divider: Rect,
    pub files: Rect,
}

#[derive(Debug)]
pub(in crate::client::shell) struct FilesPlacement {
    pub presentation: FilesPresentation,
    pub dock: FilesDock,
    /// Files share of the width for a left/right dock, in permille.
    /// Why one per axis: moving the dock to the bottom must not reuse a
    /// width chosen for columns as a share of rows.
    pub side_permille: u16,
    /// Files share of the height for a bottom dock, in permille.
    pub bottom_permille: u16,
    /// The last composed frame fell back to full screen for lack of room.
    /// Why refreshed only by `refresh_narrow` from compose rather than by
    /// `carve`: `layout()` also answers surface-size queries for other
    /// sizes, and those must not move the hysteresis between frames.
    narrow: bool,
}

impl Default for FilesPlacement {
    fn default() -> Self {
        Self {
            presentation: FilesPresentation::default(),
            dock: FilesDock::default(),
            side_permille: DEFAULT_SIDE_PERMILLE,
            bottom_permille: DEFAULT_BOTTOM_PERMILLE,
            narrow: false,
        }
    }
}

/// The placement as saved: per workspace in each of its saved
/// investigations (Q8), and once per preferences file as the placement a
/// workspace without investigations starts from. Why every tab of a
/// workspace carries a copy rather than one entry per workspace: the saved
/// entries are per tab, and a workspace keeps its placement as long as any
/// of them is left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::client::shell) struct SavedFilesPlacement {
    #[serde(default, deserialize_with = "lenient")]
    pub presentation: FilesPresentation,
    #[serde(default, deserialize_with = "lenient")]
    pub dock: FilesDock,
    #[serde(default, deserialize_with = "lenient")]
    pub side_permille: Option<u16>,
    #[serde(default, deserialize_with = "lenient")]
    pub bottom_permille: Option<u16>,
}

/// Reads a value or falls back to its default. Why not a plain field: one
/// unknown value written by a newer client would fail the whole preferences
/// file, losing the saved investigations and sidebar with it.
pub(in crate::client::shell) fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// The extent along the dock axis and its minimums:
/// (extent, min terminal, min files, return margin).
fn axis(dock: FilesDock, surface: Rect) -> (u16, u16, u16, u16) {
    match dock {
        FilesDock::Left | FilesDock::Right => (
            surface.width,
            MIN_TERMINAL_COLS,
            MIN_FILES_COLS,
            RETURN_MARGIN_COLS,
        ),
        FilesDock::Bottom => (
            surface.height,
            MIN_TERMINAL_ROWS,
            MIN_FILES_ROWS,
            RETURN_MARGIN_ROWS,
        ),
    }
}

impl FilesPlacement {
    pub fn permille(&self) -> u16 {
        match self.dock {
            FilesDock::Left | FilesDock::Right => self.side_permille,
            FilesDock::Bottom => self.bottom_permille,
        }
    }
    fn set_permille(&mut self, permille: u16) -> bool {
        let slot = match self.dock {
            FilesDock::Left | FilesDock::Right => &mut self.side_permille,
            FilesDock::Bottom => &mut self.bottom_permille,
        };
        let changed = *slot != permille;
        *slot = permille;
        changed
    }
    /// The placement to keep, or `None` while it is the default.
    pub fn save(&self) -> Option<SavedFilesPlacement> {
        let defaults = Self::default();
        let saved = SavedFilesPlacement {
            presentation: self.presentation,
            dock: self.dock,
            side_permille: (self.side_permille != defaults.side_permille)
                .then_some(self.side_permille),
            bottom_permille: (self.bottom_permille != defaults.bottom_permille)
                .then_some(self.bottom_permille),
        };
        (saved != SavedFilesPlacement::default()).then_some(saved)
    }
    /// The placement a saved one describes; ratios past 100% are capped.
    pub fn restore(saved: SavedFilesPlacement) -> Self {
        let defaults = Self::default();
        Self {
            presentation: saved.presentation,
            dock: saved.dock,
            side_permille: saved
                .side_permille
                .map_or(defaults.side_permille, |p| p.min(1000)),
            bottom_permille: saved
                .bottom_permille
                .map_or(defaults.bottom_permille, |p| p.min(1000)),
            narrow: false,
        }
    }
    /// Forgets an earlier fallback, so a split chosen now is shown whenever
    /// the minimums fit.
    pub fn reset_narrow(&mut self) {
        self.narrow = false;
    }
    /// Whether the last composed frame fell back to full screen.
    #[cfg(test)]
    pub fn narrow(&self) -> bool {
        self.narrow
    }
    /// Records whether `surface` (the whole pane surface of the frame being
    /// composed) leaves room for both sides, for the next carve's
    /// hysteresis. Called whether or not the workbench is shown, so a window
    /// widened while it was hidden does not demand the return margin.
    pub fn refresh_narrow(&mut self, surface: Rect) {
        self.narrow = !self.fits(surface);
    }
    fn fits(&self, surface: Rect) -> bool {
        let (extent, min_terminal, min_files, margin) = axis(self.dock, surface);
        let available = extent.saturating_sub(1);
        let needed = min_terminal.saturating_add(min_files);
        if self.narrow {
            available >= needed.saturating_add(margin)
        } else {
            available >= needed
        }
    }
    /// Splits `surface` for the dock, or `None` when it is too small for both
    /// minimums.
    pub fn carve(&self, surface: Rect) -> Option<FilesCarve> {
        if !self.fits(surface) {
            return None;
        }
        let (extent, min_terminal, min_files, _) = axis(self.dock, surface);
        let available = extent.saturating_sub(1);
        let share = (u32::from(available) * u32::from(self.permille()) + 500) / 1000;
        let files = u16::try_from(share)
            .unwrap_or(u16::MAX)
            .clamp(min_files, available - min_terminal);
        let terminal = available - files;
        let Rect {
            x,
            y,
            width,
            height,
        } = surface;
        Some(match self.dock {
            FilesDock::Left => FilesCarve {
                files: Rect::new(x, y, files, height),
                divider: Rect::new(x + files, y, 1, height),
                terminal: Rect::new(x + files + 1, y, terminal, height),
            },
            FilesDock::Right => FilesCarve {
                terminal: Rect::new(x, y, terminal, height),
                divider: Rect::new(x + terminal, y, 1, height),
                files: Rect::new(x + terminal + 1, y, files, height),
            },
            FilesDock::Bottom => FilesCarve {
                terminal: Rect::new(x, y, width, terminal),
                divider: Rect::new(x, y + terminal, width, 1),
                files: Rect::new(x, y + terminal + 1, width, files),
            },
        })
    }
    /// Moves the divider to the pointer cell inside `surface` (the whole pane
    /// surface before the carve), keeping both minimums. Returns whether the
    /// ratio changed.
    pub fn drag_to(&mut self, surface: Rect, column: u16, row: u16) -> bool {
        let (extent, min_terminal, min_files, _) = axis(self.dock, surface);
        let available = extent.saturating_sub(1);
        if available < min_terminal.saturating_add(min_files) {
            return false;
        }
        let files = match self.dock {
            FilesDock::Left => column.saturating_sub(surface.x),
            FilesDock::Right => surface.right().saturating_sub(1).saturating_sub(column),
            FilesDock::Bottom => surface.bottom().saturating_sub(1).saturating_sub(row),
        }
        .clamp(min_files, available - min_terminal);
        // Why rounded up: `carve` rounds to nearest, and a floor would let the
        // divider creep one cell away from the pointer on each event.
        let permille = (u32::from(files) * 1000).div_ceil(u32::from(available));
        self.set_permille(u16::try_from(permille).unwrap_or(1000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_placement_round_trips_and_the_default_is_not_saved() {
        assert_eq!(FilesPlacement::default().save(), None);
        let placement = FilesPlacement {
            presentation: FilesPresentation::Full,
            dock: FilesDock::Bottom,
            bottom_permille: 550,
            ..Default::default()
        };
        let json = serde_json::to_string(&placement.save()).unwrap();
        let saved: SavedFilesPlacement = serde_json::from_str(&json).unwrap();
        let restored = FilesPlacement::restore(saved);
        assert_eq!(restored.presentation, FilesPresentation::Full);
        assert_eq!(restored.dock, FilesDock::Bottom);
        assert_eq!(restored.bottom_permille, 550);
        assert_eq!(restored.side_permille, DEFAULT_SIDE_PERMILLE);
    }

    #[test]
    fn unknown_saved_values_fall_back_to_defaults() {
        let saved: SavedFilesPlacement = serde_json::from_str(
            r#"{"presentation":"floating","dock":"top","side_permille":-3,"bottom_permille":4000}"#,
        )
        .unwrap();
        let restored = FilesPlacement::restore(saved);
        assert_eq!(restored.presentation, FilesPresentation::Split);
        assert_eq!(restored.dock, FilesDock::Left);
        assert_eq!(restored.side_permille, DEFAULT_SIDE_PERMILLE);
        assert_eq!(restored.bottom_permille, 1000);
    }

    #[test]
    fn left_dock_puts_files_left_of_a_one_cell_divider() {
        let placement = FilesPlacement::default();
        let carve = placement.carve(Rect::new(20, 1, 101, 30)).unwrap();
        assert_eq!(carve.files, Rect::new(20, 1, 30, 30));
        assert_eq!(carve.divider, Rect::new(50, 1, 1, 30));
        assert_eq!(carve.terminal, Rect::new(51, 1, 70, 30));
    }

    #[test]
    fn right_dock_puts_files_right_of_the_terminal() {
        let placement = FilesPlacement {
            dock: FilesDock::Right,
            ..Default::default()
        };
        let carve = placement.carve(Rect::new(0, 0, 101, 10)).unwrap();
        assert_eq!(carve.terminal, Rect::new(0, 0, 70, 10));
        assert_eq!(carve.divider, Rect::new(70, 0, 1, 10));
        assert_eq!(carve.files, Rect::new(71, 0, 30, 10));
    }

    #[test]
    fn bottom_dock_uses_the_row_ratio() {
        let placement = FilesPlacement {
            dock: FilesDock::Bottom,
            ..Default::default()
        };
        let carve = placement.carve(Rect::new(0, 1, 80, 31)).unwrap();
        assert_eq!(carve.terminal, Rect::new(0, 1, 80, 18));
        assert_eq!(carve.divider, Rect::new(0, 19, 80, 1));
        assert_eq!(carve.files, Rect::new(0, 20, 80, 12));
    }

    /// Carves `surface` the way a composed frame of that size would.
    fn compose_at(placement: &mut FilesPlacement, surface: Rect) -> Option<FilesCarve> {
        placement.refresh_narrow(surface);
        placement.carve(surface)
    }

    #[test]
    fn too_narrow_falls_back_until_minimum_plus_margin_fits() {
        let mut placement = FilesPlacement::default();
        let minimum = MIN_TERMINAL_COLS + MIN_FILES_COLS + 1;
        assert!(compose_at(&mut placement, Rect::new(0, 0, minimum, 10)).is_some());
        assert!(compose_at(&mut placement, Rect::new(0, 0, minimum - 1, 10)).is_none());
        assert!(placement.narrow());
        assert!(compose_at(&mut placement, Rect::new(0, 0, minimum, 10)).is_none());
        assert!(compose_at(
            &mut placement,
            Rect::new(0, 0, minimum + RETURN_MARGIN_COLS - 1, 10)
        )
        .is_none());
        assert!(compose_at(
            &mut placement,
            Rect::new(0, 0, minimum + RETURN_MARGIN_COLS, 10)
        )
        .is_some());
        assert!(!placement.narrow());
    }

    #[test]
    fn carving_another_size_leaves_the_fallback_decision_alone() {
        let mut placement = FilesPlacement::default();
        let minimum = MIN_TERMINAL_COLS + MIN_FILES_COLS + 1;
        compose_at(&mut placement, Rect::new(0, 0, minimum - 1, 10));
        assert!(placement.carve(Rect::new(0, 0, 200, 10)).is_some());
        assert!(placement.narrow());
        assert!(placement.carve(Rect::new(0, 0, minimum, 10)).is_none());
    }

    #[test]
    fn bottom_dock_falls_back_on_rows() {
        let mut placement = FilesPlacement {
            dock: FilesDock::Bottom,
            ..Default::default()
        };
        let minimum = MIN_TERMINAL_ROWS + MIN_FILES_ROWS + 1;
        assert!(compose_at(&mut placement, Rect::new(0, 0, 200, minimum - 1)).is_none());
        assert!(compose_at(
            &mut placement,
            Rect::new(0, 0, 200, minimum + RETURN_MARGIN_ROWS - 1)
        )
        .is_none());
        assert!(compose_at(
            &mut placement,
            Rect::new(0, 0, 200, minimum + RETURN_MARGIN_ROWS)
        )
        .is_some());
    }

    #[test]
    fn ratio_is_clamped_to_both_minimums() {
        let placement = FilesPlacement {
            side_permille: 1000,
            ..Default::default()
        };
        let carve = placement.carve(Rect::new(0, 0, 101, 5)).unwrap();
        assert_eq!(carve.terminal.width, MIN_TERMINAL_COLS);
        let placement = FilesPlacement {
            side_permille: 0,
            ..Default::default()
        };
        let carve = placement.carve(Rect::new(0, 0, 101, 5)).unwrap();
        assert_eq!(carve.files.width, MIN_FILES_COLS);
    }

    #[test]
    fn dragging_moves_the_divider_to_the_pointer_and_clamps() {
        let surface = Rect::new(10, 0, 101, 5);
        let mut placement = FilesPlacement::default();
        assert!(placement.drag_to(surface, 10 + 45, 2));
        assert_eq!(placement.carve(surface).unwrap().divider.x, 10 + 45);
        placement.drag_to(surface, 10 + 99, 2);
        assert_eq!(
            placement.carve(surface).unwrap().terminal.width,
            MIN_TERMINAL_COLS
        );
        placement.drag_to(surface, 10, 2);
        assert_eq!(
            placement.carve(surface).unwrap().files.width,
            MIN_FILES_COLS
        );
    }

    #[test]
    fn dragging_a_right_dock_counts_files_from_the_right_edge() {
        let surface = Rect::new(0, 0, 101, 5);
        let mut placement = FilesPlacement {
            dock: FilesDock::Right,
            ..Default::default()
        };
        placement.drag_to(surface, 60, 0);
        assert_eq!(placement.carve(surface).unwrap().divider.x, 60);
    }

    #[test]
    fn dragging_a_bottom_dock_keeps_the_side_ratio() {
        let surface = Rect::new(0, 0, 120, 41);
        let mut placement = FilesPlacement {
            dock: FilesDock::Bottom,
            ..Default::default()
        };
        placement.drag_to(surface, 0, 20);
        assert_eq!(placement.carve(surface).unwrap().divider.y, 20);
        assert_eq!(placement.side_permille, DEFAULT_SIDE_PERMILLE);
    }
}
