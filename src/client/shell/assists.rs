//! Ask AI: opens a coding-agent CLI of the server beside a pane through
//! `assist.*`, and sends it the pane's latest output.

use crossterm::event::{MouseButton, MouseEventKind};

use super::*;
use crate::api::schema::{
    AssistInfo, AssistPlacement, AssistProduct, AssistProductInfo, AssistRefreshParams,
    AssistStartParams, EmptyParams, Method, ResponseResult,
};

pub(super) const ASK_AI_LABEL: &str = "Ask AI…";
pub(super) const SEND_LATEST_LABEL: &str = "Send latest to AI";

/// The placement Tab moves to.
fn next_placement(placement: AssistPlacement) -> AssistPlacement {
    match placement {
        AssistPlacement::Auto => AssistPlacement::Right,
        AssistPlacement::Right => AssistPlacement::Below,
        AssistPlacement::Below => AssistPlacement::Auto,
    }
}

/// The button that shows and changes the placement; Tab is its key.
pub(super) fn placement_button(placement: AssistPlacement) -> &'static str {
    match placement {
        AssistPlacement::Auto => " tab auto ",
        AssistPlacement::Right => " tab right ",
        AssistPlacement::Below => " tab below ",
    }
}

pub(super) fn product_name(product: AssistProduct) -> &'static str {
    match product {
        AssistProduct::Claude => "claude",
        AssistProduct::Codex => "codex",
        AssistProduct::Copilot => "copilot",
        AssistProduct::Unknown => "unknown",
    }
}

impl ClientShellState {
    /// `(offers Ask AI…, offers Send latest)` for the pane menu of `pane_id`.
    pub(super) fn assist_menu_offers(&self, pane_id: &str) -> (bool, bool) {
        let is_helper = self.assist_links.iter().any(|link| link.pane_id == pane_id);
        let has_assist = is_helper
            || self
                .assist_links
                .iter()
                .any(|link| link.target_pane_id.as_deref() == Some(pane_id));
        let can_start = self.supports_endpoint_method(&Method::AssistStart(AssistStartParams {
            pane_id: String::new(),
            product: None,
            placement: AssistPlacement::Auto,
            focus: true,
        }));
        (can_start && !is_helper, has_assist)
    }

    /// Asks the server what assist links it has, so the next pane menu is right.
    /// Why silent when unsupported: the menu then offers no assist items at all.
    pub(super) fn refresh_assist_links(&mut self, outcome: &mut ClientShellInput) {
        let method = Method::AssistList(EmptyParams::default());
        if self.supports_endpoint_method(&method) {
            self.push_endpoint_method_with_kind(method, PendingEndpointKind::AssistList, outcome);
        }
    }

    pub(super) fn begin_assist(&mut self, pane_id: String, outcome: &mut ClientShellInput) {
        if self.assist_pending {
            return;
        }
        self.assist_pending = self.push_endpoint_method_with_kind(
            Method::AssistProducts(EmptyParams::default()),
            PendingEndpointKind::AssistProducts { pane_id },
            outcome,
        );
    }

    pub(super) fn send_latest_to_assist(
        &mut self,
        pane_id: String,
        outcome: &mut ClientShellInput,
    ) {
        self.push_endpoint_method_with_kind(
            Method::AssistRefresh(AssistRefreshParams { pane_id }),
            PendingEndpointKind::AssistRefresh,
            outcome,
        );
    }

    fn start_assist(
        &mut self,
        pane_id: String,
        product: AssistProduct,
        placement: AssistPlacement,
        outcome: &mut ClientShellInput,
    ) -> bool {
        self.assist_pending = self.push_endpoint_method_with_kind(
            Method::AssistStart(AssistStartParams {
                pane_id,
                product: Some(product),
                placement,
                focus: true,
            }),
            PendingEndpointKind::AssistStart,
            outcome,
        );
        self.assist_pending
    }

