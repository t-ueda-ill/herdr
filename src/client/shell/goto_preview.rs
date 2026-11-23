//! The Go to picker's preview: what each pane was asked and what it shows
//! or left, read from its server while the picker is open.

use std::time::{Duration, Instant};

use super::endpoint_queries::EndpointQueryKind;
use super::*;
use crate::api::schema::{
    Method, PaneReadParams, ReadFormat, ReadSource, ResponseResult, TaskInfo, TaskListParams,
    TaskResultSource, TaskTarget,
};

/// How long a screen read or a task list stays current.
pub(super) const PREVIEW_TTL: Duration = Duration::from_secs(2);
/// Screen lines read for the preview.
const PREVIEW_LINES: u32 = 20;
/// Screen lines read for the full view.
pub(super) const VIEWER_LINES: u32 = 500;

/// One read that fills a pane's preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GotoPreviewRead {
    /// `pane.read` of a pane with a task in progress or no task.
    Screen { pane_id: String, lines: u32 },
    /// `task.result` of the pane's last finished task.
    Result { pane_id: String, task_id: String },
}

impl GotoPreviewRead {
    fn pane_id(&self) -> &str {
        match self {
            Self::Screen { pane_id, .. } | Self::Result { pane_id, .. } => pane_id,
        }
    }

    fn task_id(&self) -> Option<&str> {
        match self {
            Self::Screen { .. } => None,
            Self::Result { task_id, .. } => Some(task_id),
        }
    }

    fn method(&self) -> Method {
        match self {
            Self::Screen { pane_id, lines } => Method::PaneRead(PaneReadParams {
                pane_id: pane_id.clone(),
                // Why detection: recent would make the server scroll an
                // idle agent's alternate screen to collect history, and
                // visible follows the pane's scroll offset, not its output.
                source: ReadSource::Detection,
                lines: Some(*lines),
                format: ReadFormat::Text,
                strip_ansi: true,
                intent: Default::default(),
            }),
            Self::Result { task_id, .. } => Method::TaskResult(TaskTarget {
                task_id: task_id.clone(),
            }),
        }
    }
}

/// Tasks and previews the picker read, kept while it is open.
#[derive(Debug, Default)]
pub(super) struct GotoTasks {
    lists: HashMap<ClientEndpointId, GotoTaskList>,
    previews: HashMap<(ClientEndpointId, String), GotoPreview>,
}

#[derive(Debug)]
struct GotoTaskList {
    /// When it was sent, then when it was answered. Why the answer too: a
    /// slow server would otherwise look stale on arrival and keep the
    /// single query slot from the other read.
    fresh_at: Instant,
    pending: bool,
    /// `None` until the first answer.
    by_pane: Option<HashMap<String, TaskInfo>>,
}

#[derive(Clone, Debug)]
pub(super) struct GotoPreview {
    /// When it was sent, then when it was answered, as for the list.
    fresh_at: Instant,
    pending: bool,
    /// The finished task the text came from. Why kept: a result does not
    /// change, so it is not read again.
    task_id: Option<String>,
    pub(super) body: PreviewBody,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PreviewBody {
    Loading,
    Text {
        source: PreviewSource,
        text: String,
        /// Why the result file was not used.
        note: Option<String>,
        /// The text is the start of a longer result.
        truncated: bool,
    },
    /// The server is offline, too old, or answered with an error.
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PreviewSource {
    ResultFile,
    ScreenTail,
}

impl GotoPreview {
    pub(super) fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }
}

impl GotoTasks {
    /// The task the preview shows for a pane: the one in progress, else the
    /// last one that finished.
    pub(super) fn task(&self, endpoint_id: &ClientEndpointId, pane_id: &str) -> Option<&TaskInfo> {
        self.lists.get(endpoint_id)?.by_pane.as_ref()?.get(pane_id)
    }

    pub(super) fn preview(
        &self,
        endpoint_id: &ClientEndpointId,
        pane_id: &str,
    ) -> Option<&GotoPreview> {
        self.previews
            .get(&(endpoint_id.clone(), pane_id.to_owned()))
    }

    /// Whether the list for this server answered at least once.
    pub(super) fn listed(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.lists
            .get(endpoint_id)
            .is_some_and(|list| list.by_pane.is_some())
    }

