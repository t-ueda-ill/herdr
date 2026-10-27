//! `[ui.files.details]`: the lines the files details band shows for the
//! selection, as `{token}` templates the client expands.
use serde::Deserialize;

use super::window_title::{parse_template_parts, TemplatePart};

pub(crate) const DEFAULT_FILE_TEMPLATE: &str = "{name}\n{size} · {mtime} · {mode} {owner}";
pub(crate) const DEFAULT_DIRECTORY_TEMPLATE: &str =
    "{name}/\n{children} · {mtime} · {mode} {owner}";
pub(crate) const DEFAULT_MULTI_TEMPLATE: &str = "{count} · {total_size}";
pub(crate) const DEFAULT_MATCH_TEMPLATE: &str = "{path}:{line}\n{context}";
pub(crate) const DEFAULT_MAX_LINES: u16 = 8;

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct UiFilesConfig {
    /// Lines shown below the files list for the selection.
    pub details: FilesDetailsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct FilesDetailsConfig {
    /// Shown for a selected file. Empty hides the band for files.
    pub file: String,
    /// Shown for a selected directory, `..`, or nothing selected (the listed folder).
    pub directory: String,
    /// Shown while entries are marked.
    pub multi: String,
    /// Shown for a selected content search hit.
    #[serde(rename = "match")]
    pub match_template: String,
    /// Most lines the band takes; it never takes more than a third of the files body.
    pub max_lines: u16,
}

impl Default for FilesDetailsConfig {
    fn default() -> Self {
        Self {
            file: DEFAULT_FILE_TEMPLATE.to_owned(),
            directory: DEFAULT_DIRECTORY_TEMPLATE.to_owned(),
            multi: DEFAULT_MULTI_TEMPLATE.to_owned(),
            match_template: DEFAULT_MATCH_TEMPLATE.to_owned(),
            max_lines: DEFAULT_MAX_LINES,
        }
    }
}

/// A value the client substitutes into a details template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsToken {
    Name,
    Path,
    Kind,
    /// `12.3 KiB`.
    Size,
    SizeBytes,
    /// Local `YYYY-MM-DD HH:MM`.
    Mtime,
    /// UTC RFC 3339.
    MtimeIso,
    /// `hidden` for a dotfile, else empty.
    Hidden,
    /// `ignored` for a Git-ignored entry, else empty.
    Ignored,
    LinkTarget,
    /// `rwxr-xr-x`.
    Mode,
    Owner,
    /// `12 items`.
    Children,
    /// `3 selected`.
    Count,
    /// Size of the marked files; directories are not walked.
    TotalSize,
    Line,
    /// The lines around a search hit, `grep -C` style.
    Context,
}

impl DetailsToken {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "name" => Self::Name,
            "path" => Self::Path,
            "kind" => Self::Kind,
            "size" => Self::Size,
            "size_bytes" => Self::SizeBytes,
            "mtime" => Self::Mtime,
            "mtime_iso" => Self::MtimeIso,
            "hidden" => Self::Hidden,
            "ignored" => Self::Ignored,
            "link_target" => Self::LinkTarget,
            "mode" => Self::Mode,
            "owner" => Self::Owner,
            "children" => Self::Children,
            "count" => Self::Count,
            "total_size" => Self::TotalSize,
            "line" => Self::Line,
            "context" => Self::Context,
            _ => return None,
        })
    }
}

/// One template, already split into lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DetailsTemplate {
    lines: Vec<Vec<TemplatePart<DetailsToken>>>,
}

impl DetailsTemplate {
    /// An empty template has no lines and hides the band.
    pub fn parse(template: &str) -> Result<Self, String> {
        let mut lines = vec![Vec::new()];
        for part in parse_template_parts(template, DetailsToken::parse)? {
            match part {
                TemplatePart::Literal(text) => {
                    let mut pieces = text.split('\n');
                    if let Some(first) = pieces.next() {
                        push_literal(lines.last_mut(), first);
                    }
                    for piece in pieces {
                        lines.push(Vec::new());
                        push_literal(lines.last_mut(), piece);
                    }
                }
                token => lines
                    .last_mut()
                    .into_iter()
                    .for_each(|line| line.push(token.clone())),
            }
        }
        if lines.iter().all(Vec::is_empty) {
            lines.clear();
        }
        Ok(Self { lines })
    }

