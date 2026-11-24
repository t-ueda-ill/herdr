//! `assist.*`: open a coding-agent CLI of this machine beside a pane, give it
//! what the pane shows, and tell it when that changes.

use std::path::{Path, PathBuf};

use crate::api::schema::{
    AgentPromptParams, AssistInfo, AssistProduct, AssistProductInfo, AssistRefreshParams,
    AssistStartParams, EventData, EventEnvelope, EventKind, ResponseResult, SplitDirection,
};
use crate::app::App;
use crate::assist::{AssistContext, AssistLink, ContextRead};
use crate::terminal::TerminalId;

use super::responses::{encode_error, encode_success};

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

impl App {
    /// The products found on the `PATH` this server gives its panes.
    pub(super) fn assist_products(&self) -> Vec<(AssistProduct, PathBuf)> {
        #[cfg(test)]
        if let Some(path) = &self.test_assist_path {
            return crate::assist::find_products(Some(path), crate::pane::is_executable_file);
        }
        crate::assist::find_products(
            std::env::var_os("PATH").as_deref(),
            crate::pane::is_executable_file,
        )
    }

    pub(super) fn handle_assist_products(&mut self, id: String) -> String {
        let products = self
            .assist_products()
            .into_iter()
            .map(|(product, path)| AssistProductInfo {
                product,
                path: path.display().to_string(),
            })
            .collect();
        encode_success(id, ResponseResult::AssistProducts { products })
    }

    pub(super) fn handle_assist_list(&mut self, id: String) -> String {
        let assists = self
            .state
            .assists
            .iter()
            .filter_map(|link| self.assist_info(link))
            .collect();
        encode_success(id, ResponseResult::AssistList { assists })
    }

    pub(super) fn handle_assist_start(&mut self, id: String, params: AssistStartParams) -> String {
        let Some((ws_idx, target_pane)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(target_terminal) = self.state.terminal_id_for_pane(ws_idx, target_pane) else {
            return pane_not_found(id, &params.pane_id);
        };
        if self.state.assists.is_helper(&target_terminal) {
            return encode_error(
                id,
                "pane_is_assist",
                format!(
                    "pane {} is an assist; open it for the pane it reads",
                    params.pane_id
                ),
            );
        }
        if let Some(link) = self.state.assists.for_target(&target_terminal).cloned() {
            return self.focus_existing_assist(id, &link);
        }
        let products = self.assist_products();
        let found = match params.product {
            Some(wanted) => products.iter().find(|(product, _)| *product == wanted),
            None => products.first(),
        };
        let Some((product, executable)) = found.cloned() else {
            let found: Vec<_> = products
                .iter()
                .map(|(product, _)| format!("{product:?}").to_lowercase())
                .collect();
            return encode_error(
                id,
                "assist_product_not_found",
                format!(
                    "no {} on this server's PATH (found: {})",
                    params
                        .product
                        .map(|product| format!("{product:?}").to_lowercase())
                        .unwrap_or_else(|| "claude, codex or copilot".into()),
                    if found.is_empty() {
                        "none".into()
                    } else {
                        found.join(", ")
                    }
                ),
            );
        };
        let Some(context) = self.assist_context(ws_idx, target_pane) else {
            return pane_not_found(id, &params.pane_id);
        };
        let context_path = match write_new_context(&context) {
            Ok(path) => path,
            Err(err) => return encode_error(id, "assist_context_failed", err.to_string()),
        };
        let context_dir = context_path
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        // Why the pane id and not its label: the label can hold a terminal
        // title with command arguments, and argv shows in process listings.
        let ask = crate::assist::ask_line(&context_path, &context.pane_id);
        let Some(argv) = crate::assist::product_argv(product, &executable, &ask, &context_dir)
        else {
            remove_context(&context_path);
            return encode_error(id, "assist_product_not_found", "unknown assist product");
        };
        let target_cols = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, target_pane)
            .map(|runtime| runtime.current_size().1)
            .unwrap_or(0);
        let (direction, ratio) = crate::assist::split_for(params.placement, target_cols);
        let direction = match direction {
            SplitDirection::Right => ratatui::layout::Direction::Horizontal,
            SplitDirection::Down => ratatui::layout::Direction::Vertical,
        };
        let follow_cwd = self.launch_cwd_for_pane_in_workspace(ws_idx, target_pane);
        let cwd = self.resolve_new_terminal_cwd(follow_cwd);
        let (rows, cols) = self
            .state
            .new_pane_size(crate::ui::NewPanePlacement::Split {
                ws_idx,
                target: target_pane,
                direction,
                ratio,
            });
        let previous_focus = self.state.current_pane_focus_target();
        let extra_env = vec![("HERDR_ASSIST_TARGET".to_owned(), params.pane_id.clone())];
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            remove_context(&context_path);
            return pane_not_found(id, &params.pane_id);
        };
        let result = ws.split_pane_argv_command_with_ratio(
            target_pane,
            direction,
            ratio,
            rows,
            cols,
            Some(cwd),
            &argv,
            extra_env,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            params.focus,
        );
        let (tab_idx, new_pane) = match result {
            Some(Ok(result)) => result,
            Some(Err(err)) => {
                remove_context(&context_path);
                return encode_error(id, "assist_start_failed", err.to_string());
            }
            None => {
                remove_context(&context_path);
                return pane_not_found(id, &params.pane_id);
            }
        };
        if params.focus {
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            self.state
                .record_pane_focus_change(previous_focus, ws_idx, new_pane.pane_id);
            self.state.mode = crate::app::Mode::Terminal;
        }
        let helper_terminal = new_pane.terminal.id.clone();
        let mut terminal = new_pane.terminal;
        terminal.set_manual_label(format!("assist ▸ {}", context.label));
        self.terminal_runtimes
            .insert(helper_terminal.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state
            .terminals
            .insert(helper_terminal.clone(), terminal);
        let link = AssistLink {
            helper: helper_terminal,
            target: Some(target_terminal),
            product,
            context_path,
        };
        self.state.assists.insert(link.clone());
        self.schedule_session_save();
        if let Some(pane) = self.pane_info(ws_idx, new_pane.pane_id) {
            self.emit_event(EventEnvelope {
                event: EventKind::PaneCreated,
                data: EventData::PaneCreated { pane },
            });
        }
        self.emit_layout_updated_event(ws_idx, tab_idx);
        match self.assist_info(&link) {
            Some(assist) => encode_success(
                id,
                ResponseResult::AssistStarted {
                    assist,
                    created: true,
                },
            ),
            None => encode_error(id, "assist_start_failed", "the assist pane disappeared"),
        }
    }

