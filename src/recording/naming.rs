//! Recording file names: which space a pane belongs to, and the `[log]
//! file_name` template that turns it into a file stem.

use std::path::Path;

/// `{space}` first so one space's recordings sit together in a listing, and
/// `{start}` so a restarted server never reuses the same name for pane 1.
pub(crate) const DEFAULT_FILE_NAME: &str = "{space}-{start}-pane-{pane}";

/// Why in bytes: file systems limit a name to 255 bytes, not characters, and
/// one CJK or emoji character takes 3 or 4. With each placeholder used at
/// most once, the longest name is 80 (template text) + 2 x 49 (space and
/// session, a `~` may add a byte) + 15 (start) + 10 (pane) + 13
/// (`.1000.cast.gz`) = 216 bytes.
const MAX_COMPONENT_BYTES: usize = 48;
const MAX_LITERAL_BYTES: usize = 80;

/// A workspace's label, and whether someone chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceLabel {
    /// Given by a rename or at creation, such as the Docker name the
    /// container picker passes.
    Custom(String),
    /// Derived from the workspace's folder or repository.
    Auto(String),
}

/// What a pane runs in, as far as naming its recording goes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SpaceSource<'a> {
    /// The container the shell runs in.
    pub container: Option<&'a str>,
    /// The ssh host alias the shell runs on.
    pub ssh: Option<&'a str>,
    /// The label of the workspace the pane opens in.
    pub label: Option<&'a WorkspaceLabel>,
}

impl SpaceSource<'_> {
    /// The space name: a chosen workspace label, else the container, else
    /// the ssh host, else the derived workspace label, else the folder name
    /// of `cwd`.
    ///
    /// Why a chosen label first: the container picker names the workspace by
    /// the Docker name (`shop-web-1`), while the container reference of a
    /// Compose service is `project/service`. Why off-host before a derived
    /// label: an ssh or container workspace derives its label from the host
    /// folder it was opened from, which is not where the shell runs.
    pub(crate) fn name(&self, cwd: &Path) -> String {
        let (custom, auto) = match self.label {
            Some(WorkspaceLabel::Custom(label)) => (Some(label.as_str()), None),
            Some(WorkspaceLabel::Auto(label)) => (None, Some(label.as_str())),
            None => (None, None),
        };
        [custom, self.container, self.ssh, auto]
            .into_iter()
            .flatten()
            .find(|name| !name.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| crate::workspace::fallback_label_from_cwd(cwd))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Space,
    Pane,
    Session,
    Start,
}

/// The values a template can name.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FileNameVars<'a> {
    pub space: &'a str,
    pub pane: u32,
    pub session: &'a str,
    /// Local start time, already in file name form.
    pub start: &'a str,
}

/// A parsed `[log] file_name`: text with `{space}`, `{pane}`, `{session}` and
/// `{start}` placeholders, without the extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileNameTemplate {
    parts: Vec<Part>,
}

impl Default for FileNameTemplate {
    fn default() -> Self {
        Self::parse(DEFAULT_FILE_NAME).unwrap_or_else(|_| Self {
            parts: vec![Part::Pane],
        })
    }
}