    pub(super) fn list_due(&self, endpoint_id: &ClientEndpointId, now: Instant) -> bool {
        self.lists.get(endpoint_id).is_none_or(|list| {
            !list.pending && now.saturating_duration_since(list.fresh_at) >= PREVIEW_TTL
        })
    }

    fn preview_due(
        &self,
        endpoint_id: &ClientEndpointId,
        read: &GotoPreviewRead,
        now: Instant,
    ) -> bool {
        let Some(preview) = self.preview(endpoint_id, read.pane_id()) else {
            return true;
        };
        if preview.pending {
            return false;
        }
        let expired = now.saturating_duration_since(preview.fresh_at) >= PREVIEW_TTL;
        match read.task_id() {
            Some(task_id) => {
                preview.task_id.as_deref() != Some(task_id)
                    || matches!(preview.body, PreviewBody::Unavailable(_)) && expired
            }
            None => preview.task_id.is_some() || expired,
        }
    }

    pub(super) fn list_sent(&mut self, endpoint_id: &ClientEndpointId, now: Instant) {
        let list = self
            .lists
            .entry(endpoint_id.clone())
            .or_insert(GotoTaskList {
                fresh_at: now,
                pending: true,
                by_pane: None,
            });
        list.fresh_at = now;
        list.pending = true;
    }

    /// A server without `task.list` has no tasks to show, only screens.
    fn list_missing(&mut self, endpoint_id: &ClientEndpointId, now: Instant) {
        self.lists.insert(
            endpoint_id.clone(),
            GotoTaskList {
                fresh_at: now,
                pending: false,
                by_pane: Some(HashMap::new()),
            },
        );
    }

    fn preview_sent(
        &mut self,
        endpoint_id: &ClientEndpointId,
        read: &GotoPreviewRead,
        now: Instant,
    ) {
        let preview = self
            .previews
            .entry((endpoint_id.clone(), read.pane_id().to_owned()))
            .or_insert(GotoPreview {
                fresh_at: now,
                pending: true,
                task_id: None,
                body: PreviewBody::Loading,
            });
        // Why the old text stays: a screen read every two seconds must not
        // blink through a loading state.
        if preview.task_id.as_deref() != read.task_id() {
            preview.body = PreviewBody::Loading;
        }
        preview.fresh_at = now;
        preview.pending = true;
        preview.task_id = read.task_id().map(str::to_owned);
    }

    fn preview_unavailable(
        &mut self,
        endpoint_id: &ClientEndpointId,
        pane_id: &str,
        now: Instant,
        message: &str,
    ) {
        self.previews.insert(
            (endpoint_id.clone(), pane_id.to_owned()),
            GotoPreview {
                fresh_at: now,
                pending: false,
                task_id: None,
                body: PreviewBody::Unavailable(message.to_owned()),
            },
        );
    }

    pub(super) fn receive_list(
        &mut self,
        endpoint_id: &ClientEndpointId,
        tasks: Option<Vec<TaskInfo>>,
        now: Instant,
    ) {
        let Some(list) = self.lists.get_mut(endpoint_id) else {
            return;
        };
        list.pending = false;
        list.fresh_at = now;
        match tasks {
            Some(tasks) => list.by_pane = Some(newest_task_by_pane(tasks)),
            // Why an empty list on failure: the screens still show.
            None => {
                list.by_pane.get_or_insert_with(HashMap::new);
            }
        }
    }