    pub(super) fn handle_assist_refresh(
        &mut self,
        id: String,
        params: AssistRefreshParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal) = self.state.terminal_id_for_pane(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(link) = self.state.assists.for_either(&terminal).cloned() else {
            return encode_error(
                id,
                "assist_not_found",
                format!("pane {} has no assist", params.pane_id),
            );
        };
        let Some((target_ws, target_pane)) = link
            .target
            .as_ref()
            .and_then(|target| self.state.pane_of_terminal(target))
        else {
            return encode_error(
                id,
                "assist_target_closed",
                "the pane this assist reads has closed",
            );
        };
        let Some(context) = self.assist_context(target_ws, target_pane) else {
            return encode_error(
                id,
                "assist_target_closed",
                "the pane this assist reads has closed",
            );
        };
        if let Err(err) = write_context(&link.context_path, &context) {
            return encode_error(id, "assist_context_failed", err.to_string());
        }
        let Some(helper_pane) = self
            .state
            .pane_of_terminal(&link.helper)
            .and_then(|(ws_idx, pane_id)| self.public_pane_id(ws_idx, pane_id))
        else {
            return encode_error(id, "assist_not_found", "the assist pane has closed");
        };
        let prompt = AgentPromptParams {
            target: helper_pane,
            text: crate::assist::refresh_line(&link.context_path),
            wait: None,
        };
        // Why the agent prompt path: it knows how each product takes a
        // submitted line, and it refuses an assist that cannot take input.
        let validated = match self.validate_agent_prompt(id.clone(), &prompt) {
            Ok(validated) => validated,
            Err(response) => return response,
        };
        // Why the completion is dropped: the answer says the file is new and
        // the line is queued; the assist's reply shows in its own pane.
        if let Err(response) = self.submit_agent_prompt(validated, &prompt) {
            return response;
        }
        match self.assist_info(&link) {
            Some(assist) => encode_success(id, ResponseResult::AssistRefreshed { assist }),
            None => encode_error(id, "assist_not_found", "the assist pane has closed"),
        }
    }

    /// Deletes every assist's context folder. Why at server stop: assists do
    /// not outlive the server, and the folders would stay in the temp folder.
    pub(crate) fn remove_assist_contexts(&mut self) {
        let mut paths = self.state.assists.take_removed_files();
        paths.extend(
            self.state
                .assists
                .iter()
                .map(|link| link.context_path.clone()),
        );
        for path in paths {
            remove_context(&path);
        }
    }

