//! One preview: a loopback listener, a thread per connection, and a watcher
//! that tells open pages to reload.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::http::{self, encode_head, ByteRange, Method, ParseError, Request};
use super::page::{self, ASSET_PREFIX, EVENTS_PATH};
use super::route::{self, Access};
use super::source::{FileSource, ReadError, Window};

/// Open connections beyond this get 503; each holds a thread.
const MAX_CONNECTIONS: usize = 64;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often an idle event stream proves it is still open.
const KEEPALIVE: Duration = Duration::from_secs(15);
const STOP_POLL: Duration = Duration::from_millis(100);

struct Shared {
    source: Box<dyn FileSource>,
    port: u16,
    token: String,
    watch_every: Duration,
    /// Bumped each time the watcher sees a change.
    generation: Mutex<u64>,
    changed: Condvar,
    stopping: AtomicBool,
    connections: AtomicUsize,
    /// Pages listening for reloads. The watcher only polls while one is,
    /// so an unopened preview never runs commands in a remote environment.
    listeners: AtomicUsize,
    /// Paths the browser asked for, in first-asked order: what the watcher
    /// follows. Missing ones stay, so a page reloads when its 404 appears.
    served: Mutex<Vec<String>>,
}

impl Shared {
    fn remember(&self, path: &str) {
        let Ok(mut served) = self.served.lock() else {
            return;
        };
        if served.len() < super::source::WATCH_PATH_LIMIT
            && !served.iter().any(|known| known == path)
        {
            served.push(path.to_owned());
        }
    }
}

pub(crate) struct PreviewServer {
    shared: Arc<Shared>,
}

impl PreviewServer {
    pub(crate) fn start(source: Box<dyn FileSource>, watch_every: Duration) -> io::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared {
            source,
            port,
            token: new_token(),
            watch_every,
            generation: Mutex::new(0),
            changed: Condvar::new(),
            stopping: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            listeners: AtomicUsize::new(0),
            served: Mutex::new(Vec::new()),
        });
        let accept = Arc::clone(&shared);
        crate::thread_spawn::spawn_named("herdr-preview-accept", move || {
            accept_loop(&listener, &accept);
        })?;
        let watch = Arc::clone(&shared);
        crate::thread_spawn::spawn_named("herdr-preview-watch", move || watch_loop(&watch))?;
        Ok(Self { shared })
    }

    pub(crate) fn port(&self) -> u16 {
        self.shared.port
    }

    /// The address that opens `entry`, a path below the root (`""` for the
    /// root itself). It carries the session token, so it is a secret.
    pub(crate) fn url(&self, entry: &str) -> String {
        format!(
            "http://127.0.0.1:{}/{}/{}",
            self.shared.port,
            self.shared.token,
            http::percent_encode_path(entry)
        )
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.shared.stopping.load(Ordering::SeqCst)
    }

    pub(crate) fn stop(&self) {
        if self.shared.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared.changed.notify_all();
        // Wakes the blocking accept so its thread sees the flag and exits.
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.shared.port));
    }
}

impl Drop for PreviewServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Why not a random-number crate: `RandomState` is seeded from the operating
/// system's randomness, which is all a loopback session token needs.
fn new_token() -> String {
    use std::hash::BuildHasher;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_nanos())
        .unwrap_or_default();
    let first = std::collections::hash_map::RandomState::new().hash_one(nanos);
    let second =
        std::collections::hash_map::RandomState::new().hash_one((first, std::process::id()));
    format!("{first:016x}{second:016x}")
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        let Ok(mut stream) = stream else {
            continue;
        };
        if shared.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            shared.connections.fetch_sub(1, Ordering::SeqCst);
            let _ = write_simple(&mut stream, Method::Get, 503, "too many connections");
            continue;
        }
        let connection = Arc::clone(shared);
        let spawned = crate::thread_spawn::spawn_named("herdr-preview-conn", move || {
            serve(stream, &connection);
            connection.connections.fetch_sub(1, Ordering::SeqCst);
        });
        if let Err(err) = spawned {
            shared.connections.fetch_sub(1, Ordering::SeqCst);
            tracing::warn!(%err, "could not start a preview connection thread");
        }
    }
}

fn watch_loop(shared: &Shared) {
    // The fingerprint and how many paths it covered. Why the count: a page
    // that loads another file changes the set, not any file, and must not
    // reload for that.
    let mut last: Option<(usize, String)> = None;
    while !shared.stopping.load(Ordering::SeqCst) {
        if shared.listeners.load(Ordering::SeqCst) == 0 {
            // Nothing to tell; what changed meanwhile is already on the page
            // the next listener loads.
            last = None;
            std::thread::sleep(STOP_POLL);
            continue;
        }
        let served = shared
            .served
            .lock()
            .map(|served| served.clone())
            .unwrap_or_default();
        if !served.is_empty() {
            if let Ok(current) = shared.source.fingerprint(&served) {
                let changed = last
                    .as_ref()
                    .is_some_and(|(count, last)| *count == served.len() && *last != current);
                if changed {
                    if let Ok(mut generation) = shared.generation.lock() {
                        *generation += 1;
                    }
                    shared.changed.notify_all();
                }
                last = Some((served.len(), current));
            }
        }
        let mut waited = Duration::ZERO;
        while waited < shared.watch_every && !shared.stopping.load(Ordering::SeqCst) {
            std::thread::sleep(STOP_POLL);
            waited += STOP_POLL;
        }
    }
}

