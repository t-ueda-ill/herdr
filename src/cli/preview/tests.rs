use super::*;
use crate::api::schema::{PaneLocation, PaneLocationKind};

fn parse(args: &[&str]) -> Result<Parsed, ParseError> {
    request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
}

#[test]
fn start_sends_an_absolute_host_root_and_entry() {
    let parsed = parse(&["start", "site", "--entry", "site/docs/a.md", "--open"]).unwrap();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        serde_json::to_value(parsed.request).unwrap(),
        serde_json::json!({
            "id": "cli:preview:start",
            "method": "preview.start",
            "params": {
                "root": cwd.join("site").to_string_lossy(),
                "entry": cwd.join("site/docs/a.md").to_string_lossy(),
            }
        })
    );
    assert!(parsed.open);
    assert!(!parsed.json);
}

#[test]
fn start_in_a_pane_passes_its_paths_through() {
    let parsed = parse(&["start", "/srv/site", "--pane", "w1-2", "--json"]).unwrap();
    assert_eq!(
        serde_json::to_value(parsed.request).unwrap(),
        serde_json::json!({
            "id": "cli:preview:start",
            "method": "preview.start",
            "params": {"pane_id": "w1-2", "root": "/srv/site"}
        })
    );
    assert!(parsed.json);
    assert!(!parsed.open);
}

#[test]
fn list_and_stop_name_their_methods() {
    assert_eq!(
        serde_json::to_value(parse(&["list"]).unwrap().request).unwrap(),
        serde_json::json!({"id": "cli:preview:list", "method": "preview.list", "params": {}})
    );
    assert_eq!(
        serde_json::to_value(parse(&["stop", "preview-3"]).unwrap().request).unwrap(),
        serde_json::json!({
            "id": "cli:preview:stop",
            "method": "preview.stop",
            "params": {"preview_id": "preview-3"}
        })
    );
}

#[test]
fn unknown_commands_and_missing_arguments_are_usage_errors() {
    for args in [
        vec![],
        vec!["start"],
        vec!["stop"],
        vec!["open"],
        vec!["list", "x"],
    ] {
        assert!(
            matches!(parse(&args), Err(ParseError::Clap(error)) if error.exit_code() == 2),
            "{args:?}"
        );
    }
    command().debug_assert();
}

#[test]
fn the_list_names_where_each_folder_lives() {
    let previews = vec![
        PreviewInfo {
            preview_id: "preview-1".into(),
            root: "/home/me/site".into(),
            location: None,
            port: 40123,
            url: String::new(),
        },
        PreviewInfo {
            preview_id: "preview-2".into(),
            root: "/srv".into(),
            location: Some(PaneLocation {
                kind: PaneLocationKind::Container,
                ssh_host: Some("coding".into()),
                container: Some("web".into()),
            }),
            port: 40124,
            url: String::new(),
        },
    ];
    assert_eq!(
        format_previews(&previews),
        "id         port   place          root\n\
         preview-1  40123  this host      /home/me/site\n\
         preview-2  40124  web on coding  /srv\n"
    );
    assert_eq!(format_previews(&[]), "No previews running.\n");
}
