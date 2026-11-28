use std::time::Duration;

use super::*;
use crate::api::schema::PaneLocationKind;
use crate::preview::source::LocalSource;

fn server() -> PreviewServer {
    let source = LocalSource::new(&std::env::temp_dir()).unwrap();
    PreviewServer::start(
        Box::new(source),
        Duration::from_secs(1),
        crate::preview::server::ListingSwitch::new(true),
    )
    .unwrap()
}

fn container() -> PaneLocation {
    PaneLocation {
        kind: PaneLocationKind::Container,
        ssh_host: Some("coding".into()),
        container: Some("web".into()),
    }
}

#[test]
fn the_entry_becomes_a_path_below_the_root() {
    assert_eq!(entry_relative("/site", None).unwrap(), "");
    assert_eq!(entry_relative("/site/", Some("/site")).unwrap(), "");
    assert_eq!(
        entry_relative("/site", Some("/site/docs/a.md")).unwrap(),
        "docs/a.md"
    );
    assert_eq!(
        entry_relative("/", Some("/etc/hosts")).unwrap(),
        "etc/hosts"
    );
}

#[test]
fn an_entry_outside_the_root_is_refused() {
    for entry in ["/other/a.md", "/sitemap/a.md", "/site/../etc/passwd"] {
        assert!(entry_relative("/site", Some(entry)).is_err(), "{entry}");
    }
}

#[test]
fn a_host_entry_is_relative_by_path_components_on_any_platform() {
    let root = std::env::temp_dir().join("site");
    let nested = root.join("docs").join("a b.md");
    assert_eq!(host_entry_relative(&root, &nested).unwrap(), "docs/a b.md");
    assert_eq!(host_entry_relative(&root, &root).unwrap(), "");
    let outside = std::env::temp_dir().join("sitemap").join("a.md");
    assert!(host_entry_relative(&root, &outside).is_err());
}

#[test]
fn the_same_folder_in_the_same_place_is_found_again() {
    let mut registry = PreviewRegistry::default();
    let started = registry.insert("/site/", Some(container()), server(), "");
    let again = registry
        .find("/site", Some(&container()), "docs/a.md")
        .unwrap();
    assert_eq!(again.preview_id, started.preview_id);
    assert_eq!(again.port, started.port);
    assert!(again.url.ends_with("/docs/a.md"), "{}", again.url);
    assert!(registry.find("/site", None, "").is_none());
    assert!(registry.find("/other", Some(&container()), "").is_none());
}

#[test]
fn stopping_removes_the_preview_from_the_list() {
    let mut registry = PreviewRegistry::default();
    let first = registry.insert("/a", None, server(), "");
    let second = registry.insert("/b", None, server(), "");
    assert_ne!(first.preview_id, second.preview_id);
    assert_eq!(registry.list().len(), 2);
    assert!(registry.stop(&first.preview_id));
    assert!(!registry.stop(&first.preview_id));
    let listed: Vec<_> = registry
        .list()
        .into_iter()
        .map(|info| info.preview_id)
        .collect();
    assert_eq!(listed, vec![second.preview_id]);
}
