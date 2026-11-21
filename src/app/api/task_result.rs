//! `task.result`: the text a finished task left, from its result file or else
//! from the end of its pane. A file on ssh or in a container is read on a
//! worker, like `pane.file.read`, so a slow remote never blocks the app.

use std::sync::mpsc::Sender;

use crate::api::schema::{
    Method, PaneLocationKind, ReadFormat, ReadSource, Request, ResponseResult, TaskResult,
    TaskResultSource,
};
use crate::app::App;
use crate::offhost::PaneTarget;
use crate::tasks::TaskId;

use super::responses::{encode_error, encode_success};

/// Lines of the pane returned when there is no result file.
const SCREEN_TAIL_LINES: u32 = 40;

enum FileRead {
    Host(String),
    OffHost(Box<PaneTarget>, String),
}

impl FileRead {
    fn run(self) -> Result<String, String> {
        match self {
            Self::Host(path) => {
                crate::files::read(std::path::Path::new(&path)).map_err(|(_, message)| message)
            }
            Self::OffHost(target, path) => {
                super::pane_directory::read_off_host_text(&target, &path)
                    .map_err(|(_, message)| message)
            }
        }
    }
}

impl App {
    /// Starts `task.result`. Returns false for any other method.
    pub(crate) fn handle_deferred_task_api_request(
        &mut self,
        request: Request,
        respond_to: Sender<String>,
    ) -> bool {
        let Request { id, method } = request;
        let Method::TaskResult(target) = method else {
            return false;
        };
        let Some(task) = TaskId::parse(&target.task_id).and_then(|task| self.state.tasks.get(task))
        else {
            let _ = respond_to.send(encode_error(
                id,
                "task_not_found",
                format!("no task {}", target.task_id),
            ));
            return true;
        };
        if !task.state.is_finished() {
            let _ = respond_to.send(encode_error(
                id,
                "task_not_finished",
                format!(
                    "{} is {:?}; wait for it first",
                    task.id.public(),
                    task.state
                ),
            ));
            return true;
        }
        let task_id = task.id.public();
        let path = task.result_path.clone();
        let host = task.location.kind == PaneLocationKind::Host;
        let terminal = task.terminal.clone();
        let screen = self.task_screen_tail(&terminal);
        let read = path.clone().and_then(|path| {
            if host {
                return Some(FileRead::Host(path));
            }
            // Why not fail here: a closed pane still leaves no screen, and the
            // answer below says so.
            let pane = self.terminal_public_pane_id(&terminal)?;
            let target = self.pane_target(&pane).ok()?;
            Some(FileRead::OffHost(Box::new(target), path))
        });
        let Some(read) = read else {
            let _ = respond_to.send(result_response(id, task_id, path, None, screen));
            return true;
        };
        let job = (id, task_id, path, screen, read, respond_to);
        let spawned = crate::thread_spawn::spawn_named_with(
            "herdr-task-result",
            job,
            |(id, task_id, path, screen, read, respond_to)| {
                let file = read.run();
                let _ = respond_to.send(result_response(id, task_id, path, Some(file), screen));
            },
        );
        if let Err(((id, task_id, path, screen, _, respond_to), err)) = spawned {
            tracing::warn!(err = %err, "failed to spawn task result thread");
            let _ = respond_to.send(result_response(
                id,
                task_id,
                path,
                Some(Err(format!("could not start the read: {err}"))),
                screen,
            ));
        }
        true
    }

    fn task_screen_tail(&self, terminal: &crate::terminal::TerminalId) -> Option<String> {
        let (ws_idx, pane_id) = self.terminal_pane(terminal)?;
        let (runtime, _) = self.lookup_runtime(ws_idx, pane_id)?;
        Some(
            crate::app::api_helpers::read_terminal_snapshot(
                runtime,
                ReadSource::Recent,
                ReadFormat::Text,
                Some(SCREEN_TAIL_LINES),
            )
            .text,
        )
    }
}

fn result_response(
    id: String,
    task_id: String,
    path: Option<String>,
    file: Option<Result<String, String>>,
    screen: Option<String>,
) -> String {
    match choose_result(task_id, path, file, screen) {
        Ok(result) => encode_success(id, ResponseResult::TaskResult { result }),
        Err(message) => encode_error(id, "task_result_unavailable", message),
    }
}

/// The file wins; the screen is the fallback for a missing or unread file.
fn choose_result(
    task_id: String,
    path: Option<String>,
    file: Option<Result<String, String>>,
    screen: Option<String>,
) -> Result<TaskResult, String> {
    let file_error = match file {
        Some(Ok(text)) => {
            return Ok(TaskResult {
                task_id,
                source: TaskResultSource::File,
                path,
                text,
            })
        }
        Some(Err(message)) => Some(message),
        None => None,
    };
    match screen {
        Some(text) => Ok(TaskResult {
            task_id,
            source: TaskResultSource::Screen,
            path: None,
            text,
        }),
        None => Err(match file_error {
            Some(message) => format!("no result file ({message}) and the agent pane is gone"),
            None => "the agent pane is gone and the task has no result file".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_result_file_wins_over_the_screen() {
        let result = choose_result(
            "t_1".into(),
            Some("/tmp/r.md".into()),
            Some(Ok("done".into())),
            Some("screen".into()),
        )
        .unwrap();
        assert_eq!(result.source, TaskResultSource::File);
        assert_eq!(result.text, "done");
        assert_eq!(result.path.as_deref(), Some("/tmp/r.md"));
    }

    #[test]
    fn an_unread_result_file_falls_back_to_the_screen() {
        let result = choose_result(
            "t_1".into(),
            Some("/tmp/r.md".into()),
            Some(Err("not found".into())),
            Some("screen".into()),
        )
        .unwrap();
        assert_eq!(result.source, TaskResultSource::Screen);
        assert_eq!(result.path, None);
    }

    #[test]
    fn no_file_and_no_pane_is_unavailable_with_the_file_error() {
        let message = choose_result(
            "t_1".into(),
            Some("/tmp/r.md".into()),
            Some(Err("not found".into())),
            None,
        )
        .unwrap_err();
        assert!(message.contains("not found"), "{message}");
    }
}
