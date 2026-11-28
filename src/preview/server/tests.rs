use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;
use crate::preview::source::LocalSource;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "herdr-preview-server-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(&path).unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, relative: &str, contents: &[u8]) {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Reply {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn send(port: u16, request: &str) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let end = crate::preview::http::head_end(&raw).expect("response head");
    let head = String::from_utf8(raw[..end].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
        .collect();
    Reply {
        status,
        headers,
        body: raw[end..].to_vec(),
    }
}

struct Preview {
    server: PreviewServer,
    listing: ListingSwitch,
    token: String,
    _root: TempDir,
}

impl Preview {
    fn start(root: TempDir) -> Self {
        let source = LocalSource::new(root.path()).unwrap();
        let listing = ListingSwitch::new(true);
        let server = PreviewServer::start(
            Box::new(source),
            Duration::from_millis(100),
            listing.clone(),
        )
        .unwrap();
        let token = server
            .url("")
            .trim_start_matches("http://127.0.0.1:")
            .split('/')
            .nth(1)
            .unwrap()
            .to_owned();
        Self {
            server,
            listing,
            token,
            _root: root,
        }
    }

    fn port(&self) -> u16 {
        self.server.port()
    }

    fn get(&self, path: &str, extra: &str) -> Reply {
        send(
            self.port(),
            &format!(
                "GET /{}{path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{extra}\r\n",
                self.token,
                self.port()
            ),
        )
    }
}

fn site() -> TempDir {
    let root = TempDir::new("site");
    root.file(
        "index.html",
        b"<html><body><h1>home</h1><script src=\"/app.js\"></script></body></html>",
    );
    root.file("docs/README.md", b"# Title\n\n<script>alert(1)</script>\n");
    root.file("media/clip.bin", b"0123456789");
    root
}

#[test]
fn the_url_opens_the_entry_under_the_token_and_sets_no_cookie() {
    let preview = Preview::start(site());
    let url = preview.server.url("docs/README.md");
    assert_eq!(
        url,
        format!(
            "http://127.0.0.1:{}/{}/docs/README.md",
            preview.port(),
            preview.token
        )
    );
    let reply = preview.get("/docs/README.md?herdr-raw=1", "");
    assert_eq!(reply.status, 200);
    assert!(reply.header("set-cookie").is_none());
    assert_eq!(reply.header("referrer-policy"), Some("same-origin"));
    let bare = send(
        preview.port(),
        &format!(
            "GET /{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            preview.token,
            preview.port()
        ),
    );
    assert_eq!(bare.status, 301);
    assert_eq!(
        bare.header("location"),
        Some(format!("/{}/", preview.token).as_str())
    );
}

#[test]
fn a_root_absolute_link_from_a_preview_page_moves_under_the_token() {
    let preview = Preview::start(site());
    let referer = format!(
        "Referer: http://127.0.0.1:{}/{}/index.html\r\n",
        preview.port(),
        preview.token
    );
    // Why a redirect and not the file: what the stylesheet loads in turn
    // names it as the referrer, so its URL must keep the token too.
    let style = send(
        preview.port(),
        &format!(
            "GET /style.css HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nSec-Fetch-Mode: no-cors\r\n{referer}\r\n",
            preview.port()
        ),
    );
    assert_eq!(style.status, 307);
    assert_eq!(
        style.header("location"),
        Some(format!("/{}/style.css", preview.token).as_str())
    );
    let page = send(
        preview.port(),
        &format!(
            "GET /docs/README.md?x=1 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nSec-Fetch-Mode: navigate\r\n{referer}\r\n",
            preview.port()
        ),
    );
    assert_eq!(page.status, 303);
    assert_eq!(
        page.header("location"),
        Some(format!("/{}/docs/README.md?x=1", preview.token).as_str())
    );
}

#[test]
fn a_browser_without_the_token_or_from_another_host_is_turned_away() {
    let preview = Preview::start(site());
    let stranger = send(
        preview.port(),
        &format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            preview.port()
        ),
    );
    assert_eq!(stranger.status, 403);
    let other_port = send(
        preview.port(),
        &format!(
            "GET /index.html HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nReferer: http://127.0.0.1:1/{}/\r\n\r\n",
            preview.port(),
            preview.token
        ),
    );
    assert_eq!(other_port.status, 403);
    let rebound = send(
        preview.port(),
        &format!(
            "GET /{}/ HTTP/1.1\r\nHost: evil.example:{}\r\n\r\n",
            preview.token,
            preview.port()
        ),
    );
    assert_eq!(rebound.status, 403);
}

#[test]
fn an_html_page_gets_the_reload_hook_and_keeps_its_own_scripts() {
    let preview = Preview::start(site());
    let reply = preview.get("/", "Sec-Fetch-Dest: document\r\n");
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(reply.header("content-security-policy").is_none());
    let text = reply.text();
    assert!(text.contains("<script src=\"/app.js\"></script>"));
    assert!(text.contains(crate::preview::page::RELOAD_TAG));
}

