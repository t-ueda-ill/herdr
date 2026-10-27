use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ClientRemoteCollapsedGroups {
    pub(super) profile_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) collapsed_groups: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct ClientChromePreferences {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) investigations: Vec<super::investigation::SavedInvestigation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sidebar_width: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sidebar_section_split: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sidebar_collapsed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) agent_panel_sort: Option<crate::config::AgentPanelSortConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) collapsed_groups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) remote_collapsed_groups: Vec<ClientRemoteCollapsedGroups>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_picker: Option<ClientOpenPickerPreferences>,
    #[serde(
        default,
        deserialize_with = "super::investigation::lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(super) files_placement: Option<super::investigation::SavedFilesPlacement>,
    /// `Some(true)` once the files details band is turned off; absent shows it.
    #[serde(
        default,
        deserialize_with = "super::investigation::lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(super) hide_files_details: Option<bool>,
}

/// Favorites and successful opens of the ssh host, container host and
/// container pickers. Keys are `ssh:<alias>`, `host:local`, `host:<alias>`,
/// `container:local:<name>` and `container:<alias>:<name>`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct ClientOpenPickerPreferences {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) favorites: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) recent: Vec<ClientOpenPickerRecent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct ClientOpenPickerRecent {
    pub(super) key: String,
    pub(super) last_used_ms: u64,
    #[serde(default)]
    pub(super) count: u32,
}

pub(super) fn path_for_local_endpoint(socket_path: &Path) -> PathBuf {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in socket_path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    crate::config::state_dir()
        .join("client-shell")
        .join(format!("local-{hash:016x}.json"))
}

/// SSH preferences belong to the target/session, not its per-process bridge socket.
pub(super) fn path_for_remote_endpoint(target: &str, session: &str) -> PathBuf {
    use sha2::{Digest as _, Sha256};

    let mut hash = Sha256::new();
    for part in [target, session] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    crate::config::state_dir()
        .join("client-shell")
        .join(format!("remote-{:x}.json", hash.finalize()))
}

