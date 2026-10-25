//! Files dropped onto the workbench tree (spec Q1, Q4, Q18, Q22; design §4).
//!
//! The client loop decides that a paste is a drop and carries the bytes; this
//! side names the destination, asks how to treat existing names, and lists
//! the folder again afterwards.

use super::*;
use crate::api::schema::*;
use crate::client::shell::file_viewer::model::FileLocation;
use crate::client::shell::{ClientDownloadRequest, ClientUploadDecision};

/// How a tree drop reaches the tab's filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkbenchDropPlace {
    /// The Herdr server's own filesystem.
    Host,
    /// An ssh/container location, through this live pane of it.
    Pane(String),
    /// An ssh/container location with no live pane left: the notice to show.
    NoPane(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkbenchDropTarget {
    pub(crate) tab: u64,
    /// The selected folder, the selected file's folder, or the root (Q18).
    pub(crate) destination: String,
    pub(crate) place: WorkbenchDropPlace,
    /// The environment the tab browses, to compare with where the server
    /// finds the pane when the upload begins.
    pub(crate) location: PaneLocation,
}

/// What the tree dialog asks once an upload's session exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientUploadQuestion {
    pub(crate) job: u64,
    pub(crate) destination: String,
    pub(crate) summary: String,
    pub(crate) conflicts: Vec<String>,
    pub(crate) note: Option<String>,
}

impl ClientShellState {
    /// Where a drop onto the shown tree would go, or `None` when the tree is
    /// not what receives pastes (a text field, a dialog, search results).
    pub(crate) fn workbench_drop_target(&self) -> Option<WorkbenchDropTarget> {
        // Why focus rather than the pointer: a drop arrives as pasted text
        // with no position, so beside a focused terminal it is the
        // terminal's, like any other paste.
        if self.overlay.is_some() || !self.files_has_focus() {
            return None;
        }
        let tab = self.investigation()?;
        if tab.dialog.is_some() || tab.focus != Focus::Results || tab.content_search {
            return None;
        }
        let root = tab.browser.root.clone()?;
        let destination = match tab.browser.selected_entry() {
            Some(entry) if entry.is_dir => entry.path.clone(),
            Some(entry) => tab
                .browser
                .parent_path(&entry.path)
                .map_or(root, str::to_owned),
            None => root,
        };
        let place = self.workbench_place(tab);
        Some(WorkbenchDropTarget {
            tab: tab.id,
            destination,
            place,
            location: FileLocation::to_pane_location(tab.browser.location.as_ref()),
        })
    }

    /// Where this tab's files are read and written: the server's disk, or a
    /// live pane of its ssh/container location.
    pub(in crate::client::shell) fn workbench_place(
        &self,
        tab: &Investigation,
    ) -> WorkbenchDropPlace {
        match tab.browser.location.as_ref() {
            None => WorkbenchDropPlace::Host,
            Some(location) => match self.file_location_pane(
                tab.browser.workspace_id.as_deref(),
                location,
                tab.browser.location_pane.as_deref(),
            ) {
                Some(pane_id) => WorkbenchDropPlace::Pane(pane_id),
                None => WorkbenchDropPlace::NoPane(Self::no_pane_notice(location)),
            },
        }
    }

    /// Hands the dialog's answer to the client loop. The destination stays as
    /// typed: `~` and the platform's path syntax are resolved where the files
    /// are written.
    pub(in crate::client::shell) fn queue_download(
        &mut self,
        paths: Vec<String>,
        destination: String,
    ) {
        let Some(tab) = self.investigation() else {
            return;
        };
        let request = ClientDownloadRequest {
            tab: tab.id,
            endpoint_id: tab.endpoint.clone(),
            place: self.workbench_place(tab),
            location: FileLocation::to_pane_location(tab.browser.location.as_ref()),
            paths,
            destination,
        };
        self.download_requests.push(request);
        if let Some(tab) = self.investigation_mut() {
            tab.notice = "preparing download…".into();
        }
    }

    /// Shows an upload's question in its tab. Why refuse when another dialog
    /// is open: the user is in the middle of it, and a dialog they did not
    /// open could take a keystroke meant for theirs.
    pub(crate) fn open_workbench_upload_dialog(
        &mut self,
        tab: u64,
        question: ClientUploadQuestion,
    ) -> bool {
        let Some(investigation) = self
            .workbench
            .tabs
            .iter_mut()
            .find(|t| t.id == tab && t.dialog.is_none())
        else {
            return false;
        };
        investigation.dialog = Some(Dialog::Upload(UploadDialog {
            job: question.job,
            destination: question.destination,
            summary: question.summary,
            ask_collision: question.note.is_some(),
            conflicts: question.conflicts,
            note: question.note,
            selected: 0,
        }));
        investigation.notice.clear();
        self.workbench.asking_uploads.push(question.job);
        // The upload runtime repaints on `true`, so the outcome is not needed.
        self.close_help_over_workbench_dialog(&mut ClientShellInput::default());
        true
    }

    /// The job ended without an answer (connection lost, server refusal).
    pub(in crate::client::shell) fn close_workbench_upload_dialog(&mut self, job: u64) -> bool {
        self.workbench
            .asking_uploads
            .retain(|asking| *asking != job);
        let mut closed = false;
        for tab in &mut self.workbench.tabs {
            if matches!(&tab.dialog, Some(Dialog::Upload(UploadDialog { job: asked, .. })) if *asked == job)
            {
                tab.dialog = None;
                closed = true;
            }
        }
        closed
    }