#[test]
fn a_markdown_navigation_gets_the_viewer_under_a_script_policy() {
    let preview = Preview::start(site());
    let reply = preview.get("/docs/README.md", "Sec-Fetch-Dest: document\r\n");
    assert_eq!(reply.status, 200);
    let policy = reply.header("content-security-policy").unwrap();
    assert!(policy.contains(&format!(
        "script-src http://127.0.0.1:{}/__herdr/assets/",
        preview.port()
    )));
    assert!(reply.text().contains(r#"data-format="markdown""#));
    assert!(!reply.text().contains("alert(1)"));
}

#[test]
fn the_viewer_fetch_gets_the_raw_bytes() {
    let preview = Preview::start(site());
    let reply = preview.get("/docs/README.md?herdr-raw=1", "Sec-Fetch-Dest: empty\r\n");
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(reply.body, b"# Title\n\n<script>alert(1)</script>\n");
}

#[test]
fn a_range_request_gets_only_those_bytes() {
    let preview = Preview::start(site());
    let reply = preview.get("/media/clip.bin", "Range: bytes=2-5\r\n");
    assert_eq!(reply.status, 206);
    assert_eq!(reply.body, b"2345");
    assert_eq!(reply.header("content-range"), Some("bytes 2-5/10"));
    let beyond = preview.get("/media/clip.bin", "Range: bytes=20-\r\n");
    assert_eq!(beyond.status, 416);
    assert_eq!(beyond.header("content-range"), Some("bytes */10"));
}

#[test]
fn a_folder_redirects_to_its_slash_and_a_missing_page_waits_for_the_file() {
    let preview = Preview::start(site());
    let folder = preview.get("/docs", "Sec-Fetch-Dest: document\r\n");
    assert_eq!(folder.status, 301);
    assert_eq!(
        folder.header("location"),
        Some(format!("/{}/docs/", preview.token).as_str())
    );
    let missing = preview.get("/docs/later.md", "Sec-Fetch-Dest: document\r\n");
    assert_eq!(missing.status, 404);
    assert!(missing.text().contains(crate::preview::page::RELOAD_TAG));
}

#[test]
fn a_folder_without_an_index_lists_its_entries() {
    let preview = Preview::start(site());
    let reply = preview.get("/docs/", "Sec-Fetch-Dest: document\r\n");
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(reply.text().contains(r#"href="./README.md""#));
    assert!(reply.header("content-security-policy").is_some());
    // A folder with an index still serves the index.
    assert!(preview.get("/", "").text().contains("<h1>home</h1>"));
}

#[test]
fn a_missing_folder_is_not_found_even_with_listing_on() {
    let preview = Preview::start(site());
    assert_eq!(
        preview
            .get("/nowhere/", "Sec-Fetch-Dest: document\r\n")
            .status,
        404
    );
}

#[test]
fn turning_listing_off_brings_back_the_waiting_page_at_once() {
    let preview = Preview::start(site());
    preview.listing.set(false);
    let reply = preview.get("/docs/", "Sec-Fetch-Dest: document\r\n");
    assert_eq!(reply.status, 404);
    preview.listing.set(true);
    assert_eq!(
        preview.get("/docs/", "Sec-Fetch-Dest: document\r\n").status,
        200
    );
}

#[test]
fn a_file_added_to_a_listed_folder_does_not_reload_open_pages() {
    let root = site();
    let added = root.path().join("docs/new.md");
    let preview = Preview::start(root);
    assert_eq!(preview.get("/docs/", "").status, 200);
    let mut reader = open_events(&preview, Duration::from_millis(1800));
    std::thread::sleep(Duration::from_millis(400));
    std::fs::write(&added, b"# new").unwrap();
    let mut received = String::new();
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
        received.push_str(&line);
        line.clear();
    }
    assert!(!received.contains("data: reload"), "{received}");
}

#[test]
fn the_refusal_without_the_token_tells_how_to_find_the_address() {
    let preview = Preview::start(site());
    let stranger = send(
        preview.port(),
        &format!(
            "GET /docs/README.md HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            preview.port()
        ),
    );
    assert_eq!(stranger.status, 403);
    assert!(stranger.text().contains("herdr preview list"));
    assert!(!stranger.text().contains(&preview.token));
}

#[test]
fn herdr_assets_are_served_and_a_head_request_has_no_body() {
    let preview = Preview::start(site());
    let asset = send(
        preview.port(),
        &format!(
            "GET /__herdr/assets/viewer.js HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            preview.port()
        ),
    );
    assert_eq!(asset.status, 200);
    assert!(!asset.body.is_empty());
    let head = send(
        preview.port(),
        &format!(
            "HEAD /{}/ HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            preview.token,
            preview.port()
        ),
    );
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_ne!(head.header("content-length"), Some("0"));
}

#[test]
fn other_methods_and_garbage_are_refused() {
    let preview = Preview::start(site());
    let post = send(preview.port(), "POST / HTTP/1.1\r\n\r\n");
    assert_eq!(post.status, 405);
    let garbage = send(preview.port(), "hello\r\n\r\n");
    assert_eq!(garbage.status, 400);
}

#[test]
fn an_open_page_is_told_to_reload_when_a_file_changes() {
    let root = site();
    let file = root.path().join("index.html");
    let preview = Preview::start(root);
    let mut stream = TcpStream::connect(("127.0.0.1", preview.port())).unwrap();
    stream
        .write_all(
            format!(
                "GET /{}/__herdr/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                preview.token,
                preview.port()
            )
            .as_bytes(),
        )
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while !line.starts_with("retry:") {
        line.clear();
        reader.read_line(&mut line).unwrap();
    }
    // The page loads the file it shows, which is what the watcher follows.
    assert_eq!(preview.get("/index.html", "").status, 200);
    // Let the watcher take its first look before the change.
    std::thread::sleep(Duration::from_millis(400));
    std::fs::write(&file, b"<html><body>changed</body></html>").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line.trim() == "data: reload" {
            break;
        }
        assert!(Instant::now() < deadline, "no reload event");
    }
}

#[test]
fn a_change_to_a_file_no_page_loaded_does_not_reload() {
    let root = site();
    let unrelated = root.path().join("media/clip.bin");
    let preview = Preview::start(root);
    assert_eq!(preview.get("/index.html", "").status, 200);
    let mut stream = TcpStream::connect(("127.0.0.1", preview.port())).unwrap();
    stream
        .write_all(
            format!(
                "GET /{}/__herdr/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                preview.token,
                preview.port()
            )
            .as_bytes(),
        )
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(1500)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while !line.starts_with("retry:") {
        line.clear();
        reader.read_line(&mut line).unwrap();
    }
    std::thread::sleep(Duration::from_millis(400));
    std::fs::write(&unrelated, b"changed").unwrap();
    let mut received = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
        received.push_str(&line);
        line.clear();
    }
    assert!(!received.contains("data: reload"), "{received}");
}

#[test]
fn a_page_loading_another_file_does_not_reload_itself() {
    let preview = Preview::start(site());
    assert_eq!(preview.get("/index.html", "").status, 200);
    let mut reader = open_events(&preview, Duration::from_millis(1800));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(preview.get("/media/clip.bin", "").status, 200);
    let mut received = String::new();
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
        received.push_str(&line);
        line.clear();
    }
    assert!(!received.contains("data: reload"), "{received}");
}

