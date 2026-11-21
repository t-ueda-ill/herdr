use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::api::schema::{
    Method, Request, ResponseResult, SuccessResponse, TaskListParams, TaskState, TaskSubmitParams,
    TaskTarget, TaskWaitParams,
};

const STATES: &[&str] = &[
    "queued",
    "submitted",
    "running",
    "needs_input",
    "completed",
    "stalled",
    "lost",
    "cancelled",
];

#[derive(Debug, PartialEq)]
pub(crate) enum Operation {
    Submit {
        params: TaskSubmitParams,
        wait: Option<WaitOptions>,
    },
    Get(TaskTarget),
    List(TaskListParams),
    Wait(TaskWaitParams),
    Cancel(TaskTarget),
}

#[derive(Debug, PartialEq)]
pub(crate) struct WaitOptions {
    until: Vec<TaskState>,
    timeout_ms: Option<u64>,
}

fn until_arg() -> Arg {
    Arg::new("until")
        .long("until")
        .value_name("STATE")
        .action(ArgAction::Append)
        .value_parser(clap::builder::PossibleValuesParser::new(STATES))
        .help("Also stop at this state; repeat for more than one")
}

fn timeout_arg() -> Arg {
    Arg::new("timeout")
        .long("timeout")
        .value_name("MS")
        .value_parser(clap::value_parser!(u64))
        .help("Fail with timeout after this many milliseconds")
}

fn task_id_arg() -> Arg {
    Arg::new("task_id").value_name("TASK_ID").required(true)
}

pub(crate) fn command() -> Command {
    Command::new("task")
        .about("Send prompts as tracked tasks and follow them")
        .subcommand_required(true)
        .subcommand(
            Command::new("submit")
                .about("Send a prompt to an agent as a task")
                .arg(Arg::new("target").value_name("TARGET").required(true))
                .arg(
                    Arg::new("text")
                        .value_name("TEXT")
                        .required(true)
                        .allow_hyphen_values(true),
                )
                .arg(
                    Arg::new("wait")
                        .long("wait")
                        .action(ArgAction::SetTrue)
                        .help("Wait until the task finishes"),
                )
                .arg(until_arg().requires("wait"))
                .arg(timeout_arg().requires("wait"))
                .after_help("A working agent keeps the task queued until it is idle or done. Exit status 1 after --wait means the task ended stalled, lost, or cancelled."),
        )
        .subcommand(Command::new("get").about("Show a task").arg(task_id_arg()))
        .subcommand(
            Command::new("list").about("List tasks, oldest first").arg(
                Arg::new("location")
                    .long("location")
                    .value_name("LOCATION")
                    .help("Only tasks at this location; ssh:HOST also lists its containers"),
            ),
        )
        .subcommand(
            Command::new("wait")
                .about("Wait until a task finishes")
                .arg(task_id_arg())
                .arg(until_arg())
                .arg(timeout_arg())
                .after_help("Exit status 1 means the task ended stalled, lost, or cancelled."),
        )
        .subcommand(
            Command::new("cancel")
                .about("Remove a queued task before it is sent")
                .arg(task_id_arg()),
        )
}

fn until(matches: &ArgMatches) -> Vec<TaskState> {
    matches
        .get_many::<String>("until")
        .into_iter()
        .flatten()
        .filter_map(|state| serde_json::from_value(serde_json::Value::String(state.clone())).ok())
        .collect()
}

fn target(matches: &ArgMatches) -> TaskTarget {
    TaskTarget {
        task_id: matches
            .get_one::<String>("task_id")
            .cloned()
            .unwrap_or_default(),
    }
}

pub(crate) fn parse(args: &[String]) -> Result<Operation, clap::Error> {
    let matches = command()
        .bin_name("herdr task")
        .try_get_matches_from(std::iter::once("task".to_string()).chain(args.iter().cloned()))?;
    let Some((name, matches)) = matches.subcommand() else {
        return Err(command().error(
            clap::error::ErrorKind::MissingSubcommand,
            "missing task command",
        ));
    };
    Ok(match name {
        "submit" => Operation::Submit {
            params: TaskSubmitParams {
                target: matches
                    .get_one::<String>("target")
                    .cloned()
                    .unwrap_or_default(),
                text: matches
                    .get_one::<String>("text")
                    .cloned()
                    .unwrap_or_default(),
            },
            wait: matches.get_flag("wait").then(|| WaitOptions {
                until: until(matches),
                timeout_ms: matches.get_one::<u64>("timeout").copied(),
            }),
        },
        "get" => Operation::Get(target(matches)),
        "list" => Operation::List(TaskListParams {
            location: matches.get_one::<String>("location").cloned(),
        }),
        "wait" => Operation::Wait(TaskWaitParams {
            task_id: target(matches).task_id,
            until: until(matches),
            timeout_ms: matches.get_one::<u64>("timeout").copied(),
        }),
        "cancel" => Operation::Cancel(target(matches)),
        _ => {
            return Err(command().error(
                clap::error::ErrorKind::InvalidSubcommand,
                format!("unknown task command: {name}"),
            ))
        }
    })
}

/// 0 when the wait ended where the caller asked; 1 when the task ended another way.
fn wait_exit_code(response: &serde_json::Value, until: &[TaskState]) -> i32 {
    let Ok(success) = serde_json::from_value::<SuccessResponse>(response.clone()) else {
        return 1;
    };
    match success.result {
        ResponseResult::TaskInfo { task }
            if task.state == TaskState::Completed || until.contains(&task.state) =>
        {
            0
        }
        _ => 1,
    }
}