    /// Whether an agent target, a pane id or an agent name, names an assist's pane.
    ///
    /// Why the pane id first: an assist whose CLI is not detected yet has no
    /// agent to resolve, and must be refused all the same.
    pub(super) fn agent_target_is_assist(&self, target: &str) -> bool {
        self.parse_pane_id(target)
            .or_else(|| {
                self.resolve_agent_target(target)
                    .ok()
                    .map(|resolved| (resolved.ws_idx, resolved.pane_id))
            })
            .and_then(|(ws_idx, pane_id)| self.state.terminal_id_for_pane(ws_idx, pane_id))
            .is_some_and(|terminal| self.state.assists.is_helper(&terminal))
    }

    fn focus_existing_assist(&mut self, id: String, link: &AssistLink) -> String {
        if let Some((ws_idx, pane_id)) = self.state.pane_of_terminal(&link.helper) {
            self.state.focus_pane_in_workspace(ws_idx, pane_id);
            self.state.mark_active_tab_seen();
            self.state.mode = crate::app::Mode::Terminal;
        }
        match self.assist_info(link) {
            Some(assist) => encode_success(
                id,
                ResponseResult::AssistStarted {
                    assist,
                    created: false,
                },
            ),
            None => encode_error(id, "assist_not_found", "the assist pane has closed"),
        }
    }

    fn assist_info(&self, link: &AssistLink) -> Option<AssistInfo> {
        let public_pane = |terminal: &TerminalId| {
            self.state
                .pane_of_terminal(terminal)
                .and_then(|(ws_idx, pane_id)| self.public_pane_id(ws_idx, pane_id))
        };
        Some(AssistInfo {
            pane_id: public_pane(&link.helper)?,
            target_pane_id: link.target.as_ref().and_then(public_pane),
            product: link.product,
            context_path: link.context_path.display().to_string(),
        })
    }

    /// What the assist reads about a pane: commands from a shell that marks its
    /// prompts, otherwise the bottom of the screen.
    pub(super) fn assist_context(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<AssistContext> {
        let pane = self.pane_info(ws_idx, pane_id)?;
        let runtime =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;
        // Why an agent is never read by command: its own TUI draws over the
        // shell's marks, and scanning them would only hold the core lock.
        let read = if pane.agent.is_some() {
            ContextRead::Screen(runtime.detection_text())
        } else {
            match runtime.recent_commands(
                crate::assist::CONTEXT_COMMANDS,
                crate::assist::CONTEXT_LINES,
            ) {
                Ok(recent) => ContextRead::Commands {
                    commands: recent.commands,
                    waiting_prompt: recent.waiting_prompt,
                },
                Err(_) => ContextRead::Screen(
                    runtime
                        .recent_text_snapshot(crate::assist::CONTEXT_LINES)
                        .text,
                ),
            }
        };
        let label = pane
            .label
            .clone()
            .or_else(|| pane.title.clone())
            .or_else(|| pane.agent.clone())
            .or_else(|| pane.terminal_title_stripped.clone())
            .unwrap_or_else(|| pane.pane_id.clone());
        Some(AssistContext {
            label,
            location: pane
                .location
                .as_ref()
                .map(|location| location.id.clone())
                .unwrap_or_else(|| "local".into()),
            cwd: pane.foreground_cwd.clone().or_else(|| pane.cwd.clone()),
            agent: pane.agent.clone(),
            pane_id: pane.pane_id,
            read,
        })
    }
}

/// A new folder only this user can open, with the context file in it.
///
/// Why a folder per assist in the temp folder, as for task results: the
/// assist's CLI is allowed to read this folder, and nothing else must be in it.
fn write_new_context(context: &AssistContext) -> std::io::Result<PathBuf> {
    let dir =
        std::env::temp_dir().join(format!("herdr-assist-{}", crate::tasks::unguessable_name()));
    crate::platform::create_private_dir(&dir)?;
    let path = dir.join("context.md");
    if let Err(err) = write_context(&path, context) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(err);
    }
    Ok(path)
}

fn write_context(path: &Path, context: &AssistContext) -> std::io::Result<()> {
    std::fs::write(
        path,
        crate::assist::render_context(context, crate::assist::CONTEXT_LINES),
    )?;
    crate::platform::set_permission_bits(path, 0o600)
}

/// Removes a context file and its folder.
pub(crate) fn remove_context(path: &Path) {
    let _ = std::fs::remove_file(path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests;