    /// Expands the template. A literal of spaces and punctuation next to an
    /// empty token is dropped, so `{a} · {b}` with no `b` reads `a`, and one
    /// left between two values stays once; a line whose tokens are all empty
    /// is dropped. A token value may hold several lines.
    pub fn render(&self, value: impl Fn(DetailsToken) -> String) -> Vec<String> {
        enum Piece<'a> {
            Text(std::borrow::Cow<'a, str>),
            /// A separator literal, and whether an empty token is beside it.
            Separator(&'a str, bool),
        }
        let mut rendered = Vec::new();
        for line in &self.lines {
            let mut pieces: Vec<Piece<'_>> = Vec::new();
            let mut after_empty = false;
            let mut tokens = 0;
            let mut filled = 0;
            for part in line {
                match part {
                    TemplatePart::Literal(literal) if is_separator(literal) => {
                        pieces.push(Piece::Separator(literal, after_empty));
                    }
                    TemplatePart::Literal(literal) => {
                        pieces.push(Piece::Text(literal.as_str().into()));
                        after_empty = false;
                    }
                    TemplatePart::Token(token) => {
                        tokens += 1;
                        let value = value(*token);
                        if value.is_empty() {
                            if let Some(Piece::Separator(_, beside_empty)) = pieces.last_mut() {
                                *beside_empty = true;
                            }
                            after_empty = true;
                            continue;
                        }
                        filled += 1;
                        after_empty = false;
                        pieces.push(Piece::Text(value.into()));
                    }
                }
            }
            if tokens > 0 && filled == 0 {
                continue;
            }
            // Separators left side by side by dropped tokens act as the first.
            let mut merged: Vec<Piece<'_>> = Vec::new();
            for piece in pieces {
                match (merged.last_mut(), piece) {
                    (Some(Piece::Separator(_, beside)), Piece::Separator(_, also)) => {
                        *beside |= also;
                    }
                    (_, piece) => merged.push(piece),
                }
            }
            let last = merged.len().saturating_sub(1);
            let mut text = String::new();
            for (index, piece) in merged.iter().enumerate() {
                match piece {
                    Piece::Text(value) => text.push_str(value),
                    // Between two values a separator always stays; at an end it
                    // stays only when no dropped token was beside it.
                    Piece::Separator(literal, beside_empty) => {
                        if !(*beside_empty && (index == 0 || index == last)) {
                            text.push_str(literal);
                        }
                    }
                }
            }
            rendered.extend(text.split('\n').map(str::to_owned));
        }
        rendered
    }
}

fn push_literal(line: Option<&mut Vec<TemplatePart<DetailsToken>>>, text: &str) {
    if let (Some(line), false) = (line, text.is_empty()) {
        line.push(TemplatePart::Literal(text.to_owned()));
    }
}

/// Spaces with punctuation only, like ` · ` or ` | `. Why not any
/// punctuation: `{name}/` and `{path}:{line}` attach their literal to a token.
fn is_separator(literal: &str) -> bool {
    literal.chars().any(char::is_whitespace) && !literal.chars().any(char::is_alphanumeric)
}

/// The parsed templates, each falling back to its default when it does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailsTemplates {
    pub file: DetailsTemplate,
    pub directory: DetailsTemplate,
    pub multi: DetailsTemplate,
    pub match_template: DetailsTemplate,
    pub max_lines: u16,
}

impl Default for DetailsTemplates {
    fn default() -> Self {
        Self::from_config(&FilesDetailsConfig::default())
    }
}

impl DetailsTemplates {
    pub fn from_config(config: &FilesDetailsConfig) -> Self {
        let parse = |template: &str, default: &str| {
            DetailsTemplate::parse(template)
                .or_else(|_| DetailsTemplate::parse(default))
                .unwrap_or_default()
        };
        Self {
            file: parse(&config.file, DEFAULT_FILE_TEMPLATE),
            directory: parse(&config.directory, DEFAULT_DIRECTORY_TEMPLATE),
            multi: parse(&config.multi, DEFAULT_MULTI_TEMPLATE),
            match_template: parse(&config.match_template, DEFAULT_MATCH_TEMPLATE),
            max_lines: config.max_lines.max(1),
        }
    }
}