fn request(id: &str, method: Method) -> Request {
    Request {
        id: format!("cli:task:{id}"),
        method,
    }
}

fn print_wait(response: &serde_json::Value, until: &[TaskState]) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        return super::print_response(response);
    }
    println!(
        "{}",
        serde_json::to_string(response).map_err(std::io::Error::other)?
    );
    Ok(wait_exit_code(response, until))
}

pub(super) fn run_task_command(args: &[String]) -> std::io::Result<i32> {
    let operation = match parse(args) {
        Ok(operation) => operation,
        Err(error) => return super::print_clap_error(&error),
    };
    match operation {
        Operation::Submit { params, wait } => {
            let submitted = super::send_request(&request("submit", Method::TaskSubmit(params)))?;
            let Some(wait) = wait else {
                return super::print_response(&submitted);
            };
            let Some(task_id) = submitted["result"]["task"]["task_id"].as_str() else {
                return super::print_response(&submitted);
            };
            let waited = super::send_request(&request(
                "wait",
                Method::TaskWait(TaskWaitParams {
                    task_id: task_id.to_string(),
                    until: wait.until.clone(),
                    timeout_ms: wait.timeout_ms,
                }),
            ))?;
            print_wait(&waited, &wait.until)
        }
        Operation::Get(target) => super::print_response(&super::send_request(&request(
            "get",
            Method::TaskGet(target),
        ))?),
        Operation::List(params) => super::print_response(&super::send_request(&request(
            "list",
            Method::TaskList(params),
        ))?),
        Operation::Wait(params) => {
            let until = params.until.clone();
            print_wait(
                &super::send_request(&request("wait", Method::TaskWait(params)))?,
                &until,
            )
        }
        Operation::Cancel(target) => super::print_response(&super::send_request(&request(
            "cancel",
            Method::TaskCancel(target),
        ))?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Operation {
        parse(
            &args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn submit_sends_target_and_text_and_waits_only_with_wait() {
        assert_eq!(
            parse_args(&["submit", "reviewer", "--fix this"]),
            Operation::Submit {
                params: TaskSubmitParams {
                    target: "reviewer".into(),
                    text: "--fix this".into(),
                },
                wait: None,
            }
        );
        assert_eq!(
            parse_args(&[
                "submit",
                "reviewer",
                "fix",
                "--wait",
                "--until",
                "needs_input",
                "--timeout",
                "9000"
            ]),
            Operation::Submit {
                params: TaskSubmitParams {
                    target: "reviewer".into(),
                    text: "fix".into(),
                },
                wait: Some(WaitOptions {
                    until: vec![TaskState::NeedsInput],
                    timeout_ms: Some(9000),
                }),
            }
        );
    }

    #[test]
    fn until_and_timeout_need_wait_on_submit() {
        let args = ["submit", "reviewer", "fix", "--until", "running"];
        assert!(parse(
            &args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>()
        )
        .is_err());
    }

    #[test]
    fn wait_list_get_and_cancel_build_their_requests() {
        assert_eq!(
            parse_args(&[
                "wait",
                "t_3",
                "--until",
                "running",
                "--until",
                "needs_input"
            ]),
            Operation::Wait(TaskWaitParams {
                task_id: "t_3".into(),
                until: vec![TaskState::Running, TaskState::NeedsInput],
                timeout_ms: None,
            })
        );
        assert_eq!(
            parse_args(&["list", "--location", "ssh:devbox"]),
            Operation::List(TaskListParams {
                location: Some("ssh:devbox".into()),
            })
        );
        assert_eq!(
            parse_args(&["get", "t_1"]),
            Operation::Get(TaskTarget {
                task_id: "t_1".into()
            })
        );
        assert_eq!(
            parse_args(&["cancel", "t_1"]),
            Operation::Cancel(TaskTarget {
                task_id: "t_1".into()
            })
        );
    }

    #[test]
    fn an_unknown_state_is_rejected() {
        let args = ["wait", "t_1", "--until", "done"];
        assert!(parse(
            &args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>()
        )
        .is_err());
    }

    #[test]
    fn the_state_list_covers_every_task_state() {
        for state in STATES {
            let parsed: TaskState =
                serde_json::from_value(serde_json::Value::String((*state).into())).unwrap();
            assert_ne!(parsed, TaskState::Unknown, "{state}");
        }
        assert_eq!(STATES.len(), 8);
    }

    #[test]
    fn a_wait_succeeds_only_where_the_caller_asked() {
        let response = |state: &str| {
            serde_json::json!({
                "id": "x",
                "result": { "type": "task_info", "task": {
                    "task_id": "t_1", "state": state, "target": "r", "terminal_id": "term",
                    "location": { "id": "local", "kind": "host" }, "text": "x", "created_at_ms": 0
                }}
            })
        };
        assert_eq!(wait_exit_code(&response("completed"), &[]), 0);
        assert_eq!(wait_exit_code(&response("stalled"), &[]), 1);
        assert_eq!(wait_exit_code(&response("lost"), &[]), 1);
        assert_eq!(
            wait_exit_code(&response("needs_input"), &[TaskState::NeedsInput]),
            0
        );
    }
}
