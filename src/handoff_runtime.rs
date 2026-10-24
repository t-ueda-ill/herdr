#[cfg(unix)]
use serde::{Deserialize, Serialize};

/// Long-lived pane runtime transferred during server replacement.
///
/// Handoff preserves server-owned session state such as PTYs, processes, agent
/// identity, and durable plugin/session metadata. It intentionally does not
/// preserve transient coordination such as in-flight requests, waits,
/// subscriptions, client sockets, or pane-to-pane messages; clients reconnect
/// and retry those operations after replacement.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffRuntimeState {
    pub pane_id: u32,
    pub child_pid: u32,
    pub rows: u16,
    pub cols: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    #[serde(default)]
    pub keyboard_protocol_flags: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_protocol_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_state: Option<crate::pane::InputState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_history_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<crate::terminal::state::HandoffAgentState>,
    /// Whether the shell runs in a container or over ssh. Why they travel: the
    /// pane is rebuilt around a PTY fd, so nothing else says the shell is not on
    /// this host, and a host verdict would reject the remote OSC 7 host name and
    /// ignore claims. Absent (false) in handoffs from older binaries.
    #[serde(default)]
    pub in_container: bool,
    #[serde(default)]
    pub in_ssh: bool,
    /// The shell's last OSC 7 report, so the sidebar, files, and new tabs
    /// follow the remote directory right after the handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_cwd: Option<std::path::PathBuf>,
    /// The agent an in-band claim says is running in the shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offhost_claim: Option<HandoffOffHostClaim>,
}

/// A live `herdr-agent` start claim: the agent label and the token its exit
/// must present.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HandoffOffHostClaim {
    pub kind: String,
    pub token: String,
}

#[cfg(unix)]
impl HandoffRuntimeState {
    pub fn with_pane_id(mut self, pane_id: crate::layout::PaneId) -> Self {
        self.pane_id = pane_id.raw();
        self
    }
}

#[derive(Debug)]
pub(crate) struct ImportedHandoffRuntime {
    #[cfg(unix)]
    pub master_fd: std::os::fd::RawFd,
    #[cfg(unix)]
    pub state: HandoffRuntimeState,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn legacy_state() -> serde_json::Value {
        serde_json::json!({
            "pane_id": 1,
            "child_pid": 2,
            "rows": 24,
            "cols": 80,
            "cell_width_px": 8,
            "cell_height_px": 16,
        })
    }

    #[test]
    fn state_from_an_older_binary_is_a_host_shell_without_claim_or_cwd() {
        let state: HandoffRuntimeState = serde_json::from_value(legacy_state()).unwrap();

        assert!(!state.in_container);
        assert!(!state.in_ssh);
        assert_eq!(state.reported_cwd, None);
        assert_eq!(state.offhost_claim, None);
    }

    #[test]
    fn off_host_fields_survive_a_serde_round_trip() {
        let mut state: HandoffRuntimeState = serde_json::from_value(legacy_state()).unwrap();
        state.in_container = true;
        state.reported_cwd = Some("/srv/app".into());
        state.offhost_claim = Some(HandoffOffHostClaim {
            kind: "codex".into(),
            token: "7-1".into(),
        });

        let back: HandoffRuntimeState =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();

        assert!(back.in_container && !back.in_ssh);
        assert_eq!(back.reported_cwd, Some("/srv/app".into()));
        assert_eq!(back.offhost_claim, state.offhost_claim);
    }

    #[test]
    fn a_host_shell_state_omits_the_optional_off_host_fields() {
        let state: HandoffRuntimeState = serde_json::from_value(legacy_state()).unwrap();

        let value = serde_json::to_value(&state).unwrap();

        assert!(value.get("reported_cwd").is_none());
        assert!(value.get("offhost_claim").is_none());
    }
}
