use super::*;

impl ClientShellState {
    /// Lists hosts through `ssh.hosts` first, so the picker only offers
    /// aliases the server can open. A server without the method gets the
    /// client-local "Action unavailable" notice from the endpoint push.
    pub(super) fn begin_ssh_open(&mut self, outcome: &mut ClientShellInput) {
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::SshHosts(crate::api::schema::SshHostsParams::default()),
            PendingEndpointKind::PrepareSshOpen,
            outcome,
        );
        outcome.repaint = true;
    }

    pub(super) fn insert_ssh_overlay_text(&mut self, text: &str) -> bool {
        let Some(ClientShellOverlay::SshOpen(open)) = self.overlay.as_mut() else {
            return false;
        };
        if open.search_focused && !open.opening && open.query.insert(text) {
            if let Some(first) = open.filtered_indices().first().copied() {
                open.selected = first;
            }
        }
        true
    }

    pub(super) fn route_ssh_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::SshOpen(open)) = self.overlay.as_mut() else {
            return false;
        };
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let opening = open.opening;
        let search_focused = open.search_focused;
        if !opening && search_focused {
            if let Some(content_changed) = open.query.handle_key(key) {
                if content_changed {
                    if let Some(first) = open.filtered_indices().first().copied() {
                        open.selected = first;
                    }
                }
                outcome.repaint = true;
                return true;
            }
        }
        match code {
            KeyCode::Esc if !opening => {
                self.overlay = None;
                outcome.repaint = true;
            }
            KeyCode::Enter => self.submit_ssh_open(outcome),
            KeyCode::Up if !opening => {
                self.move_ssh_open_selection(-1);
                outcome.repaint = true;
            }
            KeyCode::Down if !opening => {
                self.move_ssh_open_selection(1);
                outcome.repaint = true;
            }
            KeyCode::Char('n' | 'p')
                if !opening && modifiers == crossterm::event::KeyModifiers::CONTROL =>
            {
                self.move_ssh_open_selection(if code == KeyCode::Char('n') { 1 } else { -1 });
                outcome.repaint = true;
            }
            KeyCode::Char('/') if !opening && !search_focused => {
                open.search_focused = true;
                outcome.repaint = true;
            }
            _ => {}
        }
        true
    }

    pub(super) fn move_ssh_open_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::SshOpen(open)) = self.overlay.as_mut() else {
            return;
        };
        let filtered = open.filtered_indices();
        if filtered.is_empty() {
            open.selected = 0;
            return;
        }
        let current = filtered
            .iter()
            .position(|index| *index == open.selected)
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, filtered.len() as isize - 1) as usize;
        open.selected = filtered[next];
    }

    pub(super) fn submit_ssh_open(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::SshOpen(open)) = self.overlay.as_mut() else {
            return;
        };
        if open.opening {
            return;
        }
        let Some(index) = open.selected_entry_index() else {
            return;
        };
        let Some(alias) = open.entries.get(index).map(|entry| entry.alias.clone()) else {
            return;
        };
        open.selected = index;
        open.opening = true;
        open.error = None;
        let method = crate::api::schema::Method::SshOpen(crate::api::schema::SshOpenParams {
            host: alias.clone(),
            target: crate::api::schema::SshOpenTarget::Workspace(
                crate::api::schema::WorkspaceCreateParams {
                    source_workspace_id: None,
                    cwd: None,
                    focus: true,
                    label: Some(alias),
                    env: Default::default(),
                },
            ),
        });
        if !self.push_endpoint_method_with_kind(method, PendingEndpointKind::SshOpen, outcome) {
            if let Some(ClientShellOverlay::SshOpen(open)) = self.overlay.as_mut() {
                open.opening = false;
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn handle_ssh_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        use crate::api::schema::ResponseResult;

        match (kind, result) {
            (PendingEndpointKind::PrepareSshOpen, Ok(ResponseResult::SshHosts { hosts })) => {
                let entries = hosts
                    .into_iter()
                    .map(|host| ClientSshHostEntry::new(host.alias, host.hostname, host.user))
                    .collect::<Vec<_>>();
                if entries.is_empty() {
                    self.set_endpoint_error("No ssh hosts found in ~/.ssh/config.");
                } else {
                    self.overlay = Some(ClientShellOverlay::SshOpen(ClientSshOpenOverlay {
                        entries,
                        selected: 0,
                        query: TextEditor::default(),
                        search_focused: false,
                        error: None,
                        opening: false,
                    }));
                }
                true
            }
            (PendingEndpointKind::SshOpen, Ok(ResponseResult::WorkspaceCreated { .. })) => {
                self.overlay = None;
                true
            }
            (PendingEndpointKind::SshOpen, Err(error)) => {
                // Kept open so the user can pick another host or retry after
                // fixing the config; the rejection itself is not retried.
                if let Some(ClientShellOverlay::SshOpen(open)) = self.overlay.as_mut() {
                    open.opening = false;
                    open.error = Some(error.message);
                }
                true
            }
            // The endpoint already raised a rejection notice for the listing.
            (PendingEndpointKind::PrepareSshOpen, Err(_)) => true,
            (_, Ok(_)) => {
                if let Some(ClientShellOverlay::SshOpen(open)) = self.overlay.as_mut() {
                    open.opening = false;
                }
                self.set_endpoint_error("endpoint returned an unexpected ssh host result");
                true
            }
            (_, Err(_)) => true,
        }
    }
}
