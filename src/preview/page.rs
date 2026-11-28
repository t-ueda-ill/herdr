//! HTML herdr writes itself: the viewer page around a rendered format, the
//! reload hook added to HTML files, short message pages, and the embedded
//! scripts and styles they load.

use super::http::percent_encode_path;
use super::route::{Format, RAW_QUERY};

/// Where herdr's own files are served. A preview root may hold a folder of
/// this name; herdr's files win, which only hides that folder.
pub(crate) const ASSET_PREFIX: &str = "/__herdr/assets/";
pub(crate) const EVENTS_PATH: &str = "/__herdr/events";

pub(crate) const RELOAD_TAG: &str = r#"<script src="/__herdr/assets/reload.js"></script>"#;

pub(crate) struct Asset {
    pub(crate) content_type: &'static str,
    pub(crate) body: &'static [u8],
}

macro_rules! vendor {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/preview/vendor/",
            $name
        ))
    };
}

macro_rules! own {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/preview/",
            $name
        ))
    };
}

const JS: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const TEXT: &str = "text/plain; charset=utf-8";

pub(crate) fn asset(name: &str) -> Option<Asset> {
    let (content_type, body): (&'static str, &'static [u8]) = match name {
        "viewer.js" => (JS, own!("viewer.js")),
        "viewer.css" => (CSS, own!("viewer.css")),
        "reload.js" => (JS, own!("reload.js")),
        "markdown-it.min.js" => (JS, vendor!("markdown-it.min.js")),
        "markdown-it-footnote.min.js" => (JS, vendor!("markdown-it-footnote.min.js")),
        "highlight.min.js" => (JS, vendor!("highlight.min.js")),
        "highlight-github.min.css" => (CSS, vendor!("highlight-github.min.css")),
        "highlight-github-dark.min.css" => (CSS, vendor!("highlight-github-dark.min.css")),
        "mermaid.min.js" => (JS, vendor!("mermaid.min.js")),
        "markdown-it.LICENSE" => (TEXT, vendor!("markdown-it.LICENSE")),
        "markdown-it-footnote.LICENSE" => (TEXT, vendor!("markdown-it-footnote.LICENSE")),
        "highlight.LICENSE" => (TEXT, vendor!("highlight.LICENSE")),
        "mermaid.LICENSE" => (TEXT, vendor!("mermaid.LICENSE")),
        _ => return None,
    };
    Some(Asset { content_type, body })
}

pub(crate) fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            ch => out.push(ch),
        }
    }
    out
}

/// The page shown when the browser opens a format herdr renders. The file
/// itself is fetched by `viewer.js`, so the page never embeds its bytes.
pub(crate) fn viewer_page(relative: &str, format: Format) -> String {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    let src = format!("{}?{RAW_QUERY}=1", percent_encode_path(name));
    let language = match format {
        Format::Code(language) => format!(r#" data-language="{language}""#),
        _ => String::new(),
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<link rel="stylesheet" href="/__herdr/assets/viewer.css">
<link rel="stylesheet" href="/__herdr/assets/highlight-github.min.css" media="(prefers-color-scheme: light)">
<link rel="stylesheet" href="/__herdr/assets/highlight-github-dark.min.css" media="(prefers-color-scheme: dark)">
</head>
<body>
<main id="herdr-view" data-format="{format}" data-src="{src}"{language}></main>
<script src="/__herdr/assets/markdown-it.min.js"></script>
<script src="/__herdr/assets/markdown-it-footnote.min.js"></script>
<script src="/__herdr/assets/highlight.min.js"></script>
<script src="/__herdr/assets/viewer.js"></script>
{RELOAD_TAG}
</body>
</html>
"#,
        title = escape_html(name),
        format = format.name(),
        src = escape_html(&src),
    )
}

/// The Content-Security-Policy of viewer pages. Only herdr's embedded scripts
/// run: a README from a cloned repository may hold `<script>` or `onerror`
/// markup, and the file's own folder may hold scripts it would load.
///
/// Why styles stay inline-capable: mermaid writes its diagrams' styles inline.
/// Why https images: README badges and screenshots link off-site. Why not
/// plain http: another service on this machine's loopback is an http origin,
/// and a request to it must not carry anything from the preview.
pub(crate) fn viewer_policy(host: &str) -> String {
    format!(
        "default-src 'self'; script-src http://{host}/__herdr/assets/; \
         style-src 'self' 'unsafe-inline'; img-src 'self' https: data: blob:; font-src 'self' data:; \
         connect-src 'self'; media-src 'self'; object-src 'none'; frame-src 'none'; \
         base-uri 'none'; form-action 'none'"
    )
}

/// Adds the reload hook before the last `</body>`, or at the end. An HTML
/// file keeps its own scripts; only the hook is added.
pub(crate) fn inject_reload(page: &[u8]) -> Vec<u8> {
    let lower = page.to_ascii_lowercase();
    let position = lower
        .windows(7)
        .rposition(|window| window == b"</body>")
        .unwrap_or(page.len());
    let mut out = Vec::with_capacity(page.len() + RELOAD_TAG.len());
    out.extend_from_slice(&page[..position]);
    out.extend_from_slice(RELOAD_TAG.as_bytes());
    out.extend_from_slice(&page[position..]);
    out
}

/// The page of a folder without an `index.html`: folders first, then files,
/// each linked relative to the folder so the token in the URL stays.
pub(crate) fn listing_page(relative: &str, listing: &super::source::Listing) -> String {
    let mut entries: Vec<_> = listing.entries.iter().collect();
    entries.sort_by(|a, b| b.folder.cmp(&a.folder).then_with(|| a.name.cmp(&b.name)));
    let mut items = String::new();
    if !relative.is_empty() {
        items.push_str("<li><a href=\"../\">../</a></li>\n");
    }
    for entry in entries {
        let slash = if entry.folder { "/" } else { "" };
        items.push_str(&format!(
            "<li><a href=\"./{href}{slash}\">{name}{slash}</a></li>\n",
            href = percent_encode_path(&entry.name),
            name = escape_html(&entry.name),
        ));
    }
    let more = if listing.truncated {
        format!(
            "<p>More entries are in this folder than the {} shown.</p>\n",
            super::source::LIST_LIMIT
        )
    } else {
        String::new()
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<link rel="stylesheet" href="/__herdr/assets/viewer.css"></head>
<body><main class="herdr-message herdr-listing"><h1>{title}</h1>
<ul>
{items}</ul>
{more}</main>
{RELOAD_TAG}
</body>
</html>
"#,
        title = escape_html(&format!("/{relative}")),
    )
}

/// The refusal of a request without the token. Why no link to the right
/// address: anything that can reach the port could then read the token.
pub(crate) fn token_missing_page() -> String {
    message_page(
        "This preview address is missing its token",
        "Preview addresses look like http://127.0.0.1:PORT/TOKEN/file. \
         Copy the full address from the url column of herdr preview list, or \
         open the preview from previews in the herdr menu.",
    )
}

pub(crate) fn message_page(title: &str, detail: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>{title}</title>
<link rel="stylesheet" href="/__herdr/assets/viewer.css"></head>
<body><main class="herdr-message"><h1>{title}</h1><p>{detail}</p></main>
{RELOAD_TAG}
</body>
</html>
"#,
        title = escape_html(title),
        detail = escape_html(detail),
    )
}

#[cfg(test)]
mod tests;
