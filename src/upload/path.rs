//! Validation of names and relative paths before they reach a sink. The remote
//! receiver reads one header per line, so anything with a control character
//! (newline included) is rejected here rather than escaped.

use std::collections::HashSet;

use super::STAGE_PREFIX;

pub(crate) const MAX_NAMES: usize = 1_024;
/// Keeps the begin envelope under the 1 MiB request limit even when JSON doubles
/// every `"` and `\`.
pub(crate) const MAX_NAMES_TOTAL_BYTES: usize = 256 * 1024;
pub(crate) const MAX_NAME_BYTES: usize = 255;
pub(crate) const MAX_PATH_BYTES: usize = 4_096;
pub(crate) const MAX_LINK_TARGET_BYTES: usize = 4_096;

pub(crate) type Failure = (&'static str, String);

fn invalid(message: String) -> Failure {
    ("invalid_params", message)
}

fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

/// One path component: what a filesystem accepts as a single name.
fn validate_component(component: &str, what: &str) -> Result<(), Failure> {
    if component.is_empty() {
        return Err(invalid(format!("{what} has an empty component")));
    }
    if component == "." || component == ".." {
        return Err(invalid(format!("{what} must not contain `.` or `..`")));
    }
    if component.len() > MAX_NAME_BYTES {
        return Err(invalid(format!(
            "{what} has a component longer than {MAX_NAME_BYTES} bytes"
        )));
    }
    // `\` is a valid Unix name character; a Windows host sink refuses it
    // itself, where it would be read as a separator.
    if component.contains('/') || has_control(component) {
        return Err(invalid(format!(
            "{what} contains a separator or control character: {component:?}"
        )));
    }
    Ok(())
}

/// The top-level names of a begin request.
pub(crate) fn validate_names(names: &[String]) -> Result<(), Failure> {
    if names.is_empty() || names.len() > MAX_NAMES {
        return Err(invalid(format!(
            "names must hold 1 to {MAX_NAMES} entries, not {}",
            names.len()
        )));
    }
    let total: usize = names.iter().map(String::len).sum();
    if total > MAX_NAMES_TOTAL_BYTES {
        return Err(invalid(format!(
            "names must total at most {MAX_NAMES_TOTAL_BYTES} bytes"
        )));
    }
    let mut seen = HashSet::new();
    for name in names {
        validate_component(name, "name")?;
        if name.starts_with(STAGE_PREFIX) {
            return Err(invalid(format!(
                "names must not start with {STAGE_PREFIX}: {name:?}"
            )));
        }
        if !seen.insert(name.as_str()) {
            return Err(invalid(format!("duplicate name: {name:?}")));
        }
    }
    Ok(())
}

/// A validated `/`-separated relative path inside an upload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RelPath(String);

impl RelPath {
    pub(crate) fn parse(path: &str, names: &HashSet<String>) -> Result<Self, Failure> {
        if path.len() > MAX_PATH_BYTES {
            return Err(invalid(format!(
                "path is longer than {MAX_PATH_BYTES} bytes"
            )));
        }
        if path.starts_with('/') {
            return Err(invalid(format!("path must be relative: {path:?}")));
        }
        for component in path.split('/') {
            validate_component(component, "path")?;
        }
        let first = path.split('/').next().unwrap_or_default();
        if !names.contains(first) {
            return Err(invalid(format!(
                "path {path:?} is not under one of the begin names"
            )));
        }
        Ok(Self(path.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// The enclosing path, or `None` for a top-level name.
    pub(crate) fn parent(&self) -> Option<&str> {
        self.0.rsplit_once('/').map(|(parent, _)| parent)
    }

    /// [`Self::parent`] as a path, for lookups keyed by `RelPath`.
    pub(crate) fn parent_path(&self) -> Option<Self> {
        self.parent().map(|parent| Self(parent.to_owned()))
    }

    /// A begin name as a path. Callers pass names that already passed
    /// [`validate_names`].
    pub(crate) fn top_level(name: &str) -> Self {
        Self(name.to_owned())
    }

    pub(crate) fn depth(&self) -> usize {
        self.0.matches('/').count()
    }

    pub(crate) fn is_top(&self) -> bool {
        !self.0.contains('/')
    }

    #[cfg(test)]
    pub(crate) fn test_new(path: &str) -> Self {
        Self(path.to_owned())
    }
}

/// A symlink's target. Sent as is and never followed (spec Q5).
pub(crate) fn validate_link_target(target: &str) -> Result<(), Failure> {
    if target.is_empty() || target.len() > MAX_LINK_TARGET_BYTES {
        return Err(invalid(format!(
            "link_target must be 1 to {MAX_LINK_TARGET_BYTES} bytes"
        )));
    }
    if has_control(target) {
        return Err(invalid(
            "link_target must not contain control characters".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> HashSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn names_with_spaces_quotes_and_leading_dots_or_dashes_are_accepted() {
        assert_eq!(
            validate_names(&owned(&[
                "a b.txt", "it's", "$x", "-flag", ".bashrc", "日本"
            ])),
            Ok(())
        );
    }

    #[test]
    fn names_are_rejected_when_empty_dotted_separated_or_controlled() {
        for bad in ["", ".", "..", "a/b", "a\nb", "a\0b", "a\tb"] {
            let result = validate_names(&owned(&[bad]));
            assert_eq!(
                result.map_err(|(code, _)| code),
                Err("invalid_params"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn names_are_rejected_when_they_look_like_a_stage_directory() {
        assert!(validate_names(&owned(&[".herdr-upload-0123"])).is_err());
    }

    #[test]
    fn duplicate_names_are_rejected() {
        assert!(validate_names(&owned(&["a", "a"])).is_err());
    }

    #[test]
    fn name_count_and_size_limits_are_enforced() {
        assert!(validate_names(&[]).is_err());
        let many: Vec<String> = (0..=MAX_NAMES).map(|i| format!("f{i}")).collect();
        assert!(validate_names(&many).is_err());
        assert!(validate_names(&many[..MAX_NAMES]).is_ok());
        assert!(validate_names(&["x".repeat(MAX_NAME_BYTES + 1)]).is_err());
        let widest: Vec<String> = (0..MAX_NAMES)
            .map(|i| format!("{i:04}{}", "y".repeat(MAX_NAME_BYTES - 4)))
            .collect();
        assert!(
            validate_names(&widest).is_ok(),
            "the per-name cap keeps the total in range"
        );
    }

    #[test]
    fn relative_paths_must_start_at_a_begin_name_and_stay_inside_it() {
        let names = names(&["dir"]);

        assert_eq!(
            RelPath::parse("dir/a b/c", &names).map(|p| p.0),
            Ok("dir/a b/c".into())
        );
        for bad in [
            "/dir/a", "dir/../x", "dir/./x", "dir//x", "dir/", "other/x", "dir/a\nb", "dir/a\0",
        ] {
            assert!(RelPath::parse(bad, &names).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn relative_paths_report_parent_top_and_depth() {
        let path = RelPath::test_new("a/b/c");
        assert_eq!(path.parent(), Some("a/b"));
        assert_eq!(path.depth(), 2);
        assert!(!path.is_top());
        assert!(RelPath::test_new("a").is_top());
        assert_eq!(RelPath::test_new("a").parent(), None);
    }

    #[test]
    fn link_targets_keep_dotdot_but_reject_control_characters_and_overlong_values() {
        assert_eq!(validate_link_target("../../etc"), Ok(()));
        assert!(validate_link_target("").is_err());
        assert!(validate_link_target("a\nb").is_err());
        assert!(validate_link_target(&"x".repeat(MAX_LINK_TARGET_BYTES + 1)).is_err());
    }
}