pub(crate) fn files_details_diagnostics(config: &FilesDetailsConfig) -> Vec<String> {
    let mut diagnostics: Vec<String> = [
        ("file", &config.file),
        ("directory", &config.directory),
        ("multi", &config.multi),
        ("match", &config.match_template),
    ]
    .into_iter()
    .filter_map(|(key, template)| {
        DetailsTemplate::parse(template)
            .err()
            .map(|err| format!("ui.files.details.{key} {err}; using the default"))
    })
    .collect();
    if config.max_lines == 0 {
        diagnostics.push("ui.files.details.max_lines must be at least 1; using 1".to_owned());
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(template: &str, value: impl Fn(DetailsToken) -> &'static str) -> Vec<String> {
        DetailsTemplate::parse(template)
            .unwrap()
            .render(|token| value(token).to_owned())
    }

    #[test]
    fn newlines_split_the_template_into_lines_of_tokens_and_literals() {
        let lines = render("{name}/\n{children} items", |token| match token {
            DetailsToken::Name => "src",
            DetailsToken::Children => "3",
            _ => "",
        });

        assert_eq!(lines, ["src/", "3 items"]);
    }

    #[test]
    fn a_separator_next_to_an_empty_token_is_dropped_but_other_literals_stay() {
        let value = |token| match token {
            DetailsToken::Size => "5 B",
            DetailsToken::Owner => "me",
            DetailsToken::Path => "/a",
            _ => "",
        };

        assert_eq!(
            render("{size} · {mtime} · {mode} {owner}", value),
            ["5 B · me"]
        );
        assert_eq!(render("{mtime} · {size}", value), ["5 B"]);
        assert_eq!(render("{size} · {mtime}", value), ["5 B"]);
        assert_eq!(render("{path}:{line}", value), ["/a:"]);
    }

    #[test]
    fn prefixes_and_suffixes_stay_while_their_token_has_a_value() {
        let value = |token| match token {
            DetailsToken::LinkTarget => "../t",
            DetailsToken::Name => "a",
            _ => "",
        };

        assert_eq!(render("→ {link_target}", value), ["→ ../t"]);
        assert_eq!(render("  {name} *", value), ["  a *"]);
        assert_eq!(render("{size} · {name}", value), ["a"]);
        assert_eq!(
            render("{name} · {size} · {mode} | {link_target}", value),
            ["a · ../t"]
        );
    }

    #[test]
    fn a_line_whose_tokens_are_all_empty_is_dropped_and_a_literal_line_stays() {
        let lines = render("{name}\n{mode} {owner}\n--", |token| match token {
            DetailsToken::Name => "a",
            _ => "",
        });

        assert_eq!(lines, ["a", "--"]);
    }

    #[test]
    fn a_multiline_token_value_adds_lines() {
        let lines = render("{path}\n{context}", |token| match token {
            DetailsToken::Path => "/a",
            DetailsToken::Context => "1-x\n2:y",
            _ => "",
        });

        assert_eq!(lines, ["/a", "1-x", "2:y"]);
    }

    #[test]
    fn an_empty_template_has_no_lines_and_braces_escape() {
        assert_eq!(render("", |_| "x"), Vec::<String>::new());
        assert_eq!(render("{{{name}}}", |_| "n"), ["{n}"]);
    }

    #[test]
    fn a_template_that_does_not_parse_falls_back_to_its_default_with_a_diagnostic() {
        let config = FilesDetailsConfig {
            file: "{nope}".into(),
            match_template: "{path".into(),
            max_lines: 0,
            ..FilesDetailsConfig::default()
        };

        let templates = DetailsTemplates::from_config(&config);

        assert_eq!(
            templates.file,
            DetailsTemplate::parse(DEFAULT_FILE_TEMPLATE).unwrap()
        );
        assert_eq!(templates.max_lines, 1);
        assert_eq!(
            files_details_diagnostics(&config),
            [
                "ui.files.details.file has unknown token '{nope}'; using the default",
                "ui.files.details.match has an unclosed '{'; using the default",
                "ui.files.details.max_lines must be at least 1; using 1",
            ]
        );
    }

    #[test]
    fn the_match_key_deserializes_into_the_match_template() {
        let config: UiFilesConfig =
            toml::from_str("[details]\nmatch = \"{line}\"\nmax_lines = 3\n").unwrap();

        assert_eq!(config.details.match_template, "{line}");
        assert_eq!(config.details.max_lines, 3);
        assert_eq!(config.details.file, DEFAULT_FILE_TEMPLATE);
    }
}
