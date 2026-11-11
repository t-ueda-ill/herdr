//! Installs the bundled Herdr agent skill into each agent's skills directory.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::env::{
    claude_dir, codex_dir, config_dir_from_env_or_home, copilot_dir, cursor_dir, expand_tilde_path,
    home_dir, PI_CODING_AGENT_DIR_ENV_VAR,
};
use super::file_ops::remove_file_if_exists;

/// Bundled at build time so an installed skill always matches this binary's release.
pub(crate) const HERDR_SKILL: &str = include_str!("../../skills/herdr/SKILL.md");

const SKILL_DIR_NAME: &str = "herdr";
const SKILL_FILE_NAME: &str = "SKILL.md";
/// Holds the digest of the `SKILL.md` Herdr wrote. Content alone cannot show
/// who installed a copy: `npx skills add` writes the same file to the same
/// path, and a user may edit a copy Herdr installed.
const INSTALL_MARKER_NAME: &str = ".herdr-installed";

/// Skill locations, not agent integrations: several agents share
/// `~/.agents/skills`, so this list is deliberately separate from the frozen
/// `IntegrationTarget` endpoint enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillTarget {
    Claude,
    Agents,
    Opencode,
    Cursor,
    Copilot,
    Gemini,
}

impl SkillTarget {
    pub(crate) const ALL: [SkillTarget; 6] = [
        SkillTarget::Claude,
        SkillTarget::Agents,
        SkillTarget::Opencode,
        SkillTarget::Cursor,
        SkillTarget::Copilot,
        SkillTarget::Gemini,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            SkillTarget::Claude => "claude",
            SkillTarget::Agents => "agents",
            SkillTarget::Opencode => "opencode",
            SkillTarget::Cursor => "cursor",
            SkillTarget::Copilot => "copilot",
            SkillTarget::Gemini => "gemini",
        }
    }

    pub(crate) fn parse(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.label() == label)
    }

    /// The global skills directory each agent scans, matching the locations
    /// `npx skills add -g` writes so both installers converge on one path.
    pub(crate) fn skills_dir(self) -> io::Result<PathBuf> {
        Ok(match self {
            SkillTarget::Claude => claude_dir()?.join("skills"),
            SkillTarget::Agents => home_dir()?.join(".agents").join("skills"),
            SkillTarget::Opencode => xdg_config_home()?.join("opencode").join("skills"),
            SkillTarget::Cursor => cursor_dir()?.join("skills"),
            SkillTarget::Copilot => copilot_dir()?.join("skills"),
            SkillTarget::Gemini => home_dir()?.join(".gemini").join("skills"),
        })
    }

    /// True when one of the agent's config homes exists, so a bare
    /// `herdr skill install` only touches agents that are set up here.
    pub(crate) fn detected(self) -> io::Result<bool> {
        let homes = match self {
            SkillTarget::Claude => vec![claude_dir()?],
            // Codex and Pi read ~/.agents/skills instead of a private skills dir.
            SkillTarget::Agents => vec![
                home_dir()?.join(".agents"),
                codex_dir()?,
                config_dir_from_env_or_home(PI_CODING_AGENT_DIR_ENV_VAR, &[".pi", "agent"])?,
            ],
            SkillTarget::Opencode => vec![xdg_config_home()?.join("opencode")],
            SkillTarget::Cursor => vec![cursor_dir()?],
            SkillTarget::Copilot => vec![copilot_dir()?],
            SkillTarget::Gemini => vec![home_dir()?.join(".gemini")],
        };
        Ok(homes.iter().any(|home| home.is_dir()))
    }
}