fn read_head(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(end) = http::head_end(&buffer) {
            buffer.truncate(end);
            return Some(buffer);
        }
        if buffer.len() > http::MAX_HEAD_BYTES {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

fn serve(mut stream: TcpStream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let Some(head) = read_head(&mut stream) else {
        let _ = write_simple(&mut stream, Method::Get, 400, "bad request");
        return;
    };
    let request = match http::parse_request(&head) {
        Ok(request) => request,
        Err(ParseError::MethodNotAllowed) => {
            let _ = write_simple(&mut stream, Method::Get, 405, "method not allowed");
            return;
        }
        Err(ParseError::BadRequest) => {
            let _ = write_simple(&mut stream, Method::Get, 400, "bad request");
            return;
        }
    };
    let events = match route::access(&request, &shared.token) {
        Access::Token { path } => path == EVENTS_PATH,
        Access::Referred => request.path == EVENTS_PATH,
        Access::Denied => false,
    };
    if events && route::host_allowed(&request, shared.port) {
        stream_events(stream, shared);
        return;
    }
    let response = respond(&request, shared);
    let _ = response.write(&mut stream, request.method);
}

struct Response {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Response {
    fn new(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![
                ("Content-Type", content_type.to_owned()),
                ("Cache-Control", "no-store".into()),
                ("X-Content-Type-Options", "nosniff".into()),
                // Keeps the token in the URL from leaving this origin.
                ("Referrer-Policy", "same-origin".into()),
            ],
            body,
        }
    }

    fn message(status: u16, title: &str, detail: &str) -> Self {
        Self::new(
            status,
            "text/html; charset=utf-8",
            page::message_page(title, detail).into_bytes(),
        )
    }

    fn redirect(status: u16, location: String) -> Self {
        let mut response = Self::new(status, "text/plain; charset=utf-8", Vec::new());
        response.headers.push(("Location", location));
        response
    }

    fn write(mut self, stream: &mut TcpStream, method: Method) -> io::Result<()> {
        self.headers
            .push(("Content-Length", self.body.len().to_string()));
        stream.write_all(&encode_head(self.status, &self.headers))?;
        if method == Method::Get {
            stream.write_all(&self.body)?;
        }
        stream.flush()
    }
}

fn write_simple(stream: &mut TcpStream, method: Method, status: u16, text: &str) -> io::Result<()> {
    Response::new(
        status,
        "text/plain; charset=utf-8",
        text.as_bytes().to_vec(),
    )
    .write(stream, method)
}

fn respond(request: &Request, shared: &Shared) -> Response {
    if !route::host_allowed(request, shared.port) {
        return Response::new(403, "text/plain; charset=utf-8", b"unknown host".to_vec());
    }
    if let Some(name) = request.path.strip_prefix(ASSET_PREFIX) {
        return match page::asset(name) {
            Some(asset) => Response::new(200, asset.content_type, asset.body.to_vec()),
            None => Response::new(404, "text/plain; charset=utf-8", b"not found".to_vec()),
        };
    }
    let prefix = format!("/{}", shared.token);
    let path = match route::access(request, &shared.token) {
        Access::Denied => {
            return Response::message(
                403,
                "Preview closed to this browser",
                "Open the preview again from herdr.",
            )
        }
        // Why redirect the bare token: relative links need the trailing slash.
        Access::Token { path } if path.is_empty() => {
            return Response::redirect(301, format!("{prefix}/"));
        }
        Access::Token { path } => path,
        // Why move every referred request under the token, not only pages: a
        // page loaded without it loses the token for its own links, and a
        // stylesheet or module does for what it loads in turn.
        Access::Referred => {
            let query = request
                .query
                .as_deref()
                .map(|query| format!("?{query}"))
                .unwrap_or_default();
            return Response::redirect(
                if route::is_navigation(request) {
                    303
                } else {
                    307
                },
                format!(
                    "{prefix}{}{query}",
                    http::percent_encode_path(&request.path)
                ),
            );
        }
    };
    let Ok(relative) = route::relative_path(&path) else {
        return Response::message(403, "Outside the preview", &path);
    };
    let file = route::file_to_serve(&relative);
    shared.remember(&file);
    let location = Location { prefix, path };
    let page = route::wants_page(request);
    if page {
        if let Some(format) = route::format_of(&file) {
            return match shared.source.read(&file, Window::Probe) {
                Ok(_) => {
                    let mut response = Response::new(
                        200,
                        "text/html; charset=utf-8",
                        page::viewer_page(&file, format).into_bytes(),
                    );
                    let host = request.header("host").unwrap_or_default();
                    response
                        .headers
                        .push(("Content-Security-Policy", page::viewer_policy(host)));
                    response
                }
                Err(error) => read_failure(error, &location, &file, page),
            };
        }
    }
    let content_type = route::content_type(&file);
    let html = content_type.starts_with("text/html");
    let range_header = request.header("range").filter(|_| !html);
    if let Some(range_header) = range_header {
        let size = match shared.source.read(&file, Window::Probe) {
            Ok(probe) => probe.size,
            Err(error) => return read_failure(error, &location, &file, page),
        };
        return match http::parse_range(Some(range_header), size) {
            ByteRange::Unsatisfiable => {
                let mut response = Response::new(416, content_type, Vec::new());
                response
                    .headers
                    .push(("Content-Range", format!("bytes */{size}")));
                response
            }
            ByteRange::Full => {
                whole_file(shared, request, &location, &file, content_type, false, page)
            }
            ByteRange::Partial { start, end } => {
                match shared.source.read(
                    &file,
                    Window::Slice {
                        start,
                        len: end - start,
                    },
                ) {
                    Ok(slice) => {
                        let last = start + slice.bytes.len() as u64;
                        let mut response = Response::new(206, content_type, slice.bytes);
                        response.headers.push((
                            "Content-Range",
                            format!("bytes {start}-{}/{}", last.saturating_sub(1), slice.size),
                        ));
                        response.headers.push(("Accept-Ranges", "bytes".into()));
                        response
                    }
                    Err(error) => read_failure(error, &location, &file, page),
                }
            }
        };
    }
    whole_file(shared, request, &location, &file, content_type, html, page)
}

fn whole_file(
    shared: &Shared,
    request: &Request,
    location: &Location,
    file: &str,
    content_type: &str,
    html: bool,
    page: bool,
) -> Response {
    match shared.source.read(file, Window::Whole) {
        Ok(whole) => {
            let body = if html && request.query_param(route::RAW_QUERY).is_none() {
                page::inject_reload(&whole.bytes)
            } else {
                whole.bytes
            };
            let mut response = Response::new(200, content_type, body);
            response.headers.push(("Accept-Ranges", "bytes".into()));
            response
        }
        Err(error) => read_failure(error, location, file, page),
    }
}

/// Where a request landed: the URL prefix that keeps the token, and the path
/// below the root, for redirects that must keep both.
struct Location {
    prefix: String,
    path: String,
}

fn read_failure(error: ReadError, location: &Location, file: &str, page: bool) -> Response {
    match error {
        ReadError::Directory if !location.path.ends_with('/') => Response::redirect(
            301,
            format!(
                "{}{}/",
                location.prefix,
                http::percent_encode_path(&location.path)
            ),
        ),
        ReadError::Directory | ReadError::NotFound if page => Response::message(
            404,
            "Not found",
            &format!("{file} does not exist yet. This page reloads when it appears."),
        ),
        ReadError::Directory | ReadError::NotFound => {
            Response::new(404, "text/plain; charset=utf-8", b"not found".to_vec())
        }
        ReadError::Forbidden => Response::message(403, "Not readable", file),
        ReadError::TooLarge => Response::message(
            413,
            "Too large to preview",
            &format!("{file} is larger than the preview sends at once."),
        ),
        ReadError::Failed(detail) => {
            tracing::warn!(%detail, file, "preview read failed");
            Response::message(502, "Could not read the file", &detail)
        }
    }
}

fn stream_events(mut stream: TcpStream, shared: &Shared) {
    let head = encode_head(
        200,
        &[
            ("Content-Type", "text/event-stream".into()),
            ("Cache-Control", "no-store".into()),
        ],
    );
    if stream.write_all(&head).is_err() || stream.write_all(b"retry: 1000\n\n").is_err() {
        return;
    }
    shared.listeners.fetch_add(1, Ordering::SeqCst);
    let mut seen = shared.generation.lock().map(|generation| *generation).ok();
    while let Some(last) = seen {
        // Why the lock ends before writing: a slow browser must not hold up
        // the watcher or the other pages.
        let current = shared
            .generation
            .lock()
            .ok()
            .and_then(|generation| {
                shared
                    .changed
                    .wait_timeout_while(generation, KEEPALIVE, |current| {
                        *current == last && !shared.stopping.load(Ordering::SeqCst)
                    })
                    .ok()
            })
            .map(|(generation, _)| *generation);
        let Some(current) = current else {
            break;
        };
        if shared.stopping.load(Ordering::SeqCst) {
            break;
        }
        let message: &[u8] = if current == last {
            b": keepalive\n\n"
        } else {
            b"data: reload\n\n"
        };
        if stream
            .write_all(message)
            .and_then(|()| stream.flush())
            .is_err()
        {
            break;
        }
        seen = Some(current);
    }
    shared.listeners.fetch_sub(1, Ordering::SeqCst);
}

#[cfg(test)]
mod tests;