/// Connects to the event stream and reads past its opening `retry:` line.
fn open_events(preview: &Preview, timeout: Duration) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(("127.0.0.1", preview.port())).unwrap();
    stream
        .write_all(
            format!(
                "GET /{}/__herdr/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                preview.token,
                preview.port()
            )
            .as_bytes(),
        )
        .unwrap();
    stream.set_read_timeout(Some(timeout)).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while !line.starts_with("retry:") {
        line.clear();
        reader.read_line(&mut line).unwrap();
    }
    reader
}

#[test]
fn a_watcher_that_cannot_start_leaves_no_listener_behind() {
    let root = site();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    crate::thread_spawn::test_hook::fail_spawns_after(1, 1);
    let started = PreviewServer::start_on(
        listener,
        Box::new(LocalSource::new(root.path()).unwrap()),
        Duration::from_millis(100),
        ListingSwitch::new(true),
    );
    assert!(started.is_err());
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(Instant::now() < deadline, "the accept thread still listens");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_folder_redirect_does_not_watch_the_folder_itself() {
    let root = site();
    let unrelated = root.path().join("docs/unloaded.txt");
    let preview = Preview::start(root);
    assert_eq!(preview.get("/index.html", "").status, 200);
    assert_eq!(preview.get("/docs", "").status, 301);
    let mut reader = open_events(&preview, Duration::from_millis(1800));
    std::thread::sleep(Duration::from_millis(400));
    std::fs::write(&unrelated, b"never requested").unwrap();
    let mut received = String::new();
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
        received.push_str(&line);
        line.clear();
    }
    assert!(!received.contains("data: reload"), "{received}");
}

#[test]
fn a_stopped_preview_refuses_new_connections() {
    let preview = Preview::start(site());
    let port = preview.port();
    preview.server.stop();
    assert!(preview.server.is_stopped());
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(Instant::now() < deadline, "listener still open");
        std::thread::sleep(Duration::from_millis(50));
    }
}