fn xdg_config_home() -> io::Result<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        Some(value) => expand_tilde_path(PathBuf::from(value)),
        None => Ok(home_dir()?.join(".config")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillState {
    NotInstalled,
    Current,
    Outdated,
    /// A symlink, usually from `npx skills add`; its target belongs to that tool.
    Linked,
    /// A copy Herdr installed and someone edited afterwards.
    Modified,
    /// A copy Herdr did not install, whatever its content.
    Foreign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallOutcome {
    Installed,
    Updated,
    AlreadyCurrent,
    Refused(SkillState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UninstallOutcome {
    Removed,
    Unlinked,
    NotInstalled,
    Refused(SkillState),
}

/// `<skills_dir>/herdr`, the directory an agent loads as the `herdr` skill.
pub(crate) fn skill_dir_in(skills_dir: &Path) -> PathBuf {
    skills_dir.join(SKILL_DIR_NAME)
}

pub(crate) fn skill_file_in(skill_dir: &Path) -> PathBuf {
    skill_dir.join(SKILL_FILE_NAME)
}

pub(crate) fn skill_state_at(skill_dir: &Path, bundled: &str) -> io::Result<SkillState> {
    let skill_file = skill_dir.join(SKILL_FILE_NAME);
    if is_symlink(skill_dir)? || is_symlink(&skill_file)? {
        return Ok(SkillState::Linked);
    }
    let content = match fs::read_to_string(&skill_file) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(SkillState::NotInstalled),
        Err(err) => return Err(err),
    };
    let Some(installed_digest) = read_install_marker(skill_dir)? else {
        return Ok(SkillState::Foreign);
    };
    Ok(if content == bundled {
        SkillState::Current
    } else if crate::checksum::file_sha256(&skill_file)? == installed_digest {
        SkillState::Outdated
    } else {
        SkillState::Modified
    })
}

pub(crate) fn install_skill_at(
    skill_dir: &Path,
    bundled: &str,
    force: bool,
) -> io::Result<InstallOutcome> {
    let state = skill_state_at(skill_dir, bundled)?;
    match state {
        SkillState::Current => return Ok(InstallOutcome::AlreadyCurrent),
        SkillState::Linked | SkillState::Modified | SkillState::Foreign if !force => {
            return Ok(InstallOutcome::Refused(state))
        }
        SkillState::Linked => remove_skill_links(skill_dir)?,
        _ => {}
    }

    fs::create_dir_all(skill_dir)?;
    replace_file(skill_dir, SKILL_FILE_NAME, bundled.as_bytes())?;
    // Marker last, so an interrupted install never claims a copy Herdr did
    // not finish writing.
    let digest = crate::checksum::file_sha256(&skill_dir.join(SKILL_FILE_NAME))?;
    replace_file(skill_dir, INSTALL_MARKER_NAME, digest.as_bytes())?;

    Ok(match state {
        SkillState::NotInstalled | SkillState::Linked => InstallOutcome::Installed,
        _ => InstallOutcome::Updated,
    })
}

pub(crate) fn uninstall_skill_at(skill_dir: &Path, bundled: &str) -> io::Result<UninstallOutcome> {
    let state = skill_state_at(skill_dir, bundled)?;
    match state {
        SkillState::NotInstalled => {
            // A marker can outlive a SKILL.md the user deleted by hand.
            remove_file_if_exists(&skill_dir.join(INSTALL_MARKER_NAME))?;
            remove_dir_if_empty(skill_dir)?;
            Ok(UninstallOutcome::NotInstalled)
        }
        SkillState::Modified | SkillState::Foreign => Ok(UninstallOutcome::Refused(state)),
        SkillState::Linked => {
            remove_skill_links(skill_dir)?;
            remove_dir_if_empty(skill_dir)?;
            Ok(UninstallOutcome::Unlinked)
        }
        SkillState::Current | SkillState::Outdated => {
            fs::remove_file(skill_dir.join(SKILL_FILE_NAME))?;
            remove_file_if_exists(&skill_dir.join(INSTALL_MARKER_NAME))?;
            remove_dir_if_empty(skill_dir)?;
            Ok(UninstallOutcome::Removed)
        }
    }
}

fn read_install_marker(skill_dir: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(skill_dir.join(INSTALL_MARKER_NAME)) {
        Ok(digest) => Ok(Some(digest.trim().to_string())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Writes `dir/file_name` through a staging file and a rename, so an agent
/// loading skills mid-install never sees a truncated file.
fn replace_file(dir: &Path, file_name: &str, bytes: &[u8]) -> io::Result<()> {
    static STAGED: AtomicU64 = AtomicU64::new(0);
    // A fixed staging name would let two concurrent installs truncate each
    // other's file, so every call gets its own.
    let staged = dir.join(format!(
        ".{file_name}.herdr-tmp-{}-{}",
        std::process::id(),
        STAGED.fetch_add(1, Ordering::Relaxed)
    ));
    write_new_file(&staged, bytes)?;
    fs::rename(&staged, dir.join(file_name)).inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })
}

/// Fails when `path` already exists instead of writing to it: `fs::write`
/// would follow a symlink planted there and truncate its target.
fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    // Past this point the file is ours, so a failed write may remove it.
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .inspect_err(|_| {
            let _ = fs::remove_file(path);
        })
}

fn is_symlink(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// Removes the link itself, never its target, which belongs to whichever tool
/// created it.
fn remove_skill_links(skill_dir: &Path) -> io::Result<()> {
    // Check the directory first: through a directory link, `SKILL.md` names a
    // file inside the link's target, which may itself be a link we must keep.
    if is_symlink(skill_dir)? {
        return remove_link(skill_dir);
    }
    let skill_file = skill_dir.join(SKILL_FILE_NAME);
    if is_symlink(&skill_file)? {
        remove_link(&skill_file)?;
    }
    Ok(())
}

fn remove_link(path: &Path) -> io::Result<()> {
    // Windows directory symlinks reject remove_file, so fall back to
    // remove_dir instead of branching on the platform here.
    fs::remove_file(path).or_else(|_| fs::remove_dir(path))
}

fn remove_dir_if_empty(dir: &Path) -> io::Result<()> {
    let mut entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if entries.next().is_none() {
        fs::remove_dir(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const BUNDLED: &str = "---\nname: herdr\ndescription: \"Control Herdr\"\n---\n\n# Herdr\n";
    const OLDER: &str = "---\nname: herdr\ndescription: older\n---\n\n# Herdr v0\n";
    const FOREIGN: &str = "---\nname: my-herdr-notes\n---\n\nmine\n";
    const EDITED: &str = "---\nname: herdr\ndescription: older\n---\n\n# Herdr v0\n\nmy notes\n";

    struct TempSkills(PathBuf);

    impl TempSkills {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "herdr-skill-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            fs::create_dir_all(&root).expect("create temp skills dir");
            Self(root)
        }

        fn skill_dir(&self) -> PathBuf {
            skill_dir_in(&self.0)
        }

        fn skill_file(&self) -> PathBuf {
            self.skill_dir().join(SKILL_FILE_NAME)
        }

        fn write_skill(&self, content: &str) {
            fs::create_dir_all(self.skill_dir()).expect("create skill dir");
            fs::write(self.skill_file(), content).expect("write skill");
        }
    }

    impl Drop for TempSkills {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn claude_skills_dir_follows_claude_config_dir() {
        let _lock = crate::integration::integration_env_lock();
        let temp = TempSkills::new();
        let original = std::env::var_os(super::super::env::CLAUDE_CONFIG_DIR_ENV_VAR);
        std::env::set_var(super::super::env::CLAUDE_CONFIG_DIR_ENV_VAR, &temp.0);

        let skills_dir = SkillTarget::Claude.skills_dir().unwrap();
        let detected = SkillTarget::Claude.detected().unwrap();

        match original {
            Some(value) => std::env::set_var(super::super::env::CLAUDE_CONFIG_DIR_ENV_VAR, value),
            None => std::env::remove_var(super::super::env::CLAUDE_CONFIG_DIR_ENV_VAR),
        }
        assert_eq!(skills_dir, temp.0.join("skills"));
        assert!(detected);
    }

    #[test]
    fn agents_are_detected_only_when_a_reader_home_exists() {
        let _lock = crate::integration::integration_env_lock();
        let temp = TempSkills::new();
        let saved: Vec<_> = [
            "HOME",
            super::super::env::CODEX_HOME_ENV_VAR,
            PI_CODING_AGENT_DIR_ENV_VAR,
        ]
        .into_iter()
        .map(|name| (name, std::env::var_os(name)))
        .collect();
        std::env::set_var("HOME", &temp.0);
        std::env::remove_var(super::super::env::CODEX_HOME_ENV_VAR);
        std::env::remove_var(PI_CODING_AGENT_DIR_ENV_VAR);

        let before = SkillTarget::Agents.detected().unwrap();
        fs::create_dir_all(temp.0.join(".codex")).unwrap();
        let after = SkillTarget::Agents.detected().unwrap();
        let skills_dir = SkillTarget::Agents.skills_dir().unwrap();

        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        assert!(!before);
        assert!(after);
        assert_eq!(skills_dir, temp.0.join(".agents").join("skills"));
    }

    #[test]
    fn labels_round_trip_through_parse() {
        for target in SkillTarget::ALL {
            assert_eq!(SkillTarget::parse(target.label()), Some(target));
        }
        assert_eq!(SkillTarget::parse("codex"), None);
    }

    #[test]
    fn fresh_install_writes_the_bundled_skill() {
        let temp = TempSkills::new();

        let outcome = install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();

        assert_eq!(outcome, InstallOutcome::Installed);
        assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), BUNDLED);
        assert_eq!(
            skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
            SkillState::Current
        );
    }

    #[test]
    fn reinstalling_a_current_skill_changes_nothing() {
        let temp = TempSkills::new();
        install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();

        let outcome = install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();

        assert_eq!(outcome, InstallOutcome::AlreadyCurrent);
        assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), BUNDLED);
    }

    #[test]
    fn install_updates_an_older_copy_herdr_installed() {
        let temp = TempSkills::new();
        install_skill_at(&temp.skill_dir(), OLDER, false).unwrap();
        assert_eq!(
            skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
            SkillState::Outdated
        );

        let outcome = install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();

        assert_eq!(outcome, InstallOutcome::Updated);
        assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), BUNDLED);
    }

    #[test]
    fn install_keeps_a_foreign_skill_unless_forced() {
        let temp = TempSkills::new();
        temp.write_skill(FOREIGN);
        assert_eq!(
            skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
            SkillState::Foreign
        );

        let refused = install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();
        assert_eq!(refused, InstallOutcome::Refused(SkillState::Foreign));
        assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), FOREIGN);

        let forced = install_skill_at(&temp.skill_dir(), BUNDLED, true).unwrap();
        assert_eq!(forced, InstallOutcome::Updated);
        assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), BUNDLED);
    }

    #[test]
    fn a_copy_herdr_did_not_install_is_kept_even_when_identical() {
        let temp = TempSkills::new();
        temp.write_skill(BUNDLED);

        assert_eq!(
            skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
            SkillState::Foreign
        );
        assert_eq!(
            install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap(),
            InstallOutcome::Refused(SkillState::Foreign)
        );
        assert_eq!(
            uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap(),
            UninstallOutcome::Refused(SkillState::Foreign)
        );
        assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), BUNDLED);
    }

    #[test]
    fn forced_install_adopts_a_copy_herdr_did_not_install() {
        let temp = TempSkills::new();
        temp.write_skill(OLDER);

        let forced = install_skill_at(&temp.skill_dir(), BUNDLED, true).unwrap();

        assert_eq!(forced, InstallOutcome::Updated);
        assert_eq!(
            skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
            SkillState::Current
        );
        assert_eq!(
            uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap(),
            UninstallOutcome::Removed
        );
    }

    #[test]
    fn a_copy_edited_after_install_is_kept_unless_forced() {
        let temp = TempSkills::new();
        install_skill_at(&temp.skill_dir(), OLDER, false).unwrap();
        fs::write(temp.skill_file(), EDITED).unwrap();

        assert_eq!(
            skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
            SkillState::Modified
        );
        assert_eq!(
            install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap(),
            InstallOutcome::Refused(SkillState::Modified)
        );
        assert_eq!(
            uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap(),
            UninstallOutcome::Refused(SkillState::Modified)
        );
        assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), EDITED);

        let forced = install_skill_at(&temp.skill_dir(), BUNDLED, true).unwrap();
        assert_eq!(forced, InstallOutcome::Updated);
        assert_eq!(
            skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
            SkillState::Current
        );
    }

    #[test]
    fn a_leftover_marker_without_a_skill_file_counts_as_not_installed() {
        let temp = TempSkills::new();
        install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();
        fs::remove_file(temp.skill_file()).unwrap();

        assert_eq!(
            skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
            SkillState::NotInstalled
        );
        assert_eq!(
            uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap(),
            UninstallOutcome::NotInstalled
        );
        assert!(!temp.skill_dir().exists());
    }

    #[test]
    fn install_leaves_only_the_skill_and_its_marker() {
        let temp = TempSkills::new();
        install_skill_at(&temp.skill_dir(), OLDER, false).unwrap();
        install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();

        let mut names: Vec<_> = fs::read_dir(temp.skill_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();

        assert_eq!(names, [INSTALL_MARKER_NAME, SKILL_FILE_NAME]);
    }

    #[test]
    fn install_keeps_other_files_in_the_skill_dir() {
        let temp = TempSkills::new();
        fs::create_dir_all(temp.skill_dir()).unwrap();
        fs::write(temp.skill_dir().join("notes.md"), "keep").unwrap();

        install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();

        assert_eq!(
            fs::read_to_string(temp.skill_dir().join("notes.md")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn missing_skill_reports_not_installed() {
        let temp = TempSkills::new();

        assert_eq!(
            skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
            SkillState::NotInstalled
        );
        assert_eq!(
            uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap(),
            UninstallOutcome::NotInstalled
        );
    }

    #[test]
    fn uninstall_removes_the_herdr_skill_and_its_empty_dir() {
        let temp = TempSkills::new();
        install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();

        let outcome = uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap();

        assert_eq!(outcome, UninstallOutcome::Removed);
        assert!(!temp.skill_dir().exists());
    }

    #[test]
    fn uninstall_keeps_the_dir_when_other_files_remain() {
        let temp = TempSkills::new();
        install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();
        fs::write(temp.skill_dir().join("notes.md"), "keep").unwrap();

        uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap();

        assert!(!temp.skill_file().exists());
        assert!(!temp.skill_dir().join(INSTALL_MARKER_NAME).exists());
        assert!(temp.skill_dir().join("notes.md").exists());
    }

    #[test]
    fn uninstall_refuses_a_foreign_skill() {
        let temp = TempSkills::new();
        temp.write_skill(FOREIGN);

        let outcome = uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap();

        assert_eq!(outcome, UninstallOutcome::Refused(SkillState::Foreign));
        assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), FOREIGN);
    }

    #[cfg(unix)]
    mod symlinked {
        use super::*;

        fn link_skill_dir(temp: &TempSkills) -> PathBuf {
            let shared = temp.0.join("shared-herdr");
            fs::create_dir_all(&shared).unwrap();
            fs::write(shared.join(SKILL_FILE_NAME), OLDER).unwrap();
            std::os::unix::fs::symlink(&shared, temp.skill_dir()).unwrap();
            shared
        }

        #[test]
        fn a_linked_skill_dir_is_reported_and_left_alone() {
            let temp = TempSkills::new();
            let shared = link_skill_dir(&temp);

            assert_eq!(
                skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
                SkillState::Linked
            );
            let outcome = install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();

            assert_eq!(outcome, InstallOutcome::Refused(SkillState::Linked));
            assert_eq!(
                fs::read_to_string(shared.join(SKILL_FILE_NAME)).unwrap(),
                OLDER
            );
        }

        #[test]
        fn forced_install_replaces_the_link_without_writing_through_it() {
            let temp = TempSkills::new();
            let shared = link_skill_dir(&temp);

            let outcome = install_skill_at(&temp.skill_dir(), BUNDLED, true).unwrap();

            assert_eq!(outcome, InstallOutcome::Installed);
            assert!(!fs::symlink_metadata(temp.skill_dir())
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), BUNDLED);
            assert_eq!(
                fs::read_to_string(shared.join(SKILL_FILE_NAME)).unwrap(),
                OLDER
            );
        }

        #[test]
        fn uninstall_removes_only_the_link() {
            let temp = TempSkills::new();
            let shared = link_skill_dir(&temp);

            let outcome = uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap();

            assert_eq!(outcome, UninstallOutcome::Unlinked);
            assert!(fs::symlink_metadata(temp.skill_dir()).is_err());
            assert!(shared.join(SKILL_FILE_NAME).exists());
        }

        #[test]
        fn a_linked_dir_never_touches_links_inside_its_target() {
            let temp = TempSkills::new();
            let checkout = temp.0.join("checkout-skill.md");
            fs::write(&checkout, BUNDLED).unwrap();
            let shared = temp.0.join("shared-herdr");
            fs::create_dir_all(&shared).unwrap();
            std::os::unix::fs::symlink(&checkout, shared.join(SKILL_FILE_NAME)).unwrap();
            std::os::unix::fs::symlink(&shared, temp.skill_dir()).unwrap();

            install_skill_at(&temp.skill_dir(), BUNDLED, true).unwrap();
            uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap();

            assert!(fs::symlink_metadata(shared.join(SKILL_FILE_NAME))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read_to_string(&checkout).unwrap(), BUNDLED);
        }

        #[test]
        fn a_link_planted_at_the_old_staging_path_is_never_written_through() {
            let temp = TempSkills::new();
            let victim = temp.0.join("victim.txt");
            fs::write(&victim, "keep").unwrap();
            fs::create_dir_all(temp.skill_dir()).unwrap();
            std::os::unix::fs::symlink(&victim, temp.skill_dir().join(".SKILL.md.herdr-tmp"))
                .unwrap();

            let outcome = install_skill_at(&temp.skill_dir(), BUNDLED, false).unwrap();

            assert_eq!(outcome, InstallOutcome::Installed);
            assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
            assert!(!fs::symlink_metadata(temp.skill_file())
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read_to_string(temp.skill_file()).unwrap(), BUNDLED);
        }

        #[test]
        fn a_staging_path_that_already_exists_fails_instead_of_being_reused() {
            let temp = TempSkills::new();
            let victim = temp.0.join("victim.txt");
            fs::write(&victim, "keep").unwrap();
            let staged = temp.0.join("staged");
            std::os::unix::fs::symlink(&victim, &staged).unwrap();

            let result = write_new_file(&staged, b"overwritten");

            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
            assert!(fs::symlink_metadata(&staged).is_ok());
        }

        #[test]
        fn a_linked_skill_file_counts_as_linked() {
            let temp = TempSkills::new();
            let shared = temp.0.join("shared-skill.md");
            fs::write(&shared, BUNDLED).unwrap();
            fs::create_dir_all(temp.skill_dir()).unwrap();
            std::os::unix::fs::symlink(&shared, temp.skill_file()).unwrap();

            assert_eq!(
                skill_state_at(&temp.skill_dir(), BUNDLED).unwrap(),
                SkillState::Linked
            );
            assert_eq!(
                uninstall_skill_at(&temp.skill_dir(), BUNDLED).unwrap(),
                UninstallOutcome::Unlinked
            );
            assert!(shared.exists());
        }
    }
}