    pub(super) fn handle_assist_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let mut outcome = ClientShellInput::default();
        if matches!(
            kind,
            PendingEndpointKind::AssistProducts { .. } | PendingEndpointKind::AssistStart
        ) {
            self.assist_pending = false;
        }
        match (kind, result) {
            // Why nothing when another overlay is up: the user moved on while
            // the server answered, and the picker would replace their dialog.
            (PendingEndpointKind::AssistProducts { .. }, Ok(_)) if self.overlay.is_some() => {}
            (
                PendingEndpointKind::AssistProducts { pane_id },
                Ok(ResponseResult::AssistProducts { products }),
            ) => self.choose_assist_product(pane_id, products, &mut outcome),
            (
                PendingEndpointKind::AssistStart,
                Ok(ResponseResult::AssistStarted { assist, .. }),
            ) => {
                self.remember_assist(assist);
                if matches!(self.overlay, Some(ClientShellOverlay::Assist(_))) {
                    self.overlay = None;
                }
            }
            (PendingEndpointKind::AssistStart, Err(error)) => {
                if let Some(ClientShellOverlay::Assist(picker)) = self.overlay.as_mut() {
                    picker.starting = false;
                    picker.error = Some(error.message);
                }
            }
            (
                PendingEndpointKind::AssistRefresh,
                Ok(ResponseResult::AssistRefreshed { assist }),
            ) => {
                self.remember_assist(assist);
            }
            (PendingEndpointKind::AssistList, Ok(ResponseResult::AssistList { assists })) => {
                self.assist_links = assists;
                self.sync_pane_menu_assist_offers();
            }
            // The endpoint notice already says why the request failed.
            (_, _) => {}
        }
        (true, outcome.actions)
    }

    /// No product: says what to install. One: opens it. More: the picker.
    fn choose_assist_product(
        &mut self,
        pane_id: String,
        products: Vec<AssistProductInfo>,
        outcome: &mut ClientShellInput,
    ) {
        match products.as_slice() {
            [] => {
                self.push_endpoint_notice(
                    ClientEndpointNoticeKind::Rejected,
                    "assist.products:none",
                    "No AI CLI found",
                    "Install claude, codex or copilot on the PATH of the herdr server's machine.",
                );
            }
            [only] => {
                self.start_assist(pane_id, only.product, AssistPlacement::Auto, outcome);
            }
            _ => {
                let selected = self
                    .config
                    .assist_product
                    .and_then(|wanted| products.iter().position(|found| found.product == wanted))
                    .unwrap_or(0);
                self.overlay = Some(ClientShellOverlay::Assist(ClientAssistOverlay {
                    pane_id,
                    products,
                    selected,
                    placement: AssistPlacement::Auto,
                    query: TextEditor::default(),
                    starting: false,
                    error: None,
                }));
            }
        }
    }

    fn remember_assist(&mut self, assist: AssistInfo) {
        self.assist_links
            .retain(|link| link.pane_id != assist.pane_id);
        self.assist_links.push(assist);
        self.sync_pane_menu_assist_offers();
    }

    /// Why: the pane menu opens before `assist.list` answers.
    fn sync_pane_menu_assist_offers(&mut self) {
        let Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Pane { pane_id, .. },
            ..
        })) = self.overlay.as_ref()
        else {
            return;
        };
        let (ask, send) = self.assist_menu_offers(&pane_id.clone());
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.as_mut() else {
            return;
        };
        let ClientContextMenuTarget::Pane {
            offers_ask_ai,
            offers_send_latest,
            ..
        } = &mut menu.target
        else {
            return;
        };
        if (*offers_ask_ai, *offers_send_latest) == (ask, send) {
            return;
        }
        // Why by action: the items move, and the old index could now be
        // `Close pane`.
        let highlighted = menu.items().get(menu.highlighted).map(|item| item.action);
        if let ClientContextMenuTarget::Pane {
            offers_ask_ai,
            offers_send_latest,
            ..
        } = &mut menu.target
        {
            *offers_ask_ai = ask;
            *offers_send_latest = send;
        }
        menu.highlighted = highlighted
            .and_then(|action| menu.items().iter().position(|item| item.action == action))
            .unwrap_or(0);
        // Why: the rows of the last frame point at the old items until the
        // next frame draws the new ones.
        self.hits.context_menu_rows.clear();
    }

    fn move_assist_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::Assist(picker)) = self.overlay.as_mut() else {
            return;
        };
        let last = picker.products.len().saturating_sub(1) as isize;
        picker.selected = (picker.selected as isize + delta).clamp(0, last) as usize;
    }

    fn cycle_assist_placement(&mut self) {
        if let Some(ClientShellOverlay::Assist(picker)) = self.overlay.as_mut() {
            picker.placement = next_placement(picker.placement);
        }
    }

    fn submit_assist_picker(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Assist(picker)) = self.overlay.as_mut() else {
            return;
        };
        if picker.starting {
            return;
        }
        let Some(product) = picker
            .products
            .get(picker.selected)
            .map(|found| found.product)
        else {
            return;
        };
        picker.starting = true;
        picker.error = None;
        let pane_id = picker.pane_id.clone();
        let placement = picker.placement;
        if !self.start_assist(pane_id, product, placement, outcome) {
            if let Some(ClientShellOverlay::Assist(picker)) = self.overlay.as_mut() {
                picker.starting = false;
            }
        }
    }

    pub(super) fn route_assist_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::Assist(_))) {
            return false;
        }
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        match code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Enter => self.submit_assist_picker(outcome),
            KeyCode::Tab => self.cycle_assist_placement(),
            KeyCode::Up => self.move_assist_selection(-1),
            KeyCode::Down => self.move_assist_selection(1),
            KeyCode::Char('n' | 'p') if modifiers == crossterm::event::KeyModifiers::CONTROL => {
                self.move_assist_selection(if code == KeyCode::Char('n') { 1 } else { -1 });
            }
            _ => {}
        }
        outcome.repaint = true;
        true
    }

    /// Mouse on the picker: a row selects, the placement button cycles, the
    /// primary button opens, the wheel moves the selection.
    pub(super) fn route_assist_mouse(
        &mut self,
        kind: MouseEventKind,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::Assist(_))) {
            return false;
        }
        match kind {
            MouseEventKind::ScrollUp => self.move_assist_selection(-1),
            MouseEventKind::ScrollDown => self.move_assist_selection(1),
            MouseEventKind::Down(MouseButton::Left) => {
                if super::contains(self.hits.overlay_cancel, point) {
                    self.overlay = None;
                } else if super::contains(self.hits.overlay_secondary, point) {
                    self.cycle_assist_placement();
                } else if super::contains(self.hits.overlay_primary, point) {
                    self.submit_assist_picker(outcome);
                } else if let Some((_, index)) = self
                    .hits
                    .worktree_rows
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .copied()
                {
                    if let Some(ClientShellOverlay::Assist(picker)) = self.overlay.as_mut() {
                        picker.selected = index;
                    }
                }
            }
            _ => return true,
        }
        outcome.repaint = true;
        true
    }
}

#[cfg(test)]
mod tests;