impl FileNameTemplate {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut rest = text;
        let unmatched = || format!("unmatched `}}` in file_name {text:?}");
        while let Some(open) = rest.find('{') {
            if rest[..open].contains('}') {
                return Err(unmatched());
            }
            literal.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let close = after
                .find('}')
                .ok_or_else(|| format!("unclosed `{{` in file_name {text:?}"))?;
            let part = match &after[..close] {
                "space" => Part::Space,
                "pane" => Part::Pane,
                "session" => Part::Session,
                "start" => Part::Start,
                other => {
                    return Err(format!(
                        "unknown placeholder {{{other}}} in file_name {text:?}; use {{space}}, {{pane}}, {{session}} or {{start}}"
                    ))
                }
            };
            if !literal.is_empty() {
                parts.push(Part::Literal(std::mem::take(&mut literal)));
            }
            parts.push(part);
            rest = &after[close + 1..];
        }
        if rest.contains('}') {
            return Err(unmatched());
        }
        literal.push_str(rest);
        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }
        let mut literal_bytes = 0;
        for (index, part) in parts.iter().enumerate() {
            if let Part::Literal(text) = part {
                if let Some(bad) = text.chars().find(|&c| !is_name_char(c)) {
                    return Err(format!(
                        "file_name may not contain {bad:?}; it names a file inside the session folder"
                    ));
                }
                literal_bytes += text.len();
            } else if parts[..index].contains(part) {
                return Err(format!("file_name {text:?} may use each placeholder once"));
            }
        }
        if literal_bytes > MAX_LITERAL_BYTES {
            return Err(format!(
                "file_name {text:?} has more than {MAX_LITERAL_BYTES} bytes of text besides placeholders"
            ));
        }
        // Why required: without it every pane of a space writes the same name,
        // and the never-overwrite rule turns them into indistinguishable `.N` parts.
        if !parts.contains(&Part::Pane) {
            return Err(format!("file_name {text:?} must contain {{pane}}"));
        }
        Ok(Self { parts })
    }

    pub(crate) fn render(&self, vars: &FileNameVars<'_>) -> String {
        let mut name = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(text) => name.push_str(text),
                Part::Space => name.push_str(&sanitize_component(vars.space)),
                Part::Pane => name.push_str(&vars.pane.to_string()),
                Part::Session => name.push_str(&sanitize_component(vars.session)),
                Part::Start => name.push_str(&sanitize_component(vars.start)),
            }
        }
        name
    }
}

/// Characters a name may keep on every platform herdr runs on.
fn is_name_char(c: char) -> bool {
    !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
}