    pub(super) fn receive_preview(
        &mut self,
        endpoint_id: &ClientEndpointId,
        read: &GotoPreviewRead,
        body: PreviewBody,
        now: Instant,
    ) {
        let Some(preview) = self
            .previews
            .get_mut(&(endpoint_id.clone(), read.pane_id().to_owned()))
        else {
            return;
        };
        if preview.task_id.as_deref() != read.task_id() {
            return;
        }
        preview.pending = false;
        preview.fresh_at = now;
        preview.body = body;
    }
}

/// The task in progress with the highest number, else the finished task
/// that ended last.
fn newest_task_by_pane(tasks: Vec<TaskInfo>) -> HashMap<String, TaskInfo> {
    let rank = |task: &TaskInfo| {
        let number = task
            .task_id
            .strip_prefix("t_")
            .and_then(|number| number.parse::<u64>().ok())
            .unwrap_or_default();
        // Why three groups: a queued task is not sent yet, so the one the
        // agent works on, or waits on input for, is the one to show.
        let group = match task.state {
            state if state.is_finished() => 0,
            crate::api::schema::TaskState::Queued => 1,
            _ => 2,
        };
        (group, task.finished_at_ms.unwrap_or_default(), number)
    };
    let mut by_pane: HashMap<String, TaskInfo> = HashMap::new();
    for task in tasks {
        let Some(pane_id) = task.pane_id.clone() else {
            continue;
        };
        match by_pane.get(&pane_id) {
            Some(kept) if rank(kept) >= rank(&task) => {}
            _ => {
                by_pane.insert(pane_id, task);
            }
        }
    }
    by_pane
}

impl ClientShellState {
    fn goto_tasks_mut(&mut self) -> Option<&mut GotoTasks> {
        match self.overlay.as_mut()? {
            ClientShellOverlay::Navigator(navigator) => Some(&mut navigator.tasks),
            ClientShellOverlay::GotoResult(viewer) => Some(&mut viewer.navigator.tasks),
            _ => None,
        }
    }

    /// Sends the reads the picker needs now: each server's task list once,
    /// then the selected pane's preview, then a fresh list for its server.
    /// Called after picker input and on the timer, never while drawing.
    pub(crate) fn sync_goto_preview(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        self.sync_goto_result(now, outcome);
        // Why before taking the tasks: search matches their text.
        let selected = {
            let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_ref() else {
                return;
            };
            let rows = super::aggregate_navigation::navigator_rows(
                &self.endpoints,
                &self.active_endpoint_id,
                navigator,
            );
            super::aggregate_navigation::selected_navigator_target(&rows, navigator)
        };
        let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() else {
            return;
        };
        // Why taken out: the queries below borrow the whole shell.
        let mut tasks = std::mem::take(&mut navigator.tasks);

        let online = self
            .endpoints
            .iter()
            .filter(|endpoint| self.endpoint_is_online(&endpoint.endpoint_id))
            .map(|endpoint| endpoint.endpoint_id.clone())
            .collect::<Vec<_>>();
        for endpoint_id in &online {
            if tasks.lists.contains_key(endpoint_id) {
                continue;
            }
            if !self.endpoint_advertises(endpoint_id, "task.list") {
                tasks.list_missing(endpoint_id, now);
            } else if self.query_endpoint(
                endpoint_id,
                Method::TaskList(TaskListParams::default()),
                EndpointQueryKind::TaskList,
                outcome,
            ) {
                tasks.list_sent(endpoint_id, now);
            }
        }

        if let Some(ClientNavigatorTarget::Pane {
            endpoint_id,
            pane_id,
        }) = selected
        {
            if online.contains(&endpoint_id) && tasks.listed(&endpoint_id) {
                let read = match tasks.task(&endpoint_id, &pane_id) {
                    Some(task) if task.state.is_finished() => GotoPreviewRead::Result {
                        pane_id: pane_id.clone(),
                        task_id: task.task_id.clone(),
                    },
                    _ => GotoPreviewRead::Screen {
                        pane_id: pane_id.clone(),
                        lines: PREVIEW_LINES,
                    },
                };
                let method = read.method();
                if !self.endpoint_advertises(&endpoint_id, crate::api::api_method_name(&method)) {
                    if tasks.preview(&endpoint_id, &pane_id).is_none() {
                        outcome.repaint = true;
                        tasks.preview_unavailable(
                            &endpoint_id,
                            &pane_id,
                            now,
                            "not available on this server",
                        );
                    }
                } else if tasks.preview_due(&endpoint_id, &read, now)
                    && self.query_endpoint(
                        &endpoint_id,
                        method,
                        EndpointQueryKind::Preview(read.clone()),
                        outcome,
                    )
                {
                    tasks.preview_sent(&endpoint_id, &read, now);
                }
            }
            if online.contains(&endpoint_id)
                && self.endpoint_advertises(&endpoint_id, "task.list")
                && tasks.list_due(&endpoint_id, now)
                && self.query_endpoint(
                    &endpoint_id,
                    Method::TaskList(TaskListParams::default()),
                    EndpointQueryKind::TaskList,
                    outcome,
                )
            {
                tasks.list_sent(&endpoint_id, now);
            }
        }

        if let Some(goto) = self.goto_tasks_mut() {
            *goto = tasks;
        }
    }