    /// Cancels every asked upload whose dialog is gone without an answer:
    /// Esc, a closed tab, or a server restart. Without this the job would
    /// wait forever and block every later drop.
    pub(in crate::client::shell) fn sweep_workbench_upload_dialogs(
        &mut self,
    ) -> Vec<ClientUploadDecision> {
        let tabs = &self.workbench.tabs;
        let held = |job: u64| {
            tabs.iter().any(|tab| {
                matches!(&tab.dialog, Some(Dialog::Upload(UploadDialog { job: asked, .. })) if *asked == job)
            })
        };
        let (kept, dropped): (Vec<u64>, Vec<u64>) = self
            .workbench
            .asking_uploads
            .iter()
            .partition(|job| held(**job));
        self.workbench.asking_uploads = kept;
        dropped
            .into_iter()
            .map(|job| ClientUploadDecision::Collision {
                job,
                collision: None,
            })
            .collect()
    }

    pub(in crate::client::shell) fn answer_upload_dialog(
        &mut self,
        choice: UploadChoice,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        let Some(Dialog::Upload(dialog)) = tab.dialog.take() else {
            return;
        };
        outcome.repaint = true;
        let job = dialog.job;
        // Upload without conflicts publishes with `fail`, so a name created
        // meanwhile asks again instead of renaming silently.
        let collision = match choice {
            UploadChoice::Upload => Some(PaneUploadCollision::Fail),
            UploadChoice::Replace => Some(PaneUploadCollision::Replace),
            UploadChoice::KeepBoth => Some(PaneUploadCollision::Rename),
            UploadChoice::Cancel => None,
        };
        if collision.is_some() {
            tab.notice = "uploading…".into();
        }
        self.workbench
            .asking_uploads
            .retain(|asking| *asking != job);
        self.upload_decisions
            .push(ClientUploadDecision::Collision { job, collision });
    }

    /// Lists the folder again on the next tick and selects the first name.
    pub(crate) fn workbench_upload_done(
        &mut self,
        tab: u64,
        destination: String,
        names: Vec<String>,
    ) -> bool {
        let Some(investigation) = self.workbench.tabs.iter_mut().find(|t| t.id == tab) else {
            return false;
        };
        // Tree paths are `/`-separated whatever the OS.
        let select = names
            .first()
            .map(|name| format!("{}/{name}", destination.trim_end_matches('/')));
        investigation.upload_refresh = Some((destination, select));
        true
    }

    pub(crate) fn set_workbench_notice(&mut self, tab: u64, text: String) -> bool {
        let Some(investigation) = self.workbench.tabs.iter_mut().find(|t| t.id == tab) else {
            return false;
        };
        investigation.notice = text;
        true
    }

    /// Runs from the tick: only the shown tab can send its listing.
    pub(in crate::client::shell) fn refresh_after_upload(
        &mut self,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        let Some((destination, select)) = tab.upload_refresh.take() else {
            return;
        };
        if tab.browser.root.as_deref() != Some(destination.as_str()) {
            tab.browser.expanded.insert(destination.clone());
        }
        tab.browser.pending_select = select;
        self.request_file_viewer_listing(destination, outcome);
        outcome.repaint = true;
    }
}

// Why Unix only: the end-to-end runtime test needs Unix scratch trees.
#[cfg(all(test, unix))]
impl ClientShellState {
    /// A host tab browsing `root`, for tests outside the shell; returns its id.
    /// The endpoint gets a snapshot so the tab can send requests.
    pub(crate) fn push_test_investigation(&mut self, root: &str) -> u64 {
        self.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
        use crate::client::shell::file_viewer::model::FileViewerOptions;
        let mut browser = ClientFileViewerOverlay::new(FileViewerOptions {
            generation: 1,
            workspace_id: None,
            show_hidden: false,
            show_ignored: false,
            index_supported: false,
            location: None,
        });
        browser.root = Some(root.into());
        let tab = Investigation::new(browser, self.active_endpoint_id.clone(), String::new());
        let id = tab.id;
        self.workbench.tabs.push(tab);
        id
    }

    /// The job and conflicts of the tab's upload question, if one is shown.
    pub(crate) fn workbench_question_for_test(&self, tab: u64) -> Option<(u64, Vec<String>)> {
        let investigation = self.workbench.tabs.iter().find(|t| t.id == tab)?;
        match &investigation.dialog {
            Some(Dialog::Upload(UploadDialog { job, conflicts, .. })) => {
                Some((*job, conflicts.clone()))
            }
            _ => None,
        }
    }

    pub(crate) fn workbench_notice_for_test(&self, tab: u64) -> Option<&str> {
        let investigation = self.workbench.tabs.iter().find(|t| t.id == tab)?;
        Some(investigation.notice.as_str())
    }

    pub(crate) fn workbench_refresh_for_test(&self, tab: u64) -> Option<&(String, Option<String>)> {
        let investigation = self.workbench.tabs.iter().find(|t| t.id == tab)?;
        investigation.upload_refresh.as_ref()
    }
}
