use super::*;
use crate::preview::route::Format;

#[test]
fn the_viewer_page_fetches_the_raw_file_next_to_it() {
    let html = viewer_page("docs/read me.md", Format::Markdown);
    assert!(html.contains(r#"data-format="markdown""#));
    assert!(html.contains(r#"data-src="read%20me.md?herdr-raw=1""#));
    assert!(html.contains("<title>read me.md</title>"));
    assert!(html.contains(r#"src="/__herdr/assets/viewer.js""#));
    assert!(html.contains(r#"src="/__herdr/assets/reload.js""#));
}

#[test]
fn the_viewer_page_escapes_the_file_name() {
    let html = viewer_page("a<b>&\".md", Format::Markdown);
    assert!(html.contains("<title>a&lt;b&gt;&amp;&quot;.md</title>"));
    assert!(!html.contains("<b>"));
}

#[test]
fn code_pages_name_their_highlight_language() {
    let html = viewer_page("src/main.rs", Format::Code("rust"));
    assert!(html.contains(r#"data-format="code""#));
    assert!(html.contains(r#"data-language="rust""#));
}

#[test]
fn viewer_pages_allow_only_herdr_scripts() {
    let policy = viewer_policy("127.0.0.1:4321");
    let script = policy
        .split(';')
        .map(str::trim)
        .find(|directive| directive.starts_with("script-src"))
        .unwrap();
    assert_eq!(script, "script-src http://127.0.0.1:4321/__herdr/assets/");
    assert!(policy.contains("object-src 'none'"));
    // Other loopback services are http origins; images may only come from
    // this preview, https, or inline data.
    assert!(policy.contains("img-src 'self' https: data: blob:"));
    assert!(policy.contains("frame-src 'none'"));
    assert!(policy.contains("base-uri 'none'"));
}

#[test]
fn the_reload_script_goes_before_the_closing_body_tag() {
    let page = b"<html><body><p>x</p></BODY></html>";
    let injected = String::from_utf8(inject_reload(page)).unwrap();
    assert_eq!(
        injected,
        format!("<html><body><p>x</p>{RELOAD_TAG}</BODY></html>")
    );
}

#[test]
fn a_page_without_a_body_tag_gets_the_script_appended() {
    let injected = String::from_utf8(inject_reload(b"<p>fragment</p>")).unwrap();
    assert_eq!(injected, format!("<p>fragment</p>{RELOAD_TAG}"));
}

#[test]
fn the_last_body_tag_wins_when_a_script_mentions_one() {
    let page = b"<body><script>x='</body>'</script></body>";
    let injected = String::from_utf8(inject_reload(page)).unwrap();
    assert!(injected.ends_with(&format!("{RELOAD_TAG}</body>")));
}

#[test]
fn every_asset_the_pages_reference_is_embedded() {
    for name in [
        "viewer.js",
        "viewer.css",
        "reload.js",
        "markdown-it.min.js",
        "markdown-it-footnote.min.js",
        "highlight.min.js",
        "highlight-github.min.css",
        "highlight-github-dark.min.css",
        "mermaid.min.js",
    ] {
        let asset = asset(name).unwrap_or_else(|| panic!("missing asset {name}"));
        assert!(!asset.body.is_empty(), "{name}");
    }
    assert_eq!(
        asset("viewer.js").unwrap().content_type,
        "text/javascript; charset=utf-8"
    );
    assert!(asset("../Cargo.toml").is_none());
}

#[test]
fn a_message_page_escapes_its_text() {
    let html = message_page("Not found", "no <file>");
    assert!(html.contains("no &lt;file&gt;"));
}

#[test]
fn a_listing_links_folders_first_relative_to_the_folder_and_escapes_names() {
    use crate::preview::source::{DirEntry, Listing};
    let entry = |name: &str, folder| DirEntry {
        name: name.to_owned(),
        folder,
    };
    let listing = Listing {
        entries: vec![
            entry("b.md", false),
            entry("a<b>.txt", false),
            entry("x:y", false),
            entry("sub", true),
        ],
        truncated: false,
    };
    let html = listing_page("docs/", &listing);
    assert!(html.contains("<title>/docs/</title>"));
    assert!(html.contains(r#"href="../""#));
    let sub = html.find(r#"href="./sub/""#).unwrap();
    let a = html.find(r#"href="./a%3Cb%3E.txt""#).unwrap();
    let b = html.find(r#"href="./b.md""#).unwrap();
    assert!(sub < a && a < b, "{html}");
    // Why `./` and encoding: `x:y` alone would read as a URL scheme.
    assert!(html.contains(r#"href="./x%3Ay""#));
    assert!(html.contains("a&lt;b&gt;.txt"));
    assert!(!html.contains("<b>"));
    assert!(html.contains(RELOAD_TAG));
}

#[test]
fn the_root_listing_has_no_parent_link_and_a_cut_listing_says_so() {
    use crate::preview::source::Listing;
    let html = listing_page(
        "",
        &Listing {
            entries: Vec::new(),
            truncated: true,
        },
    );
    assert!(html.contains("<title>/</title>"));
    assert!(!html.contains(r#"href="../""#));
    assert!(html.contains("More entries"));
}

#[test]
fn the_page_without_the_token_says_how_to_get_the_full_address() {
    let html = token_missing_page();
    assert!(html.contains("herdr preview list"));
    assert!(html.contains("previews"));
    assert!(!html.contains("closed"));
}
