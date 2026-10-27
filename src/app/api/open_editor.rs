//! `pane.open_editor` and `pane.open_file`: the file viewer's "edit" and
//! "view" actions, run like `pane.edit_scrollback` but on a user file instead
//! of a scrollback dump.

use std::path::Path;

use crate::api::schema::{OpenFileMode, PaneOpenEditorParams, PaneOpenFileParams, ResponseResult};
use crate::app::App;

use super::directory::requested_path;
use super::responses::{encode_error, encode_success};

type EditorArgv = fn(&Path) -> std::io::Result<Vec<String>>;
type OpenArgv = fn(
    &Path,
    Option<std::num::NonZeroU32>,
    crate::api::schema::OpenFileMode,
) -> std::io::Result<Vec<String>>;

impl App {
    pub(super) fn handle_pane_open_editor(
        &mut self,
        id: String,
        params: PaneOpenEditorParams,
    ) -> String {
        self.handle_pane_open_editor_with(id, params, crate::platform::file_editor_argv)
    }

    fn handle_pane_open_editor_with(
        &mut self,
        id: String,
        params: PaneOpenEditorParams,
        editor_argv: EditorArgv,
    ) -> String {
        self.open_overlay_on_file(
            id,
            params.workspace_id.as_deref(),
            params.pane_id.as_deref(),
            &params.path,
            false,
            editor_argv,
        )
    }

    pub(super) fn handle_pane_open_file(
        &mut self,
        id: String,
        params: PaneOpenFileParams,
    ) -> String {
        self.handle_pane_open_file_with(id, params, crate::platform::file_open_argv)
    }

    fn handle_pane_open_file_with(
        &mut self,
        id: String,
        params: PaneOpenFileParams,
        open_argv: OpenArgv,
    ) -> String {
        let PaneOpenFileParams {
            workspace_id,
            pane_id,
            path,
            line,
            mode,
        } = params;
        self.open_overlay_on_file(
            id,
            workspace_id.as_deref(),
            pane_id.as_deref(),
            &path,
            mode == OpenFileMode::Pager,
            |path| open_argv(path, line, mode),
        )
    }

    /// `must_exist`: a pager has nothing to show for a missing file, while an
    /// editor creates it on save.
    fn open_overlay_on_file(
        &mut self,
        id: String,
        workspace_id: Option<&str>,
        pane_id: Option<&str>,
        path: &str,
        must_exist: bool,
        argv: impl FnOnce(&Path) -> std::io::Result<Vec<String>>,
    ) -> String {
        let path = match requested_path(path) {
            Ok(path) => path,
            Err((code, message)) => return encode_error(id, code, message),
        };
        if path.is_dir() {
            return encode_error(
                id,
                "invalid_params",
                format!("path is a directory: {}", path.display()),
            );
        }
        if must_exist && !path.exists() {
            return encode_error(id, "not_found", format!("not found: {}", path.display()));
        }
        let (ws_idx, pane_id) = match self.directory_target(workspace_id, pane_id) {
            Ok((Some(ws_idx), Some(pane_id))) => (ws_idx, pane_id),
            Ok(_) => return encode_error(id, "pane_not_found", "no focused pane"),
            Err((code, message)) => return encode_error(id, code, message),
        };
        // The overlay always opens over the active workspace's focused pane, so
        // a target that lost focus would get an editor somewhere else.
        let is_focused = self.state.active == Some(ws_idx)
            && self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(crate::workspace::Workspace::focused_pane_id)
                == Some(pane_id);
        if !is_focused {
            return encode_error(id, "stale_pane_target", "pane is no longer focused");
        }
        // Why not open it anyway: the editor would cover a pane that cannot
        // run anything, and the user asked to edit from a working shell.
        if !self.pane_is_live(ws_idx, pane_id) {
            return encode_error(id, "pane_not_live", "the focused pane has no running shell");
        }
        let argv = match argv(&path) {
            Ok(argv) => argv,
            Err(err) if err.kind() == std::io::ErrorKind::Unsupported => {
                return encode_error(id, "platform_unsupported", err.to_string());
            }
            Err(err) => return encode_error(id, "editor_failed", err.to_string()),
        };
        let pane = match self.spawn_editor_overlay(&argv, None) {
            Ok(pane) => pane,
            Err((code, message)) => return encode_error(id, code, message),
        };
        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    /// Opens `argv` as a zoomed overlay pane over the focused pane and
    /// announces it. Shared by the host editor and the off-host editor, which
    /// differ in the argv and in where the host process starts: `None` takes
    /// the focused pane's directory, which for an off-host pane is a path on the
    /// other side.
    pub(super) fn spawn_editor_overlay(
        &mut self,
        argv: &[String],
        host_cwd: Option<std::path::PathBuf>,
    ) -> Result<crate::api::schema::PaneInfo, (&'static str, String)> {
        let (env, _) = self.custom_command_env();
        let (ws_idx, new_pane) = self
            .spawn_overlay_argv_command(argv, host_cwd, env, Vec::new())
            .map_err(|err| ("editor_failed", err.to_string()))?;
        let new_pane_id = new_pane.pane_id;
        let terminal_id = new_pane.terminal.id.clone();
        self.terminal_runtimes
            .insert(terminal_id.clone(), new_pane.runtime);
        self.state.remove_alias_shadowed_by_new_pane(new_pane_id);
        self.state.terminals.insert(terminal_id, new_pane.terminal);

        let Some(pane) = self.pane_info(ws_idx, new_pane_id) else {
            return Err(("editor_failed", "editor pane disappeared".to_owned()));
        };
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::PaneCreated,
            data: crate::api::schema::EventData::PaneCreated { pane: pane.clone() },
        });
        if let Some(tab_idx) = self
            .overlay_panes
            .get(&new_pane_id)
            .map(|overlay| overlay.tab_idx)
        {
            self.emit_layout_updated_event(ws_idx, tab_idx);
        }
        Ok(pane)
    }
}

// The tests drive `/bin/sh`, Git, and Unix paths.
#[cfg(all(test, unix))]
mod tests;
