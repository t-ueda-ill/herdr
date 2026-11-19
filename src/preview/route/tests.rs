use super::*;
use crate::preview::http::parse_request;

fn request(head: &str) -> Request {
    parse_request(head.as_bytes()).unwrap()
}

#[test]
fn paths_inside_the_root_become_relative() {
    assert_eq!(relative_path("/").unwrap(), "");
    assert_eq!(relative_path("/docs/a.md").unwrap(), "docs/a.md");
    assert_eq!(relative_path("/docs//a.md").unwrap(), "docs/a.md");
    assert_eq!(relative_path("/docs/./a.md").unwrap(), "docs/a.md");
    assert_eq!(relative_path("/docs/").unwrap(), "docs/");
}

#[test]
fn paths_that_climb_or_smuggle_separators_are_refused() {
    for path in ["/../etc/passwd", "/docs/../../x", "/a\\b", "/.."] {
        assert!(relative_path(path).is_err(), "{path}");
    }
}

#[test]
fn a_directory_path_serves_its_index() {
    assert_eq!(file_to_serve(""), "index.html");
    assert_eq!(file_to_serve("docs/"), "docs/index.html");
    assert_eq!(file_to_serve("docs/a.md"), "docs/a.md");
}

#[test]
fn content_types_follow_the_extension_case_insensitively() {
    assert_eq!(content_type("index.HTML"), "text/html; charset=utf-8");
    assert_eq!(content_type("a.css"), "text/css; charset=utf-8");
    assert_eq!(content_type("a.mjs"), "text/javascript; charset=utf-8");
    assert_eq!(content_type("a.svg"), "image/svg+xml");
    assert_eq!(content_type("a.webp"), "image/webp");
    assert_eq!(content_type("a.pdf"), "application/pdf");
    assert_eq!(content_type("a.mp4"), "video/mp4");
    assert_eq!(content_type("a.wasm"), "application/wasm");
    assert_eq!(content_type("README.md"), "text/plain; charset=utf-8");
    assert_eq!(content_type("Makefile"), "application/octet-stream");
}

#[test]
fn formats_herdr_renders_are_recognised_by_extension() {
    assert_eq!(format_of("README.md"), Some(Format::Markdown));
    assert_eq!(format_of("notes.MARKDOWN"), Some(Format::Markdown));
    assert_eq!(format_of("flow.mmd"), Some(Format::Mermaid));
    assert_eq!(format_of("data.csv"), Some(Format::Csv));
    assert_eq!(format_of("data.tsv"), Some(Format::Tsv));
    assert_eq!(format_of("package.json"), Some(Format::Json));
    assert_eq!(format_of("ci.yml"), Some(Format::Code("yaml")));
    assert_eq!(format_of("Cargo.toml"), Some(Format::Code("ini")));
    assert_eq!(format_of("main.rs"), Some(Format::Code("rust")));
    assert_eq!(format_of("index.html"), None);
    assert_eq!(format_of("photo.png"), None);
    assert_eq!(format_of("Makefile"), None);
}

#[test]
fn only_a_page_navigation_is_wrapped() {
    let navigation = request("GET /a.md HTTP/1.1\r\nSec-Fetch-Dest: document\r\n\r\n");
    let fetch = request("GET /a.md HTTP/1.1\r\nSec-Fetch-Dest: empty\r\n\r\n");
    let raw = request("GET /a.md?herdr-raw=1 HTTP/1.1\r\nSec-Fetch-Dest: document\r\n\r\n");
    assert!(wants_page(&navigation));
    assert!(!wants_page(&fetch));
    assert!(!wants_page(&raw));
}

#[test]
fn browsers_without_fetch_metadata_navigate_with_an_html_accept() {
    let navigation = request("GET /a.md HTTP/1.1\r\nAccept: text/html,*/*\r\n\r\n");
    let script = request("GET /a.md HTTP/1.1\r\nAccept: */*\r\n\r\n");
    assert!(wants_page(&navigation));
    assert!(!wants_page(&script));
}

#[test]
fn only_this_machines_loopback_names_may_be_the_host() {
    let port = 4321;
    let ok = request("GET / HTTP/1.1\r\nHost: 127.0.0.1:4321\r\n\r\n");
    let localhost = request("GET / HTTP/1.1\r\nHost: localhost:4321\r\n\r\n");
    let rebound = request("GET / HTTP/1.1\r\nHost: evil.example:4321\r\n\r\n");
    let other_port = request("GET / HTTP/1.1\r\nHost: 127.0.0.1:80\r\n\r\n");
    let missing = request("GET / HTTP/1.1\r\n\r\n");
    assert!(host_allowed(&ok, port));
    assert!(host_allowed(&localhost, port));
    assert!(!host_allowed(&rebound, port));
    assert!(!host_allowed(&other_port, port));
    assert!(!host_allowed(&missing, port));
}

#[test]
fn the_token_query_signs_in_and_the_cookie_keeps_the_session() {
    let port = 4321;
    let token = "secret";
    let signing_in = request("GET /a.md?herdr-token=secret HTTP/1.1\r\n\r\n");
    let signed_in = request("GET /a.md HTTP/1.1\r\nCookie: herdr-preview-4321=secret\r\n\r\n");
    let wrong = request("GET /a.md?herdr-token=guess HTTP/1.1\r\n\r\n");
    let stranger = request("GET /a.md HTTP/1.1\r\n\r\n");
    assert_eq!(access(&signing_in, port, token), Access::SignIn);
    assert_eq!(access(&signed_in, port, token), Access::Allowed);
    assert_eq!(access(&wrong, port, token), Access::Denied);
    assert_eq!(access(&stranger, port, token), Access::Denied);
}
