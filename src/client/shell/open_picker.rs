use super::*;

impl ClientShellState {
    /// Lists through `ssh.hosts` or `container.list` first, so the picker only
    /// offers choices the server can open. A server without the method gets the
    /// client-local "Action unavailable" notice from the endpoint push.
    pub(super) fn begin_open_picker(
        &mut self,
        source: OpenPickerSource,
        outcome: &mut ClientShellInput,
    ) {
        use crate::api::schema::{ContainerListParams, Method, SshHostsParams};

        let method = match source {
            OpenPickerSource::SshHost => Method::SshHosts(SshHostsParams::default()),
            // No workspace filter: the picker is about any running container.
            OpenPickerSource::Container => Method::ContainerList(ContainerListParams::default()),
        };
        self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::PrepareOpenPicker(source),
            outcome,
        );
        outcome.repaint = true;
    }

    pub(super) fn insert_open_picker_overlay_text(&mut self, text: &str) -> bool {
        let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() else {
            return false;
        };
        if open.search_focused && !open.opening && open.query.insert(text) {
            if let Some(first) = open.filtered_indices().first().copied() {
                open.selected = first;
            }
        }
        true
    }

    pub(super) fn route_open_picker_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() else {
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
            KeyCode::Enter => self.submit_open_picker(outcome),
            KeyCode::Up if !opening => {
                self.move_open_picker_selection(-1);
                outcome.repaint = true;
            }
            KeyCode::Down if !opening => {
                self.move_open_picker_selection(1);
                outcome.repaint = true;
            }
            KeyCode::Char('n' | 'p')
                if !opening && modifiers == crossterm::event::KeyModifiers::CONTROL =>
            {
                self.move_open_picker_selection(if code == KeyCode::Char('n') { 1 } else { -1 });
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

    pub(super) fn move_open_picker_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() else {
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

    pub(super) fn submit_open_picker(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() else {
            return;
        };
        if open.opening {
            return;
        }
        let Some(index) = open.selected_entry_index() else {
            return;
        };
        let source = open.source;
        let Some((id, title)) = open
            .entries
            .get(index)
            .map(|entry| (entry.id.clone(), entry.title.clone()))
        else {
            return;
        };
        open.selected = index;
        open.opening = true;
        open.error = None;
        let target = crate::api::schema::WorkspaceCreateParams {
            source_workspace_id: None,
            cwd: None,
            focus: true,
            label: Some(title),
            env: Default::default(),
        };
        let method = match source {
            OpenPickerSource::SshHost => {
                crate::api::schema::Method::SshOpen(crate::api::schema::SshOpenParams {
                    host: id,
                    target: crate::api::schema::SshOpenTarget::Workspace(target),
                })
            }
            OpenPickerSource::Container => {
                crate::api::schema::Method::ContainerOpen(crate::api::schema::ContainerOpenParams {
                    container: id,
                    target: crate::api::schema::ContainerOpenTarget::Workspace(target),
                })
            }
        };
        if !self.push_endpoint_method_with_kind(method, PendingEndpointKind::OpenPicker, outcome) {
            if let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() {
                open.opening = false;
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn handle_open_picker_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        use crate::api::schema::ResponseResult;

        match (kind, result) {
            (PendingEndpointKind::PrepareOpenPicker(source), Ok(listing)) => {
                let entries = match (source, listing) {
                    (OpenPickerSource::SshHost, ResponseResult::SshHosts { hosts }) => hosts
                        .into_iter()
                        .map(|host| {
                            ClientOpenPickerEntry::ssh_host(host.alias, host.hostname, host.user)
                        })
                        .collect::<Vec<_>>(),
                    (OpenPickerSource::Container, ResponseResult::ContainerList { containers }) => {
                        containers
                            .into_iter()
                            .map(ClientOpenPickerEntry::container)
                            .collect::<Vec<_>>()
                    }
                    _ => {
                        self.set_endpoint_error("endpoint returned an unexpected listing result");
                        return true;
                    }
                };
                if entries.is_empty() {
                    self.set_endpoint_error(match source {
                        OpenPickerSource::SshHost => "No ssh hosts found in ~/.ssh/config.",
                        OpenPickerSource::Container => "No running docker containers found.",
                    });
                } else {
                    self.overlay = Some(ClientShellOverlay::OpenPicker(ClientOpenPickerOverlay {
                        source,
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
            (PendingEndpointKind::OpenPicker, Ok(ResponseResult::WorkspaceCreated { .. })) => {
                self.overlay = None;
                true
            }
            (PendingEndpointKind::OpenPicker, Err(error)) => {
                // Kept open so the user can pick another entry or retry after
                // fixing the setup; the rejection itself is not retried.
                if let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() {
                    open.opening = false;
                    open.error = Some(error.message);
                }
                true
            }
            // The endpoint already raised a rejection notice for the listing.
            (PendingEndpointKind::PrepareOpenPicker(_), Err(_)) => true,
            (_, Ok(_)) => {
                if let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() {
                    open.opening = false;
                }
                self.set_endpoint_error("endpoint returned an unexpected open picker result");
                true
            }
            (_, Err(_)) => true,
        }
    }
}