/// Makes `text` safe as part of one file name: no path separators or
/// characters Windows rejects, no leading dots that would hide the file or
/// climb out with `..`, no leading `~` that a shell would expand, and never
/// empty.
pub(crate) fn sanitize_component(text: &str) -> String {
    // The home folder's workspace label.
    if text == "~" {
        return "home".to_owned();
    }
    let mut replaced = String::new();
    for c in text.chars().map(|c| if is_name_char(c) { c } else { '_' }) {
        if replaced.len() + c.len_utf8() > MAX_COMPONENT_BYTES {
            break;
        }
        replaced.push(c);
    }
    let trimmed = replaced.trim_matches(|c: char| c == '.' || c.is_whitespace());
    match trimmed.strip_prefix('~') {
        _ if trimmed.is_empty() => "_".to_owned(),
        Some(rest) => format!("_{rest}"),
        None => trimmed.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn vars<'a>(space: &'a str) -> FileNameVars<'a> {
        FileNameVars {
            space,
            pane: 3,
            session: "default",
            start: "20261125-103012",
        }
    }

    fn auto(label: &str) -> WorkspaceLabel {
        WorkspaceLabel::Auto(label.into())
    }

    #[test]
    fn a_chosen_label_names_the_space_before_the_container() {
        let label = WorkspaceLabel::Custom("shop-web-1".into());
        let source = SpaceSource {
            container: Some("shop/web"),
            label: Some(&label),
            ..SpaceSource::default()
        };
        assert_eq!(source.name(Path::new("/home/u")), "shop-web-1");
    }

    #[test]
    fn a_container_names_the_space_before_ssh_and_a_derived_label() {
        let label = auto("repo");
        let source = SpaceSource {
            container: Some("shop-web-1"),
            ssh: Some("prod"),
            label: Some(&label),
        };
        assert_eq!(source.name(Path::new("/home/u/repo")), "shop-web-1");
    }

    #[test]
    fn an_ssh_host_names_the_space_before_a_derived_label() {
        let label = auto("repo");
        let source = SpaceSource {
            ssh: Some("prod"),
            label: Some(&label),
            ..SpaceSource::default()
        };
        assert_eq!(source.name(Path::new("/home/u/repo")), "prod");
    }

    #[test]
    fn a_host_pane_uses_the_workspace_label() {
        let label = auto("my-repo");
        let source = SpaceSource {
            label: Some(&label),
            ..SpaceSource::default()
        };
        assert_eq!(source.name(Path::new("/home/u/my-repo/src")), "my-repo");
    }

    #[test]
    fn without_a_label_the_folder_name_of_the_cwd_is_used() {
        let cwd = PathBuf::from("/srv/project");
        assert_eq!(SpaceSource::default().name(&cwd), "project");
    }

    #[test]
    fn the_default_template_puts_space_start_and_pane_in_the_name() {
        let template = FileNameTemplate::default();
        assert_eq!(
            template.render(&vars("repo")),
            "repo-20261125-103012-pane-3"
        );
    }

    #[test]
    fn a_template_can_reorder_placeholders_and_add_text() {
        let template = FileNameTemplate::parse("log_{session}_{pane}_{space}").unwrap();
        assert_eq!(template.render(&vars("repo")), "log_default_3_repo");
    }

    #[test]
    fn a_space_name_cannot_add_folders_or_climb_out_of_the_session_folder() {
        let template = FileNameTemplate::parse("{space}-pane-{pane}").unwrap();
        assert_eq!(template.render(&vars("shop/web")), "shop_web-pane-3");
        assert_eq!(template.render(&vars("../etc")), "_etc-pane-3");
        assert_eq!(template.render(&vars("..")), "_-pane-3");
        assert_eq!(template.render(&vars("a:b*c")), "a_b_c-pane-3");
        assert_eq!(template.render(&vars("")), "_-pane-3");
    }

    #[test]
    fn the_home_folder_label_becomes_home_and_no_name_starts_with_a_tilde() {
        let template = FileNameTemplate::parse("{space}-{pane}").unwrap();
        assert_eq!(template.render(&vars("~")), "home-3");
        assert_eq!(template.render(&vars("~user")), "_user-3");
    }

    #[test]
    fn a_long_space_name_is_shortened() {
        let template = FileNameTemplate::parse("{space}-{pane}").unwrap();
        let long = "x".repeat(300);
        let name = template.render(&vars(&long));
        assert_eq!(name, format!("{}-3", "x".repeat(MAX_COMPONENT_BYTES)));
    }

    #[test]
    fn a_space_name_of_wide_characters_is_cut_by_bytes_on_a_character_boundary() {
        let template = FileNameTemplate::default();
        let wide = "🦀".repeat(64);
        let name = template.render(&vars(&wide));
        let space = name.split('-').next().unwrap();
        assert_eq!(space, "🦀".repeat(MAX_COMPONENT_BYTES / 4));
    }

    #[test]
    fn the_longest_allowed_name_fits_in_255_bytes_with_a_numbered_extension() {
        let literal = "a".repeat(MAX_LITERAL_BYTES - 3);
        let template = FileNameTemplate::parse(&format!(
            "{literal}{{space}}-{{session}}-{{start}}-{{pane}}"
        ))
        .unwrap();
        let wide = "あ".repeat(100);
        let name = template.render(&FileNameVars {
            space: &wide,
            pane: u32::MAX,
            session: &wide,
            start: "20261125-103012",
        });
        assert!(name.len() + ".1000.cast.gz".len() <= 255, "{}", name.len());
    }

    #[test]
    fn a_placeholder_used_twice_or_too_much_template_text_is_rejected() {
        assert!(FileNameTemplate::parse("{space}{space}{pane}").is_err());
        let long = "a".repeat(MAX_LITERAL_BYTES + 1);
        assert!(FileNameTemplate::parse(&format!("{long}{{pane}}")).is_err());
    }

    #[test]
    fn a_template_without_the_pane_number_is_rejected() {
        let err = FileNameTemplate::parse("{space}").unwrap_err();
        assert!(err.contains("{pane}"), "{err}");
    }

    #[test]
    fn an_unknown_placeholder_is_rejected() {
        let err = FileNameTemplate::parse("{host}-{pane}").unwrap_err();
        assert!(err.contains("{host}"), "{err}");
    }

    #[test]
    fn path_separators_in_the_template_are_rejected() {
        assert!(FileNameTemplate::parse("logs/{pane}").is_err());
        assert!(FileNameTemplate::parse("logs\\{pane}").is_err());
    }

    #[test]
    fn unbalanced_braces_are_rejected() {
        assert!(FileNameTemplate::parse("{pane").is_err());
        assert!(FileNameTemplate::parse("pane}-{pane}").is_err());
    }
}
