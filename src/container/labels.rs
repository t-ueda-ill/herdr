use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::types::{BindMount, ComposeInfo, ContainerInfo, ContainerListError, HerdrLabels};

const LABEL_COMPOSE_PROJECT: &str = "com.docker.compose.project";
const LABEL_COMPOSE_SERVICE: &str = "com.docker.compose.service";
const LABEL_COMPOSE_WORKING_DIR: &str = "com.docker.compose.project.working_dir";
const LABEL_COMPOSE_CONFIG_FILES: &str = "com.docker.compose.project.config_files";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawInspect {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: RawState,
    #[serde(default)]
    config: RawConfig,
    #[serde(default)]
    mounts: Option<Vec<RawMount>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct RawState {
    #[serde(default)]
    status: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct RawConfig {
    #[serde(default)]
    labels: Option<HashMap<String, String>>,
    #[serde(default)]
    working_dir: String,
    #[serde(default)]
    user: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawMount {
    #[serde(rename = "Type", default)]
    kind: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    destination: String,
}

/// Parses the JSON array printed by `docker inspect`. `docker ps --format
/// json` is not usable here because it flattens labels into one
/// comma-joined string that cannot be split back reliably.
pub(crate) fn parse_inspect_output(json: &str) -> Result<Vec<ContainerInfo>, ContainerListError> {
    let raw: Vec<RawInspect> = serde_json::from_str(json)
        .map_err(|err| ContainerListError::InvalidOutput(err.to_string()))?;
    Ok(raw.into_iter().map(normalize).collect())
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn normalize(raw: RawInspect) -> ContainerInfo {
    let labels = raw.config.labels.unwrap_or_default();
    let compose = match (
        labels.get(LABEL_COMPOSE_PROJECT),
        labels.get(LABEL_COMPOSE_SERVICE),
    ) {
        (Some(project), Some(service)) => Some(ComposeInfo {
            project: project.clone(),
            service: service.clone(),
            working_dir: labels
                .get(LABEL_COMPOSE_WORKING_DIR)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            config_files: labels
                .get(LABEL_COMPOSE_CONFIG_FILES)
                .map(|v| {
                    v.split(',')
                        .filter(|part| !part.is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
        }),
        _ => None,
    };
    let herdr = HerdrLabels {
        shell: labels.get("herdr.shell").filter(|v| !v.is_empty()).cloned(),
        workdir: labels
            .get("herdr.workdir")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from),
        user: labels.get("herdr.user").filter(|v| !v.is_empty()).cloned(),
        default: labels
            .get("herdr.default")
            .is_some_and(|v| v == "true" || v == "1"),
    };
    let bind_mounts = raw
        .mounts
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.kind == "bind")
        .map(|m| BindMount {
            source: m.source.into(),
            destination: m.destination.into(),
        })
        .collect();
    ContainerInfo {
        id: raw.id,
        name: raw.name.strip_prefix('/').unwrap_or(&raw.name).to_string(),
        running: raw.state.status == "running",
        compose,
        working_dir: non_empty(raw.config.working_dir).map(PathBuf::from),
        user: non_empty(raw.config.user),
        bind_mounts,
        herdr,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::container::ContainerRef;

    const COMPOSE: &str = include_str!("../../tests/fixtures/docker/inspect-compose.json");
    const RUN: &str = include_str!("../../tests/fixtures/docker/inspect-run.json");

    fn one(json: &str) -> ContainerInfo {
        let mut infos = parse_inspect_output(json).unwrap();
        assert_eq!(infos.len(), 1);
        infos.remove(0)
    }

    #[test]
    fn inspect_output_parses_compose_labels_into_compose_info() {
        let info = one(COMPOSE);
        let compose = info.compose.expect("compose labels present");
        assert_eq!(compose.project, "shop");
        assert_eq!(compose.service, "db");
        assert_eq!(
            compose.working_dir,
            Some(PathBuf::from("/home/dev/src/shop/db"))
        );
        assert_eq!(
            compose.config_files,
            vec![
                PathBuf::from("/home/dev/src/shop/db/docker-compose.yml"),
                PathBuf::from("/home/dev/src/shop/db/docker-compose.override.yml"),
            ]
        );
        assert!(info.running);
        assert_eq!(
            info.working_dir,
            Some(PathBuf::from("/var/lib/postgresql/13/data"))
        );
        assert_eq!(info.user, None);
    }

    #[test]
    fn inspect_output_without_compose_labels_yields_name_reference() {
        let info = one(RUN);
        assert!(info.compose.is_none());
        assert_eq!(info.reference(), ContainerRef::Name("scratch".into()));
        assert_eq!(info.user.as_deref(), Some("1000:1000"));
    }

    #[test]
    fn inspect_output_reads_herdr_labels() {
        let herdr = one(RUN).herdr;
        assert_eq!(herdr.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(herdr.workdir, Some(PathBuf::from("/srv/app")));
        assert_eq!(herdr.user.as_deref(), Some("dev"));
        assert!(herdr.default);
        assert_eq!(one(COMPOSE).herdr, Default::default());
    }

    #[test]
    fn inspect_output_strips_leading_slash_from_name() {
        assert_eq!(one(COMPOSE).name, "shop-db-1");
    }

    #[test]
    fn inspect_output_keeps_only_bind_mounts() {
        let info = one(COMPOSE);
        let destinations: Vec<_> = info
            .bind_mounts
            .iter()
            .map(|m| m.destination.to_str().unwrap())
            .collect();
        assert_eq!(
            destinations,
            vec!["/usr/bin/startup", "/var/log/postgresql"]
        );
    }

    #[test]
    fn inspect_output_rejects_non_array_json() {
        assert!(matches!(
            parse_inspect_output("{}"),
            Err(ContainerListError::InvalidOutput(_))
        ));
        assert!(matches!(
            parse_inspect_output("not json"),
            Err(ContainerListError::InvalidOutput(_))
        ));
    }

    #[test]
    fn inspect_output_treats_empty_working_dir_and_user_as_unset() {
        let json = r#"[{"Id":"abc","Name":"/x","State":{"Status":"exited"},
            "Config":{"Labels":null,"WorkingDir":"","User":""},"Mounts":null}]"#;
        let info = one(json);
        assert!(!info.running);
        assert_eq!(info.working_dir, None);
        assert_eq!(info.user, None);
        assert!(info.bind_mounts.is_empty());
    }

    #[test]
    fn inspect_output_treats_half_compose_labels_as_plain_container() {
        let json = r#"[{"Id":"abc","Name":"/x","State":{"Status":"running"},
            "Config":{"Labels":{"com.docker.compose.project":"p"}},"Mounts":[]}]"#;
        assert!(one(json).compose.is_none());
    }
}
