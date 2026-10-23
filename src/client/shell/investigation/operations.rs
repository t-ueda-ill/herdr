use super::super::file_viewer::FileLocation;
use super::*;
use crate::api::schema::*;
use regex::RegexBuilder;

pub(in crate::client::shell) fn prepare_document(
    tab: &mut Investigation,
    document: FileDocument,
) -> Result<(), String> {
    if !matches!(tab.dialog, Some(Dialog::Replace { .. })) {
        return Ok(());
    }
    let selected: Vec<_> = tab
        .matches
        .iter()
        .enumerate()
        .filter(|(i, m)| m.path == document.path && !tab.omitted.contains(i))
        .map(|(_, m)| m)
        .collect();
    if selected.iter().any(|m| m.revision != document.revision) {
        return Err(format!(
            "{} changed; search again before replacing",
            document.path
        ));
    }
    let mut source = if tab.regex {
        tab.query.to_string()
    } else {
        regex::escape(tab.query.as_str())
    };
    if tab.whole_word {
        source = format!(r"\b(?:{source})\b");
    }
    let regex = RegexBuilder::new(&source)
        .case_insensitive(!tab.case_sensitive)
        .build()
        .map_err(|e| e.to_string())?;
    let offsets: std::collections::BTreeSet<_> =
        selected.iter().map(|m| (m.start, m.end)).collect();
    let mut edits = Vec::new();
    for captures in regex.captures_iter(&document.text) {
        let Some(found) = captures.get(0) else {
            continue;
        };
        if !offsets.contains(&(found.start(), found.end())) {
            continue;
        }
        let replacement = if tab.regex {
            let mut value = String::new();
            captures.expand(tab.replacement.as_str(), &mut value);
            value
        } else {
            tab.replacement.to_string()
        };
        edits.push((found.start(), found.end(), replacement));
    }
    if edits.len() != selected.len() {
        return Err("search matches changed; search again".into());
    }
    let mut after = document.text.clone();
    for (start, end, replacement) in edits.into_iter().rev() {
        after.replace_range(start..end, &replacement);
    }
    if after != document.text {
        let before_lines: Vec<_> = document.text.lines().collect();
        let after_lines: Vec<_> = after.lines().collect();
        let mut diff = vec![format!("--- {}", document.path)];
        for index in 0..before_lines.len().max(after_lines.len()) {
            let a = before_lines.get(index);
            let b = after_lines.get(index);
            if a != b {
                if let Some(a) = a {
                    diff.push(format!("-{} {a}", index + 1));
                }
                if let Some(b) = b {
                    diff.push(format!("+{} {b}", index + 1));
                }
            }
        }
        if tab.plan.iter().map(|p| p.after.text.len()).sum::<usize>() + after.len() > 512 * 1024 {
            return Err("replacement exceeds the 512 KiB batch limit; narrow the search".into());
        }
        tab.plan.push(Replacement {
            diff,
            before: FileChange {
                path: document.path.clone(),
                revision: document.revision.clone(),
                text: document.text,
            },
            after: FileChange {
                path: document.path,
                revision: document.revision,
                text: after,
            },
        });
    }
    Ok(())
}
/// Said in place of content search, replacement and file operations, which
/// only know the host's filesystem.
pub(in crate::client::shell) fn unavailable_notice(location: &FileLocation) -> String {
    format!("not available inside {}", location.label())
}
impl ClientShellState {
    pub(in crate::client::shell) fn investigation_menu_action(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        // Why before the host-only guard below: downloading works in every
        // location, so it must not say "not available inside ...".
        if MENU.get(index) == Some(&DOWNLOAD_ITEM) {
            if let Some(tab) = self.investigation_mut() {
                tab.dialog = None;
            }
            self.investigation_download_prompt(outcome);
            return;
        }
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        tab.dialog = None;
        // Kept in the menu and on their keys, but never sent to a server that
        // would act on the host: the notice says where it does not work.
        if matches!(index, 6..=15 | 22) {
            if let Some(location) = tab.browser.location.as_ref() {
                tab.notice = unavailable_notice(location);
                outcome.repaint = true;
                return;
            }
        }
        match index {
            0 => {
                let workspace = self.workspace_action_id();
                self.open_file_viewer_for(workspace, outcome);
            }
            1 => {
                let root = tab.browser.root.clone().unwrap_or_default();
                tab.dialog = Some(Dialog::Input {
                    title: "root".into(),
                    input: TextEditor::new(&root, true),
                    purpose: InputPurpose::Root,
                    sources: Vec::new(),
                });
            }
            2 => tab.horizontal = !tab.horizontal,
            3 => {
                if let Some(path) = tab.paths().first() {
                    outcome
                        .actions
                        .push(ClientShellAction::ClipboardWrite(path.as_bytes().to_vec()));
                    tab.notice = format!("copied {path}");
                }
            }
            4 => {
                if let Some(path) = tab.paths().first().cloned() {
                    let boot = tab.boot.clone();
                    let target = self.file_viewer_send_target();
                    if let Some(tab) = self.investigation_mut() {
                        match target {
                            Ok(pane) => tab.dialog = Some(Dialog::Send { path, pane, boot }),
                            Err(notice) => tab.notice = notice,
                        }
                    }
                }
            }
            5 => self.investigation_open_editor(outcome),
            6 | 7 if tab.paths().is_empty() => {
                tab.notice = "select a file or folder first".into();
            }
            6 | 7 => {
                self.workbench.clipboard = Some(FileClipboard {
                    endpoint: tab.endpoint.clone(),
                    boot: tab.boot.clone(),
                    paths: tab.paths(),
                    cut: index == 7,
                });
            }
            8 => self.investigation_paste(outcome),
            9 => {
                if let Some(path) = tab.paths().first() {
                    tab.dialog = Some(Dialog::Input {
                        title: "rename / move to absolute path".into(),
                        input: TextEditor::new(path, true),
                        purpose: InputPurpose::Operation(FileOperation::Move),
                        sources: vec![path.clone()],
                    });
                }
            }
            10 | 11 => {
                let root = tab
                    .browser
                    .selected_entry()
                    .filter(|e| e.is_dir)
                    .map(|e| e.path.clone())
                    .or_else(|| tab.browser.root.clone())
                    .unwrap_or_default();
                tab.dialog = Some(Dialog::Input {
                    title: if index == 10 {
                        "new file: absolute path"
                    } else {
                        "new directory: absolute path"
                    }
                    .into(),
                    input: TextEditor::new(&format!("{}/", root.trim_end_matches('/')), false),
                    purpose: InputPurpose::Operation(if index == 10 {
                        FileOperation::CreateFile
                    } else {
                        FileOperation::CreateDirectory
                    }),
                    sources: Vec::new(),
                });
            }
            12 | 13 => {
                let paths = tab.paths();
                if !paths.is_empty() {
                    self.investigation_plan_operation(
                        operation_params(
                            if index == 12 {
                                FileOperation::Trash
                            } else {
                                FileOperation::Delete
                            },
                            paths,
                            None,
                        ),
                        outcome,
                    );
                }
            }
            14 => self.investigation_prepare_replace(outcome),
            15 => self.investigation_apply_replace(true, outcome),
            16 => {
                tab.regex = !tab.regex;
                tab.schedule_search();
            }
            17 => {
                tab.case_sensitive = !tab.case_sensitive;
                tab.schedule_search();
            }
            18 => {
                tab.whole_word = !tab.whole_word;
                tab.schedule_search();
            }
            19 => {
                tab.ignored = !tab.ignored;
                tab.schedule_search();
            }
            20 => {
                if let Some(index) = self.workbench.active {
                    self.workbench.tabs.remove(index);
                    self.workbench.active = None;
                    self.workbench.visible = false;
                }
            }
            21 => {
                if let Some(found) = tab.matches.get(tab.selected_match) {
                    let indexes: Vec<_> = tab
                        .matches
                        .iter()
                        .enumerate()
                        .filter(|(_, m)| m.path == found.path)
                        .map(|(i, _)| i)
                        .collect();
                    let include = indexes.iter().all(|i| tab.omitted.contains(i));
                    for index in indexes {
                        if include {
                            tab.omitted.remove(&index);
                        } else {
                            tab.omitted.insert(index);
                        }
                    }
                }
            }
            22 => tab.schedule_search(),
            23 => self.refresh_investigation_files(),
            24 => match tab.root_candidate() {
                Some(root) => {
                    self.change_file_viewer_root(root, None, outcome);
                }
                None => tab.notice = "select a file or folder to open as root".into(),
            },
            _ => {}
        }
        self.persist_chrome_preferences(outcome);
        outcome.repaint = true;
    }
    /// Asks where on this machine to save the selected entries (D1). The
    /// checks are the ones `start_download` repeats, so a refusal reads the
    /// same wherever it comes from.
    pub(in crate::client::shell) fn investigation_download_prompt(
        &mut self,
        outcome: &mut ClientShellInput,
    ) {
        outcome.repaint = true;
        let folder = self.download_dir.clone();
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        let paths = tab.paths();
        if paths.is_empty() {
            return;
        }
        if let Err(refusal) = crate::download::local::check_selection(&paths) {
            tab.notice = refusal;
            return;
        }
        tab.notice = if folder.is_empty() {
            "enter a folder on this machine".into()
        } else {
            String::new()
        };
        tab.dialog = Some(Dialog::Input {
            title: format!("download {} item(s) to (this machine):", paths.len()),
            input: TextEditor::new(&folder, true),
            purpose: InputPurpose::Download,
            sources: paths,
        });
    }
    pub(in crate::client::shell) fn investigation_paste(
        &mut self,
        _outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation() else {
            return;
        };
        if let Some(location) = tab.browser.location.clone() {
            if let Some(tab) = self.investigation_mut() {
                tab.notice = unavailable_notice(&location);
            }
            return;
        }
        let Some(clipboard) = self.workbench.clipboard.as_ref() else {
            return;
        };
        if clipboard.endpoint != tab.endpoint || clipboard.boot != tab.boot {
            if let Some(tab) = self.investigation_mut() {
                tab.notice =
                    "copy/move is limited to the same host connection; select the source again"
                        .into();
            }
            return;
        }
        let root = tab
            .browser
            .selected_entry()
            .filter(|e| e.is_dir)
            .map(|e| e.path.clone())
            .or_else(|| tab.browser.root.clone())
            .unwrap_or_default();
        let sources = clipboard.paths.clone();
        let operation = if clipboard.cut {
            FileOperation::Move
        } else {
            FileOperation::Copy
        };
        if let Some(tab) = self.investigation_mut() {
            tab.dialog = Some(Dialog::Input {
                title: "paste destination: absolute directory".into(),
                input: TextEditor::new(&root, true),
                purpose: InputPurpose::Operation(operation),
                sources,
            });
        }
    }
    pub(in crate::client::shell) fn investigation_plan_operation(
        &mut self,
        params: FilesOperateParams,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if let Some(location) = tab.browser.location.as_ref() {
            tab.notice = unavailable_notice(location);
            return;
        }
        if tab.operation_pending {
            return;
        }
        let id = tab.id;
        tab.operation_pending = true;
        let sent = self.push_endpoint_method_with_kind(
            Method::FilesPlan(params.clone()),
            PendingEndpointKind::Investigation(FileRequest::Plan { id, params }),
            outcome,
        );
        if !sent {
            if let Some(tab) = self.investigation_mut() {
                tab.operation_pending = false;
            }
        }
    }
    pub(in crate::client::shell) fn investigation_execute_operation(
        &mut self,
        mut params: FilesOperateParams,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if let Some(location) = tab.browser.location.as_ref() {
            tab.notice = unavailable_notice(location);
            return;
        }
        if tab.operation_pending {
            return;
        }
        params.confirmed = true;
        let id = tab.id;
        tab.operation_pending = true;
        tab.dialog = None;
        let sent = self.push_endpoint_method_with_kind(
            Method::FilesOperate(params),
            PendingEndpointKind::Investigation(FileRequest::Operate { id }),
            outcome,
        );
        if !sent {
            if let Some(tab) = self.investigation_mut() {
                tab.operation_pending = false;
            }
        }
    }
    pub(in crate::client::shell) fn investigation_prepare_replace(
        &mut self,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if let Some(location) = tab.browser.location.as_ref() {
            tab.notice = unavailable_notice(location);
            return;
        }
        if !tab.search_ready || tab.search_due.is_some() || tab.searching || tab.operation_pending {
            tab.notice = "wait for a current search before replacing".into();
            return;
        }
        let paths: std::collections::BTreeSet<_> = tab
            .matches
            .iter()
            .enumerate()
            .filter(|(i, _)| !tab.omitted.contains(i))
            .map(|(_, m)| m.path.clone())
            .collect();
        if paths.len() > 1000 {
            tab.notice = "narrow the search to at most 1000 files".into();
            return;
        }
        tab.plan.clear();
        tab.dialog = Some(Dialog::Replace { scroll: 0 });
        tab.prepare_queue = paths.into_iter().collect();
        tab.prepare_pending = 0;
        tab.prepare_generation = tab.prepare_generation.wrapping_add(1);
        self.pump_investigation_prepare(outcome);
    }
    pub(in crate::client::shell) fn investigation_apply_replace(
        &mut self,
        undo: bool,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if let Some(location) = tab.browser.location.as_ref() {
            tab.notice = unavailable_notice(location);
            return;
        }
        if tab.operation_pending || tab.prepare_pending > 0 || !tab.prepare_queue.is_empty() {
            return;
        }
        let changes = if undo {
            tab.undo.clone()
        } else {
            tab.plan.iter().map(|p| p.after.clone()).collect()
        };
        if changes.is_empty() {
            tab.notice = "no changes to apply".into();
            return;
        }
        let originals = if undo {
            Vec::new()
        } else {
            tab.plan.iter().map(|p| p.before.clone()).collect()
        };
        let id = tab.id;
        tab.operation_pending = true;
        let sent = self.push_endpoint_method_with_kind(
            Method::FilesReplace(FilesReplaceParams { changes }),
            PendingEndpointKind::Investigation(FileRequest::Replace {
                id,
                originals,
                undo,
            }),
            outcome,
        );
        if !sent {
            if let Some(tab) = self.investigation_mut() {
                tab.operation_pending = false;
            }
        }
    }
    fn investigation_open_editor(&mut self, outcome: &mut ClientShellInput) {
        if self.workbench.editor_opening {
            return;
        }
        let Some(tab) = self.investigation() else {
            return;
        };
        let Some((path, _)) = tab.target() else {
            return;
        };
        let id = tab.id;
        let method = if tab.browser.location.is_some() {
            // Only the focused pane can host the overlay, and it must be in the
            // tab's location: the editor opens there.
            let pane_id = match self.file_viewer_send_target() {
                Ok(pane_id) => pane_id,
                Err(notice) => {
                    if let Some(tab) = self.investigation_mut() {
                        tab.notice = notice;
                    }
                    return;
                }
            };
            Method::PaneDirectoryOpenEditor(PaneDirectoryOpenEditorParams { pane_id, path })
        } else {
            Method::PaneOpenEditor(PaneOpenEditorParams {
                workspace_id: tab.browser.workspace_id.clone(),
                pane_id: self.focused_pane_id(),
                path,
            })
        };
        self.workbench.editor_opening = true;
        let sent = self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::Investigation(FileRequest::Editor { id }),
            outcome,
        );
        if !sent {
            self.workbench.editor_opening = false;
            self.workbench.terminal_requested = false;
        }
    }
}