pub(super) fn load(path: &Path) -> Option<ClientChromePreferences> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub(super) fn store(path: &Path, preferences: ClientChromePreferences) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("invalid client shell state path: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create client shell state directory: {error}"))?;
    let content = serde_json::to_vec_pretty(&preferences)
        .map_err(|error| format!("failed to encode client shell state: {error}"))?;
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = path
        .file_name()
        .ok_or_else(|| format!("invalid client shell state path: {}", path.display()))?
        .to_os_string();
    temp_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    let temp_path = parent.join(temp_name);
    std::fs::write(&temp_path, content)
        .map_err(|error| format!("failed to write client shell state: {error}"))?;
    std::fs::rename(&temp_path, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp_path);
        format!("failed to replace client shell state: {error}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_paths_are_stable_and_distinct() {
        let first = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let again = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let second = path_for_local_endpoint(Path::new("/run/herdr/two.sock"));
        assert_eq!(first, again);
        assert_ne!(first, second);
    }

    #[test]
    fn remote_preferences_distinguish_targets_sessions_and_component_boundaries() {
        let original = path_for_remote_endpoint("dev@build", "agents");
        assert_eq!(original, path_for_remote_endpoint("dev@build", "agents"));
        assert_ne!(original, path_for_remote_endpoint("other@build", "agents"));
        assert_ne!(original, path_for_remote_endpoint("dev@build", "other"));
        assert_ne!(
            path_for_remote_endpoint("ab", "c"),
            path_for_remote_endpoint("a", "bc")
        );
        assert_ne!(
            original,
            path_for_local_endpoint(Path::new("dev@build/agents"))
        );
    }

    #[test]
    fn legacy_preferences_default_remote_collapses() {
        let preferences: ClientChromePreferences =
            serde_json::from_str(r#"{"collapsed_groups":["/repo"]}"#)
                .expect("legacy client chrome preferences");

        assert_eq!(preferences.collapsed_groups, ["/repo"]);
        assert!(preferences.remote_collapsed_groups.is_empty());
    }

    #[test]
    fn preferences_written_before_open_picker_memory_load_without_it() {
        let preferences: ClientChromePreferences =
            serde_json::from_str(r#"{"sidebar_width":30,"collapsed_groups":["/repo"]}"#)
                .expect("client chrome preferences without open_picker");

        assert_eq!(preferences.sidebar_width, Some(30));
        assert_eq!(preferences.open_picker, None);
    }

    #[test]
    fn an_unreadable_files_placement_loads_as_absent_and_keeps_the_rest() {
        let preferences: ClientChromePreferences = serde_json::from_str(
            r#"{"sidebar_width":30,"files_placement":"left","investigations":[{"endpoint":"local","workspace":"ws_1","root":"/r","files":[],"active_file":null,"horizontal":false,"query":"","include":"","exclude":"","regex":false,"case_sensitive":false,"whole_word":false,"ignored":false,"content_search":false}]}"#,
        )
        .expect("client chrome preferences with an unknown files placement");

        assert_eq!(preferences.files_placement, None);
        assert_eq!(preferences.sidebar_width, Some(30));
        assert_eq!(preferences.investigations.len(), 1);
    }

    #[test]
    fn hidden_files_details_round_trip_and_an_unreadable_value_shows_them() {
        let stored = serde_json::to_string(&ClientChromePreferences {
            hide_files_details: Some(true),
            ..ClientChromePreferences::default()
        })
        .expect("encode preferences");
        let restored: ClientChromePreferences =
            serde_json::from_str(&stored).expect("decode preferences");
        let unreadable: ClientChromePreferences =
            serde_json::from_str(r#"{"sidebar_width":30,"hide_files_details":"yes"}"#)
                .expect("decode preferences with an unknown value");

        assert_eq!(restored.hide_files_details, Some(true));
        assert_eq!(unreadable.hide_files_details, None);
        assert_eq!(unreadable.sidebar_width, Some(30));
        assert!(!serde_json::to_string(&ClientChromePreferences::default())
            .expect("encode default preferences")
            .contains("hide_files_details"));
    }

    #[test]
    fn open_picker_memory_round_trips_and_is_omitted_when_absent() {
        let memory = ClientOpenPickerPreferences {
            favorites: vec!["ssh:prod".into()],
            recent: vec![ClientOpenPickerRecent {
                key: "container:local:redis-1".into(),
                last_used_ms: 1_700_000_000_000,
                count: 3,
            }],
        };
        let stored = serde_json::to_string(&ClientChromePreferences {
            open_picker: Some(memory.clone()),
            ..ClientChromePreferences::default()
        })
        .expect("encode preferences");
        let restored: ClientChromePreferences =
            serde_json::from_str(&stored).expect("decode preferences");

        assert_eq!(restored.open_picker, Some(memory));
        assert!(!serde_json::to_string(&ClientChromePreferences::default())
            .expect("encode default preferences")
            .contains("open_picker"));
    }

    #[test]
    fn concurrent_stores_leave_complete_preferences() {
        let path = std::env::temp_dir().join(format!(
            "herdr-shell-concurrent-preferences-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let writers = (20..28)
            .map(|width| {
                let path = path.clone();
                std::thread::spawn(move || {
                    store(
                        &path,
                        ClientChromePreferences {
                            sidebar_width: Some(width),
                            ..ClientChromePreferences::default()
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().expect("preference writer").expect("store");
        }
        assert!(load(&path)
            .and_then(|saved| saved.sidebar_width)
            .is_some_and(|width| (20..28).contains(&width)));
        std::fs::remove_file(path).expect("remove preferences");
    }

    #[test]
    fn repeated_store_replaces_existing_preferences() {
        let path = std::env::temp_dir().join(format!(
            "herdr-shell-preferences-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        store(
            &path,
            ClientChromePreferences {
                sidebar_width: Some(24),
                ..ClientChromePreferences::default()
            },
        )
        .expect("first preference store");
        store(
            &path,
            ClientChromePreferences {
                sidebar_width: Some(32),
                ..ClientChromePreferences::default()
            },
        )
        .expect("replacement preference store");
        assert_eq!(load(&path).and_then(|saved| saved.sidebar_width), Some(32));
        std::fs::remove_file(path).expect("remove preferences");
    }
}
