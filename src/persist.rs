//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.
//! Optional pane screen history is stored separately at `session-history.json`.
//! Installed plugins are persisted separately at `plugins.json`.

mod io;
pub mod plugin_registry;
mod restore;
mod snapshot;
pub(crate) mod tasks;
mod writer;

pub use self::io::{clear_history, load, load_history};
pub use self::restore::restore;
pub(crate) use self::restore::restored_worktree_space_membership;
#[cfg(unix)]
pub use self::restore::{handoff_pane_aliases, restore_handoff};
// Why not on Windows outside tests: session saves leave assists out, and only
// live handoff, which keeps every running pane, captures all of them.
#[cfg(any(unix, test))]
pub use self::snapshot::capture;
pub use self::snapshot::{
    capture_excluding, capture_history, DirectionSnapshot, LayoutSnapshot, SessionHistorySnapshot,
    SessionSnapshot, TabSnapshot, WorkspaceSnapshot,
};
// Why Unix-only: only live handoff, which passes PTY descriptors, uses it.
#[cfg(unix)]
pub use self::snapshot::capture_history_where;
pub(crate) use self::writer::SessionWriter;
