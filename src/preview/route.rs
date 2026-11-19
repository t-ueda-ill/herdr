//! Which file a request names, how to label it, and whether the browser may
//! have it. Pure functions over a parsed request.

use super::http::Request;

/// Rendered by herdr's viewer page instead of shown as the raw file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Markdown,
    Mermaid,
    Csv,
    Tsv,
    Json,
    /// Source text highlighted as this highlight.js language.
    Code(&'static str),
}

impl Format {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Mermaid => "mermaid",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
            Self::Code(_) => "code",
        }
    }
}

/// The path below the preview root, or `Err` when it would leave the root.
/// A trailing `/` is kept so a directory can serve its index.
pub(crate) fn relative_path(path: &str) -> Result<String, ()> {
    if path.contains('\\') {
        return Err(());
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(()),
            part => parts.push(part),
        }
    }
    let mut relative = parts.join("/");
    if path.ends_with('/') && !relative.is_empty() {
        relative.push('/');
    }
    Ok(relative)
}

pub(crate) fn file_to_serve(relative: &str) -> String {
    if relative.is_empty() || relative.ends_with('/') {
        format!("{relative}index.html")
    } else {
        relative.to_owned()
    }
}

fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_ascii_lowercase(),
        _ => String::new(),
    }
}

pub(crate) fn content_type(path: &str) -> &'static str {
    match extension(path).as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "xml" => "application/xml",
        "txt" | "md" | "markdown" | "mmd" | "mermaid" | "csv" | "tsv" | "log" | "yml" | "yaml"
        | "toml" | "ini" | "rs" | "py" | "rb" | "go" | "ts" | "tsx" | "jsx" | "sh" | "c" | "h"
        | "cpp" | "hpp" | "java" | "kt" | "swift" | "sql" | "lua" | "php" => {
            "text/plain; charset=utf-8"
        }
        _ => "application/octet-stream",
    }
}

pub(crate) fn format_of(path: &str) -> Option<Format> {
    let format = match extension(path).as_str() {
        "md" | "markdown" | "mdown" | "mkd" => Format::Markdown,
        "mmd" | "mermaid" => Format::Mermaid,
        "csv" => Format::Csv,
        "tsv" => Format::Tsv,
        "json" | "geojson" | "jsonc" => Format::Json,
        "yml" | "yaml" => Format::Code("yaml"),
        // Why ini: highlight.js ships no TOML grammar and aliases it to ini.
        "toml" | "ini" | "cfg" => Format::Code("ini"),
        "rs" => Format::Code("rust"),
        "py" => Format::Code("python"),
        "rb" => Format::Code("ruby"),
        "go" => Format::Code("go"),
        "js" | "mjs" | "cjs" | "jsx" => Format::Code("javascript"),
        "ts" | "tsx" => Format::Code("typescript"),
        "sh" | "bash" | "zsh" => Format::Code("bash"),
        "c" | "h" => Format::Code("c"),
        "cpp" | "cc" | "hpp" | "hh" => Format::Code("cpp"),
        "java" => Format::Code("java"),
        "kt" | "kts" => Format::Code("kotlin"),
        "swift" => Format::Code("swift"),
        "sql" => Format::Code("sql"),
        "lua" => Format::Code("lua"),
        "php" => Format::Code("php"),
        "css" | "scss" | "less" => Format::Code("css"),
        "xml" => Format::Code("xml"),
        "diff" | "patch" => Format::Code("diff"),
        "txt" | "log" => Format::Code("plaintext"),
        _ => return None,
    };
    Some(format)
}

/// The query flag the viewer page adds when it fetches the file itself.
pub(crate) const RAW_QUERY: &str = "herdr-raw";

/// True for a top-level navigation, which gets the viewer page; a script,
/// image, or the viewer's own fetch gets the bytes.
pub(crate) fn wants_page(request: &Request) -> bool {
    if request.query_param(RAW_QUERY).is_some() {
        return false;
    }
    match request.header("sec-fetch-dest") {
        Some(destination) => destination == "document",
        None => request
            .header("accept")
            .is_some_and(|accept| accept.starts_with("text/html")),
    }
}

/// Why check the Host: a DNS-rebinding page names its own domain while it
/// talks to this loopback port.
pub(crate) fn host_allowed(request: &Request, port: u16) -> bool {
    let Some(host) = request.header("host") else {
        return false;
    };
    let expected = port.to_string();
    match host.rsplit_once(':') {
        Some((name, host_port)) => {
            host_port == expected && matches!(name, "127.0.0.1" | "localhost" | "[::1]")
        }
        None => false,
    }
}

pub(crate) const TOKEN_QUERY: &str = "herdr-token";

pub(crate) fn cookie_name(port: u16) -> String {
    format!("herdr-preview-{port}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// The token came in the URL; answer with a cookie and drop it from the URL.
    SignIn,
    Allowed,
    Denied,
}

/// Why a cookie rather than a path prefix: pages use root-absolute links like
/// `/style.css`, which a token in the path would break.
pub(crate) fn access(request: &Request, port: u16, token: &str) -> Access {
    if let Some(offered) = request.query_param(TOKEN_QUERY) {
        return if offered == token {
            Access::SignIn
        } else {
            Access::Denied
        };
    }
    if request.cookie(&cookie_name(port)) == Some(token) {
        Access::Allowed
    } else {
        Access::Denied
    }
}

#[cfg(test)]
mod tests;
