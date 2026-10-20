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
        use crate::api::schema::{Method, SshContainerListParams, SshHostsParams};

        match source {
            OpenPickerSource::SshHost => {
                self.push_endpoint_method_with_kind(
                    Method::SshHosts(SshHostsParams::default()),
                    PendingEndpointKind::PrepareOpenPicker(source),
                    outcome,
                );
            }
            OpenPickerSource::ContainerHost | OpenPickerSource::Container => {
                let hosts = Method::SshHosts(SshHostsParams::default());
                let remote_list = Method::SshContainerList(SshContainerListParams {
                    host: String::new(),
                    workspace_id: None,
                });
                // Checked up front, not through the push: an old server must
                // fall back to local listing without an "Action unavailable" notice.
                // Both methods are required so a host is never offered that
                // cannot be listed, and no remote pick falls back to local docker.
                if self.supports_endpoint_method(&hosts)
                    && self.supports_endpoint_method(&remote_list)
                {
                    self.push_endpoint_method_with_kind(
                        hosts,
                        PendingEndpointKind::PrepareOpenPicker(OpenPickerSource::ContainerHost),
                        outcome,
                    );
                } else {
                    self.list_local_containers(outcome);
                }
            }
        }
        outcome.repaint = true;
    }

    fn list_local_containers(&mut self, outcome: &mut ClientShellInput) {
        use crate::api::schema::{ContainerListParams, Method};

        // No workspace filter: the picker is about any running container.
        self.push_endpoint_method_with_kind(
            Method::ContainerList(ContainerListParams::default()),
            PendingEndpointKind::PrepareOpenPicker(OpenPickerSource::Container),
            outcome,
        );
    }

    /// Esc and the cancel button: one stage back when the host stage is
    /// retained, otherwise close.
    pub(super) fn cancel_open_picker(&mut self) {
        let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() else {
            return;
        };
        let Some(stage) = open.host_stage.take() else {
            self.overlay = None;
            return;
        };
        open.source = OpenPickerSource::ContainerHost;
        open.entries = stage.entries;
        open.selected = stage.selected;
        open.query = TextEditor::default();
        open.error = None;
        open.reorder(&self.open_picker_memory);
    }

    /// Stars or unstars `index`, or the selected row when `None`, and saves
    /// it at once like the other chrome preferences.
    pub(super) fn toggle_open_picker_favorite(
        &mut self,
        index: Option<usize>,
        outcome: &mut ClientShellInput,
    ) {
        let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() else {
            return;
        };
        if open.opening {
            return;
        }
        let Some(key) = index
            .or_else(|| open.selected_entry_index())
            .and_then(|index| open.entries.get(index))
            .map(|entry| entry.key.clone())
        else {
            return;
        };
        self.open_picker_memory.toggle_favorite(&key);
        open.reorder(&self.open_picker_memory);
        outcome.repaint = true;
        self.persist_chrome_preferences(outcome);
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
        // Taken before the filter, so `*` never reaches the query, as Enter
        // never does. Why not let the filter have it: container names and
        // listed ssh aliases never contain `*`, so such a query matches no row.
        if code == KeyCode::Char('*') {
            self.toggle_open_picker_favorite(None, outcome);
            return true;
        }
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
                self.cancel_open_picker();
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
        let Some((id, title, kind, entry_key)) = open.entries.get(index).map(|entry| {
            (
                entry.id.clone(),
                entry.title.clone(),
                entry.kind.clone(),
                entry.key.clone(),
            )
        }) else {
            return;
        };
        // Dispatched on the row, not `open.source`: the host stage also holds
        // favorite containers, which open straight from there.
        let (container_host, host_row) = match kind {
            OpenPickerRowKind::FavoriteContainer(host) => (Some(host), None),
            OpenPickerRowKind::Host(host) => (None, Some(host)),
            OpenPickerRowKind::Plain if source == OpenPickerSource::Container => {
                (Some(open.host.clone()), None)
            }
            OpenPickerRowKind::Plain => (None, None),
        };
        // A container open is the only success the host picker ever sees, so
        // it also counts as a use of the docker host it ran on.
        let history_keys = match &container_host {
            Some(host) => vec![entry_key, host.history_key()],
            None => vec![entry_key],
        };
        open.selected = index;
        open.opening = true;
        open.error = None;
        let ssh_host = container_host.as_ref().and_then(OpenPickerHost::ssh_alias);
        // Remote rows share names across hosts, so the workspace label says
        // which. A favorite row's title already carries it.
        let title = match &ssh_host {
            Some(alias) if source == OpenPickerSource::Container => format!("{title}@{alias}"),
            _ => title,
        };
        let target = crate::api::schema::WorkspaceCreateParams {
            source_workspace_id: None,
            cwd: None,
            focus: true,
            label: Some(title),
            env: Default::default(),
        };
        let method = match (source, container_host.is_some()) {
            (OpenPickerSource::SshHost, _) => {
                crate::api::schema::Method::SshOpen(crate::api::schema::SshOpenParams {
                    host: id,
                    target: crate::api::schema::SshOpenTarget::Workspace(target),
                })
            }
            (_, true) => {
                let target = crate::api::schema::ContainerOpenTarget::Workspace(target);
                match ssh_host {
                    Some(host) => crate::api::schema::Method::SshContainerOpen(
                        crate::api::schema::SshContainerOpenParams {
                            host,
                            container: id,
                            target,
                        },
                    ),
                    None => crate::api::schema::Method::ContainerOpen(
                        crate::api::schema::ContainerOpenParams {
                            container: id,
                            target,
                        },
                    ),
                }
            }
            (OpenPickerSource::Container, false) => {
                // A listed container row is always `Plain` on the container stage.
                open.opening = false;
                return;
            }
            (OpenPickerSource::ContainerHost, false) => {
                let Some(host) = host_row else {
                    open.opening = false;
                    return;
                };
                let kind = PendingEndpointKind::ListContainers(host.clone());
                let method = match host.ssh_alias() {
                    Some(host) => crate::api::schema::Method::SshContainerList(
                        crate::api::schema::SshContainerListParams {
                            host,
                            workspace_id: None,
                        },
                    ),
                    None => crate::api::schema::Method::ContainerList(Default::default()),
                };
                if !self.push_endpoint_method_with_kind(method, kind, outcome) {
                    if let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() {
                        open.opening = false;
                    }
                }
                outcome.repaint = true;
                return;
            }
        };
        if !self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::OpenPicker { history_keys },
            outcome,
        ) {
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
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crate::api::schema::ResponseResult;

        match (kind, result) {
            (
                PendingEndpointKind::PrepareOpenPicker(OpenPickerSource::ContainerHost),
                Ok(ResponseResult::SshHosts { hosts }),
            ) => {
                if hosts.is_empty() {
                    // A host stage with only "local" would cost a keystroke for nothing.
                    self.list_local_containers(outcome);
                    return true;
                }
                let mut entries = favorite_container_entries(&self.open_picker_memory, &hosts);
                entries.push(ClientOpenPickerEntry::container_host(
                    OpenPickerHost::Local,
                    "this machine's docker".to_owned(),
                ));
                entries.extend(hosts.into_iter().map(|host| {
                    let row = ClientOpenPickerEntry::ssh_host(host.alias, host.hostname, host.user);
                    ClientOpenPickerEntry::container_host(
                        OpenPickerHost::Ssh(row.title),
                        row.detail,
                    )
                }));
                self.overlay = Some(ClientShellOverlay::OpenPicker(
                    ClientOpenPickerOverlay::new(
                        OpenPickerSource::ContainerHost,
                        entries,
                        &self.open_picker_memory,
                    ),
                ));
                true
            }
            (PendingEndpointKind::ListContainers(host), result) => {
                let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() else {
                    return true;
                };
                open.opening = false;
                match result {
                    Ok(ResponseResult::ContainerList { containers }) if containers.is_empty() => {
                        open.error = Some(format!(
                            "No running docker containers found on {}.",
                            host.display_name()
                        ));
                    }
                    Ok(ResponseResult::ContainerList { containers }) => {
                        let entries = containers
                            .into_iter()
                            .map(|info| ClientOpenPickerEntry::container(info, &host))
                            .collect();
                        let mut next = ClientOpenPickerOverlay::new(
                            OpenPickerSource::Container,
                            entries,
                            &self.open_picker_memory,
                        );
                        next.host = host;
                        next.host_stage = Some(OpenPickerHostStage {
                            entries: std::mem::take(&mut open.entries),
                            selected: open.selected,
                        });
                        *open = next;
                    }
                    Ok(_) => {
                        open.error =
                            Some("endpoint returned an unexpected listing result".to_owned());
                    }
                    // Kept on the host stage so another host can be picked.
                    Err(error) => open.error = Some(error.message),
                }
                true
            }
            (PendingEndpointKind::PrepareOpenPicker(source), Ok(listing)) => {
                let entries = match (source, listing) {
                    (OpenPickerSource::SshHost, ResponseResult::SshHosts { hosts }) => hosts
                        .into_iter()
                        .map(|host| {
                            ClientOpenPickerEntry::ssh_host(host.alias, host.hostname, host.user)
                        })
                        .collect::<Vec<_>>(),
                    (
                        OpenPickerSource::Container | OpenPickerSource::ContainerHost,
                        ResponseResult::ContainerList { containers },
                    ) => containers
                        .into_iter()
                        .map(|info| ClientOpenPickerEntry::container(info, &OpenPickerHost::Local))
                        .collect::<Vec<_>>(),
                    _ => {
                        self.set_endpoint_error("endpoint returned an unexpected listing result");
                        return true;
                    }
                };
                if entries.is_empty() {
                    self.set_endpoint_error(match source {
                        OpenPickerSource::SshHost => "No ssh hosts found in ~/.ssh/config.",
                        OpenPickerSource::Container | OpenPickerSource::ContainerHost => {
                            "No running docker containers found."
                        }
                    });
                } else {
                    self.overlay = Some(ClientShellOverlay::OpenPicker(
                        ClientOpenPickerOverlay::new(source, entries, &self.open_picker_memory),
                    ));
                }
                true
            }
            (
                PendingEndpointKind::OpenPicker { history_keys },
                Ok(ResponseResult::WorkspaceCreated { .. }),
            ) => {
                self.overlay = None;
                let now_ms = super::open_picker_memory::now_ms();
                for key in &history_keys {
                    self.open_picker_memory.record_open(key, now_ms);
                }
                self.persist_chrome_preferences(outcome);
                true
            }
            // Why not record a rejected open: history is meant to surface
            // targets that work, not ones the user just failed to reach.
            (PendingEndpointKind::OpenPicker { .. }, Err(error)) => {
                // Kept open so the user can pick another entry or retry after
                // fixing the setup; the rejection itself is not retried.
                if let Some(ClientShellOverlay::OpenPicker(open)) = self.overlay.as_mut() {
                    open.opening = false;
                    open.error = Some(error.message);
                }
                true
            }
            // Why not fall back: a cancellation arrives while the endpoint is
            // being replaced, so a fresh request would be left unsent.
            (PendingEndpointKind::PrepareOpenPicker(_), Err(error))
                if error.code.as_deref() == Some("endpoint_cancelled") =>
            {
                true
            }
            // Why not stop at the notice the endpoint raised: a broken ssh
            // config must not hide this machine's own containers.
            (PendingEndpointKind::PrepareOpenPicker(OpenPickerSource::ContainerHost), Err(_)) => {
                self.list_local_containers(outcome);
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

/// Favorites whose docker host exists on this endpoint. Why not check the
/// container: that would cost a listing per host before the picker shows; a
/// stopped one fails on open with the server's own error instead.
fn favorite_container_entries(
    memory: &preferences::ClientOpenPickerPreferences,
    hosts: &[crate::api::schema::SshHostInfoDto],
) -> Vec<ClientOpenPickerEntry> {
    memory
        .favorites
        .iter()
        .filter_map(|key| {
            let (host, name) = key.strip_prefix("container:")?.split_once(':')?;
            if name.is_empty() {
                return None;
            }
            let host = if host == "local" {
                OpenPickerHost::Local
            } else if hosts.iter().any(|known| known.alias == host) {
                OpenPickerHost::Ssh(host.to_owned())
            } else {
                return None;
            };
            Some(ClientOpenPickerEntry::favorite_container(
                host,
                name.to_owned(),
            ))
        })
        .collect()
}
