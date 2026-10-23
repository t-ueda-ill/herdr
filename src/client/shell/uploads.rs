//! Shell presentation of dropped-file uploads: the progress card, the confirm
//! dialog, and notices. The transfer itself lives in `client::upload`; the
//! shell only queues what the user decided for the client loop to apply.

use super::*;
use crate::client::upload::transfer::{JobId, UploadProgressView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientUploadDecision {
    Cancel(JobId),
    Confirm {
        job: JobId,
        accept: bool,
    },
    /// The tree dialog's answer; `None` cancels (spec Q4).
    Collision {
        job: JobId,
        collision: Option<crate::api::schema::PaneUploadCollision>,
    },
}

/// The workbench's dialog asked to save these paths on this machine. The client
/// loop starts the download (design §5.2, §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientDownloadRequest {
    /// The investigation tab, which shows the outcome too.
    pub(crate) tab: u64,
    pub(crate) endpoint_id: crate::client::endpoint::ClientEndpointId,
    /// Where the files are read: the server's disk, or a pane's environment.
    pub(crate) place: WorkbenchDropPlace,
    /// The environment the tab browses.
    pub(crate) location: crate::api::schema::PaneLocation,
    pub(crate) paths: Vec<String>,
    /// The dialog's text, as typed. Resolving it is the worker's job.
    pub(crate) destination: String,
}

impl ClientShellState {
    /// Where the dialog starts, worked out once when the client starts.
    pub(crate) fn set_download_dir(&mut self, dir: String) {
        self.download_dir = dir;
    }

    pub(crate) fn download_dir(&self) -> &str {
        &self.download_dir
    }

    pub(crate) fn take_download_requests(&mut self) -> Vec<ClientDownloadRequest> {
        std::mem::take(&mut self.download_requests)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn queue_download_for_test(&mut self, request: ClientDownloadRequest) {
        self.download_requests.push(request);
    }

    /// Why `Uploaded`/`UploadFailed` and not kinds of their own: a kind only
    /// picks the color and the group, and these two already repeat.
    pub(crate) fn push_download_notice(
        &mut self,
        failed: bool,
        title: String,
        body: String,
    ) -> bool {
        let kind = if failed {
            ClientEndpointNoticeKind::UploadFailed
        } else {
            ClientEndpointNoticeKind::Uploaded
        };
        self.push_endpoint_notice(kind, "download", title, body)
    }

    /// Returns whether the card changed and needs a repaint.
    pub(crate) fn set_upload_progress(&mut self, view: Option<UploadProgressView>) -> bool {
        if self.upload_progress == view {
            return false;
        }
        self.upload_progress = view;
        true
    }

    /// Opens the confirm dialog, unless another overlay is in the way.
    /// Why not replace it: the user is in the middle of something else, and a
    /// dialog they did not open could take a keystroke meant for theirs.
    pub(crate) fn open_upload_confirm(
        &mut self,
        job: JobId,
        title: String,
        detail: String,
    ) -> bool {
        if self.overlay.is_some() {
            return false;
        }
        self.overlay = Some(ClientShellOverlay::UploadConfirm(
            ClientUploadConfirmOverlay { job, title, detail },
        ));
        self.confirming_upload = Some(job);
        true
    }

    /// Closes the job's question, the threshold overlay or a tree dialog.
    pub(crate) fn close_upload_confirm(&mut self, job: JobId) -> bool {
        if self.confirming_upload == Some(job) {
            self.confirming_upload = None;
        }
        if matches!(&self.overlay, Some(ClientShellOverlay::UploadConfirm(confirm)) if confirm.job == job)
        {
            self.overlay = None;
            return true;
        }
        self.close_workbench_upload_dialog(job)
    }

    pub(crate) fn push_upload_notice(&mut self, failed: bool, title: String, body: String) -> bool {
        let kind = if failed {
            ClientEndpointNoticeKind::UploadFailed
        } else {
            ClientEndpointNoticeKind::Uploaded
        };
        self.push_endpoint_notice(kind, "upload", title, body)
    }

    pub(crate) fn take_upload_decisions(&mut self) -> Vec<ClientUploadDecision> {
        let swept = self.sweep_workbench_upload_dialogs();
        self.upload_decisions.extend(swept);
        if let Some(declined) = self.sweep_upload_confirm() {
            self.upload_decisions.push(declined);
        }
        std::mem::take(&mut self.upload_decisions)
    }

    /// Declines a confirm dialog that another overlay replaced (a popup
    /// opening, an endpoint reset). Why not guard every assignment of the
    /// overlay: there are many, and one missed would block every later drop.
    fn sweep_upload_confirm(&mut self) -> Option<ClientUploadDecision> {
        let job = self.confirming_upload?;
        if matches!(&self.overlay, Some(ClientShellOverlay::UploadConfirm(confirm)) if confirm.job == job)
        {
            return None;
        }
        self.confirming_upload = None;
        Some(ClientUploadDecision::Confirm { job, accept: false })
    }

    /// Enter or the primary button accepts; Esc or a click elsewhere declines.
    pub(super) fn answer_upload_confirm(&mut self, accept: bool, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::UploadConfirm(confirm)) = self.overlay.take() else {
            return;
        };
        self.confirming_upload = None;
        self.upload_decisions.push(ClientUploadDecision::Confirm {
            job: confirm.job,
            accept,
        });
        outcome.repaint = true;
    }

