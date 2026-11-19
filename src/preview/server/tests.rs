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
    cookie: String,
    _root: TempDir,
}

impl Preview {
    fn start(root: TempDir) -> Self {
        let source = LocalSource::new(root.path()).unwrap();
        let server = PreviewServer::start(Box::new(source), Duration::from_millis(100)).unwrap();
        let token = server
            .url("")
            .split_once("herdr-token=")
            .unwrap()
            .1
            .to_owned();
        let cookie = format!("herdr-preview-{}={token}", server.port());
        Self {
            server,
            cookie,
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
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n{extra}\r\n",
                self.port(),
                self.cookie
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
fn the_url_signs_the_browser_in_with_a_cookie_and_drops_the_token() {
    let preview = Preview::start(site());
    let url = preview.server.url("docs/README.md");
    let path = url.split_once(&format!("{}", preview.port())).unwrap().1;
    let reply = send(
        preview.port(),
        &format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            preview.port()
        ),
    );
    assert_eq!(reply.status, 303);
    assert_eq!(reply.header("location"), Some("/docs/README.md"));
    let cookie = reply.header("set-cookie").unwrap();
    assert!(cookie.starts_with(&preview.cookie), "{cookie}");
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
}

#[test]
fn a_browser_without_the_cookie_or_from_another_host_is_turned_away() {
    let preview = Preview::start(site());
    let stranger = send(
        preview.port(),
        &format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            preview.port()
        ),
    );
    assert_eq!(stranger.status, 403);
    let rebound = send(
        preview.port(),
        &format!(
            "GET / HTTP/1.1\r\nHost: evil.example:{}\r\nCookie: {}\r\n\r\n",
            preview.port(),
            preview.cookie
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
    assert_eq!(folder.header("location"), Some("/docs/"));
    let missing = preview.get("/docs/", "Sec-Fetch-Dest: document\r\n");
    assert_eq!(missing.status, 404);
    assert!(missing.text().contains(crate::preview::page::RELOAD_TAG));
}

#[test]
fn herdr_assets_are_served_and_a_head_request_has_no_body() {
    let preview = Preview::start(site());
    let asset = preview.get("/__herdr/assets/viewer.js", "");
    assert_eq!(asset.status, 200);
    assert!(!asset.body.is_empty());
    let head = send(
        preview.port(),
        &format!(
            "HEAD / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
            preview.port(),
            preview.cookie
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
                "GET /__herdr/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
                preview.port(),
                preview.cookie
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
                "GET /__herdr/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
                preview.port(),
                preview.cookie
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
                "GET /__herdr/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
                preview.port(),
                preview.cookie
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
