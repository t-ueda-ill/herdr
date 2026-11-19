//! The slice of HTTP/1.1 a browser needs from a static file server: `GET` and
//! `HEAD`, single byte ranges, and one response per connection. Pure
//! functions; the sockets live in `server`.

/// Largest request head accepted. Browsers send a few hundred bytes.
pub(crate) const MAX_HEAD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Head,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) method: Method,
    /// Percent-decoded, always starting with `/`, without the query.
    pub(crate) path: String,
    /// The raw query after `?`, still encoded.
    pub(crate) query: Option<String>,
    /// Header names in lowercase, in arrival order.
    pub(crate) headers: Vec<(String, String)>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn query_param(&self, name: &str) -> Option<String> {
        self.query.as_deref()?.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then(|| percent_decode(value).unwrap_or_default())
        })
    }

    pub(crate) fn cookie(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key == "cookie")
            .flat_map(|(_, value)| value.split(';'))
            .find_map(|pair| {
                let (key, value) = pair.trim().split_once('=')?;
                (key == name).then_some(value)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseError {
    BadRequest,
    MethodNotAllowed,
}

/// Offset just past the blank line that ends the head, once it has arrived.
pub(crate) fn head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

pub(crate) fn parse_request(head: &[u8]) -> Result<Request, ParseError> {
    let head = std::str::from_utf8(head).map_err(|_| ParseError::BadRequest)?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or(ParseError::BadRequest)?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ParseError::BadRequest);
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(ParseError::BadRequest);
    }
    let method = match method {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        _ => return Err(ParseError::MethodNotAllowed),
    };
    let (raw_path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query.to_owned())),
        None => (target, None),
    };
    let path = percent_decode(raw_path).ok_or(ParseError::BadRequest)?;
    let mut headers = Vec::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(ParseError::BadRequest)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok(Request {
        method,
        path,
        query,
        headers,
    })
}

/// `None` for a malformed escape, a NUL, or bytes that are not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    if out.contains(&0) {
        return None;
    }
    String::from_utf8(out).ok()
}

/// Encodes everything but unreserved characters and `/`, for building URLs
/// from relative paths.
pub(crate) fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// What part of a file a request asked for. `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByteRange {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

/// Why not multiple ranges: browsers ask for one when seeking media, and a
/// server may always answer a range request with the whole file instead.
pub(crate) fn parse_range(header: Option<&str>, len: u64) -> ByteRange {
    let Some(spec) = header.and_then(|header| header.trim().strip_prefix("bytes=")) else {
        return ByteRange::Full;
    };
    if spec.contains(',') {
        return ByteRange::Full;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return ByteRange::Full;
    };
    let (start, end) = match (first.trim(), last.trim()) {
        ("", suffix) => match suffix.parse::<u64>() {
            Ok(0) => return ByteRange::Unsatisfiable,
            Ok(count) => (len.saturating_sub(count), len),
            Err(_) => return ByteRange::Full,
        },
        (start, "") => match start.parse::<u64>() {
            Ok(start) => (start, len),
            Err(_) => return ByteRange::Full,
        },
        (start, last) => match (start.parse::<u64>(), last.parse::<u64>()) {
            (Ok(start), Ok(last)) if start <= last => (start, last.saturating_add(1).min(len)),
            _ => return ByteRange::Full,
        },
    };
    if start >= len {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Partial { start, end }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        301 => "Moved Permanently",
        303 => "See Other",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        416 => "Range Not Satisfiable",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// The status line and headers. Every response closes its connection, so a
/// handler thread never waits on an idle keep-alive.
pub(crate) fn encode_head(status: u16, headers: &[(&str, String)]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    head.into_bytes()
}

#[cfg(test)]
mod tests;