    /// A click on the progress card cancels, except while publishing.
    pub(super) fn click_upload_progress(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(progress) = self.upload_progress.as_ref() else {
            return false;
        };
        if !super::contains(self.hits.upload_progress, point) {
            return false;
        }
        if progress.cancellable {
            self.upload_decisions
                .push(ClientUploadDecision::Cancel(progress.job));
            outcome.repaint = true;
        }
        true
    }

    #[cfg(all(test, unix))]
    pub(crate) fn upload_progress_for_test(&self) -> Option<&UploadProgressView> {
        self.upload_progress.as_ref()
    }

    /// The visible notice's title and body.
    #[cfg(all(test, unix))]
    pub(crate) fn endpoint_notice_for_test(&self) -> Option<(&str, &str)> {
        self.visible_endpoint_notice
            .as_ref()
            .map(|notice| (notice.title.as_str(), notice.body.as_str()))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn shell() -> ClientShellState {
        ClientShellState::new(ClientShellConfig::from_config(
            &crate::config::Config::default(),
        ))
    }

    fn request(destination: &str) -> ClientDownloadRequest {
        ClientDownloadRequest {
            tab: 1,
            endpoint_id: crate::client::endpoint::ClientEndpointId::Local,
            place: WorkbenchDropPlace::Host,
            location: crate::api::schema::PaneLocation {
                kind: crate::api::schema::PaneLocationKind::Host,
                ssh_host: None,
                container: None,
            },
            paths: vec!["/srv/a".into()],
            destination: destination.into(),
        }
    }

    #[test]
    fn queued_download_requests_are_handed_over_once_in_order() {
        let mut shell = shell();
        shell.queue_download_for_test(request("/a"));
        shell.queue_download_for_test(request("/b"));

        let taken = shell.take_download_requests();

        assert_eq!(
            taken
                .iter()
                .map(|r| r.destination.as_str())
                .collect::<Vec<_>>(),
            ["/a", "/b"]
        );
        assert!(shell.take_download_requests().is_empty());
    }

    #[test]
    fn the_dialogs_starting_folder_is_what_the_client_set() {
        let mut shell = shell();
        assert_eq!(shell.download_dir(), "");

        shell.set_download_dir("/home/me/Downloads".into());

        assert_eq!(shell.download_dir(), "/home/me/Downloads");
    }

    #[test]
    fn every_download_is_announced_not_just_the_first() {
        let mut shell = shell();

        assert!(shell.push_download_notice(false, "Downloaded".into(), "1 item to /a".into()));
        assert!(shell.push_download_notice(false, "Downloaded".into(), "1 item to /b".into()));
        assert_eq!(
            shell.endpoint_notice_for_test(),
            Some(("Downloaded", "1 item to /b"))
        );
    }
}
