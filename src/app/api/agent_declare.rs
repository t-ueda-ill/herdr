use crate::api::schema::{PaneDeclareAgentParams, ResponseResult};
use crate::app::App;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_pane_declare_agent(
        &mut self,
        id: String,
        params: PaneDeclareAgentParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        };
        let agent = match params.agent.as_deref().map(str::trim) {
            None => None,
            Some(label) => match crate::detect::parse_agent_label(label) {
                Some(agent) => Some(agent),
                None => {
                    return encode_error(id, "invalid_agent", format!("unknown agent {label}"));
                }
            },
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        };
        if !runtime.shell_runs_off_host() {
            return encode_error(
                id,
                "pane_not_off_host",
                format!(
                    "pane {} runs on this host, where Herdr identifies agents by process",
                    params.pane_id
                ),
            );
        }
        runtime.set_declared_agent(agent);

        encode_success(id, ResponseResult::Ok {})
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;

    use crate::api::schema::{
        AgentPromptParams, AgentSendKeysParams, ErrorResponse, Method, PaneDeclareAgentParams,
        PaneReleaseAgentParams, Request, ResponseResult, SuccessResponse,
    };
    use crate::app::{App, Mode};
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    struct Fixture {
        app: App,
        pane: String,
        rx: tokio::sync::mpsc::Receiver<Bytes>,
    }

    fn fixture(in_ssh: bool) -> Fixture {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("remote")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let (mut runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.test_set_in_ssh(in_ssh);
        app.state.insert_test_runtime(pane_id, runtime);
        let pane = app.public_pane_id(0, pane_id).unwrap();
        Fixture { app, pane, rx }
    }

    fn declare(fixture: &mut Fixture, agent: Option<&str>) -> String {
        fixture.app.handle_pane_declare_agent(
            "declare".into(),
            PaneDeclareAgentParams {
                pane_id: fixture.pane.clone(),
                agent: agent.map(str::to_string),
            },
        )
    }

    fn prompt(fixture: &mut Fixture) -> String {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(fixture.app.handle_deferred_agent_api_request(
            Request {
                id: "prompt".into(),
                method: Method::AgentPrompt(AgentPromptParams {
                    target: fixture.pane.clone(),
                    text: "hello".into(),
                    wait: None,
                }),
            },
            respond_to,
        ));
        response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("agent prompt responds")
    }

    fn send_keys(fixture: &mut Fixture) -> String {
        fixture.app.handle_agent_send_keys(
            "keys".into(),
            AgentSendKeysParams {
                target: fixture.pane.clone(),
                keys: vec!["ctrl+u".into()],
            },
        )
    }

    fn error_code(response: &str) -> String {
        serde_json::from_str::<ErrorResponse>(response)
            .unwrap_or_else(|_| panic!("expected an error: {response}"))
            .error
            .code
    }

    fn assert_ok(response: &str) {
        let success: SuccessResponse = serde_json::from_str(response)
            .unwrap_or_else(|_| panic!("expected success: {response}"));
        assert!(!matches!(success.result, ResponseResult::AgentList { .. }));
    }

    #[tokio::test]
    async fn an_undeclared_off_host_agent_refuses_input_with_a_hint() {
        let mut fixture = fixture(true);

        let prompted = prompt(&mut fixture);
        let keyed = send_keys(&mut fixture);

        for response in [&prompted, &keyed] {
            assert_eq!(error_code(response), "agent_off_host_undeclared");
            let message = serde_json::from_str::<ErrorResponse>(response)
                .unwrap()
                .error
                .message;
            assert!(message.contains("ssh pane"), "{message}");
            assert!(
                message.contains(&format!("herdr pane declare-agent {} claude", fixture.pane)),
                "{message}"
            );
        }
        assert!(fixture.rx.try_recv().is_err(), "nothing reaches the pane");
    }

    #[tokio::test]
    async fn a_declared_off_host_agent_accepts_input() {
        let mut fixture = fixture(true);

        assert_ok(&declare(&mut fixture, Some("claude")));

        assert_ok(&prompt(&mut fixture));
        assert_eq!(fixture.rx.try_recv().unwrap(), Bytes::from_static(b"hello"));
        assert_eq!(fixture.rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
        assert_ok(&send_keys(&mut fixture));
        assert_eq!(fixture.rx.try_recv().unwrap(), Bytes::from_static(b"\x15"));
    }

    #[tokio::test]
    async fn a_declaration_of_another_agent_does_not_unlock_input() {
        let mut fixture = fixture(true);

        assert_ok(&declare(&mut fixture, Some("codex")));

        assert_eq!(
            error_code(&prompt(&mut fixture)),
            "agent_off_host_undeclared"
        );
        assert!(fixture.rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn withdrawing_a_declaration_locks_input_again() {
        let mut fixture = fixture(true);
        assert_ok(&declare(&mut fixture, Some("claude")));

        assert_ok(&declare(&mut fixture, None));

        assert_eq!(
            error_code(&send_keys(&mut fixture)),
            "agent_off_host_undeclared"
        );
    }

    #[tokio::test]
    async fn releasing_the_agent_withdraws_its_declaration() {
        let mut fixture = fixture(true);
        assert_ok(&declare(&mut fixture, Some("claude")));
        let pane_id = fixture.app.state.workspaces[0].tabs[0].root_pane;
        let reset = fixture
            .app
            .lookup_runtime_sender(0, pane_id)
            .unwrap()
            .agent_detection_reset_notify_for_test();

        fixture.app.handle_pane_release_agent(
            "release".into(),
            PaneReleaseAgentParams {
                pane_id: fixture.pane.clone(),
                source: "cli".into(),
                agent: "claude".into(),
                seq: None,
            },
        );

        assert_eq!(
            error_code(&send_keys(&mut fixture)),
            "agent_off_host_undeclared"
        );
        tokio::time::timeout(Duration::from_secs(1), reset.notified())
            .await
            .expect("the detector releases the identity and claims that arrived meanwhile");
    }

    #[tokio::test]
    async fn a_local_pane_cannot_be_declared() {
        let mut fixture = fixture(false);

        assert_eq!(
            error_code(&declare(&mut fixture, Some("claude"))),
            "pane_not_off_host"
        );
    }

    #[tokio::test]
    async fn an_unknown_agent_label_is_rejected() {
        let mut fixture = fixture(true);

        assert_eq!(
            error_code(&declare(&mut fixture, Some("not-an-agent"))),
            "invalid_agent"
        );
    }
}