    pub(super) fn receive_goto_tasks(
        &mut self,
        endpoint_id: &ClientEndpointId,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        let Some(goto) = self.goto_tasks_mut() else {
            return false;
        };
        let tasks = match result {
            Ok(ResponseResult::TaskList { tasks }) => Some(tasks),
            _ => None,
        };
        goto.receive_list(endpoint_id, tasks, Instant::now());
        true
    }

    /// Sends the full view's read while it has no text.
    fn sync_goto_result(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::GotoResult(viewer)) = self.overlay.as_ref() else {
            return;
        };
        let endpoint_id = viewer.endpoint_id.clone();
        let Some(read) = viewer.read.clone() else {
            // Opened before the task list: fetch it, then choose the read.
            if viewer.navigator.tasks.listed(&endpoint_id)
                || !self.endpoint_advertises(&endpoint_id, "task.list")
            {
                outcome.repaint |= self.choose_goto_result_read();
            } else if !self.endpoint_is_online(&endpoint_id) {
                if let Some(ClientShellOverlay::GotoResult(viewer)) = self.overlay.as_mut() {
                    viewer.body = PreviewBody::Unavailable("not available on this server".into());
                    outcome.repaint = true;
                }
            } else if viewer.navigator.tasks.list_due(&endpoint_id, now)
                && self.query_endpoint(
                    &endpoint_id,
                    Method::TaskList(TaskListParams::default()),
                    EndpointQueryKind::TaskList,
                    outcome,
                )
            {
                if let Some(goto) = self.goto_tasks_mut() {
                    goto.list_sent(&endpoint_id, now);
                }
            }
            return;
        };
        if viewer.pending || viewer.body != PreviewBody::Loading {
            return;
        }
        let method = read.method();
        let available = self.endpoint_is_online(&endpoint_id)
            && self.endpoint_advertises(&endpoint_id, crate::api::api_method_name(&method));
        let sent = available
            && self.query_endpoint(
                &endpoint_id,
                method,
                EndpointQueryKind::FullView(read),
                outcome,
            );
        if let Some(ClientShellOverlay::GotoResult(viewer)) = self.overlay.as_mut() {
            if sent {
                viewer.pending = true;
            } else if !available {
                viewer.body = PreviewBody::Unavailable("not available on this server".into());
                outcome.repaint = true;
            }
        }
    }

    pub(super) fn receive_goto_result(
        &mut self,
        endpoint_id: &ClientEndpointId,
        read: GotoPreviewRead,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        let Some(ClientShellOverlay::GotoResult(viewer)) = self.overlay.as_mut() else {
            return false;
        };
        if &viewer.endpoint_id != endpoint_id || viewer.read.as_ref() != Some(&read) {
            return false;
        }
        viewer.pending = false;
        viewer.body = preview_body(result);
        true
    }

    pub(super) fn receive_goto_preview(
        &mut self,
        endpoint_id: &ClientEndpointId,
        read: GotoPreviewRead,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        let Some(goto) = self.goto_tasks_mut() else {
            return false;
        };
        goto.receive_preview(endpoint_id, &read, preview_body(result), Instant::now());
        true
    }
}

fn preview_body(result: Result<ResponseResult, ClientShellEndpointError>) -> PreviewBody {
    match result {
        Ok(ResponseResult::PaneRead { read }) => PreviewBody::Text {
            source: PreviewSource::ScreenTail,
            text: read.text,
            note: None,
            truncated: false,
        },
        Ok(ResponseResult::TaskResult { result }) => PreviewBody::Text {
            source: match result.source {
                TaskResultSource::File => PreviewSource::ResultFile,
                _ => PreviewSource::ScreenTail,
            },
            text: result.text,
            note: result.file_error,
            truncated: result.truncated,
        },
        Ok(_) => PreviewBody::Unavailable("unexpected answer".into()),
        Err(error) => PreviewBody::Unavailable(error.message),
    }
}

#[cfg(test)]
mod tests;
