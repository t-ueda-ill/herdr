use crate::integration::skill::{
    install_skill_at, skill_dir_in, skill_file_in, skill_state_at, uninstall_skill_at,
    InstallOutcome, SkillState, SkillTarget, UninstallOutcome, HERDR_SKILL,
};

const USAGE: &str =
    "usage: herdr skill <install [<target>...] [--force]|uninstall [<target>...]|status|print>\n\
targets: claude, agents, opencode, cursor, copilot, gemini";

#[derive(Debug, PartialEq, Eq)]
enum SkillCommand {
    Install {
        targets: Vec<SkillTarget>,
        force: bool,
    },
    Uninstall {
        targets: Vec<SkillTarget>,
    },
    Status,
    Print,
}

pub(super) fn run_skill_command(args: &[String]) -> std::io::Result<i32> {
    let command = match parse_skill_args(args) {
        Ok(command) => command,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };
    match command {
        SkillCommand::Install { targets, force } => install(targets, force),
        SkillCommand::Uninstall { targets } => uninstall(targets),
        SkillCommand::Status => status(),
        SkillCommand::Print => {
            print!("{HERDR_SKILL}");
            Ok(0)
        }
    }
}

fn parse_skill_args(args: &[String]) -> Result<SkillCommand, String> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err(String::new());
    };
    let mut force = false;
    let mut targets = Vec::new();
    for arg in rest {
        match arg.as_str() {
            "--force" if subcommand == "install" => force = true,
            label if !label.starts_with('-') => match SkillTarget::parse(label) {
                Some(target) if !targets.contains(&target) => targets.push(target),
                Some(_) => {}
                None => return Err(format!("unknown skill target: {label}")),
            },
            other => return Err(format!("unknown option: {other}")),
        }
    }
    match subcommand.as_str() {
        "install" => Ok(SkillCommand::Install { targets, force }),
        "uninstall" => Ok(SkillCommand::Uninstall { targets }),
        "status" | "print" if !targets.is_empty() => {
            Err(format!("herdr skill {subcommand} takes no targets"))
        }
        "status" => Ok(SkillCommand::Status),
        "print" => Ok(SkillCommand::Print),
        other => Err(format!("unknown skill command: {other}")),
    }
}

fn install(targets: Vec<SkillTarget>, force: bool) -> std::io::Result<i32> {
    let implicit = targets.is_empty();
    let targets = if implicit {
        SkillTarget::ALL.to_vec()
    } else {
        targets
    };

    let mut exit_code = 0;
    let mut attempted = false;
    for target in targets {
        if implicit && !target.detected()? {
            println!("{}: skipped (agent not found)", target.label());
            continue;
        }
        attempted = true;
        let skill_dir = skill_dir_in(&target.skills_dir()?);
        let path = skill_file_in(&skill_dir);
        match install_skill_at(&skill_dir, HERDR_SKILL, force) {
            Ok(outcome) => {
                let (ok, message) = describe_install(target, &path.display().to_string(), outcome);
                if ok {
                    println!("{message}");
                } else {
                    eprintln!("{message}");
                    exit_code = 1;
                }
            }
            Err(err) => {
                eprintln!("{}: {err} ({})", target.label(), path.display());
                exit_code = 1;
            }
        }
    }
    if !attempted {
        eprintln!("no supported agent found; name a target to install anyway");
        eprintln!("{USAGE}");
        return Ok(1);
    }
    Ok(exit_code)
}

fn uninstall(targets: Vec<SkillTarget>) -> std::io::Result<i32> {
    let explicit = !targets.is_empty();
    let targets = if explicit {
        targets
    } else {
        SkillTarget::ALL.to_vec()
    };

    let mut exit_code = 0;
    for target in targets {
        let skill_dir = skill_dir_in(&target.skills_dir()?);
        let path = skill_file_in(&skill_dir);
        match uninstall_skill_at(&skill_dir, HERDR_SKILL, explicit) {
            Ok(outcome) => {
                let Some((ok, message)) =
                    describe_uninstall(target, &path.display().to_string(), outcome, explicit)
                else {
                    continue;
                };
                if ok {
                    println!("{message}");
                } else {
                    eprintln!("{message}");
                    exit_code = 1;
                }
            }
            Err(err) => {
                eprintln!("{}: {err} ({})", target.label(), path.display());
                exit_code = 1;
            }
        }
    }
    Ok(exit_code)
}

fn status() -> std::io::Result<i32> {
    let mut exit_code = 0;
    for target in SkillTarget::ALL {
        let skill_dir = skill_dir_in(&target.skills_dir()?);
        let path = skill_file_in(&skill_dir);
        // One unreadable skill must not hide the state of the other targets.
        match skill_state_at(&skill_dir, HERDR_SKILL) {
            Ok(state) => println!(
                "{}: {} ({})",
                target.label(),
                describe_state(state),
                path.display()
            ),
            Err(err) => {
                eprintln!("{}: {err} ({})", target.label(), path.display());
                exit_code = 1;
            }
        }
    }
    Ok(exit_code)
}

fn describe_state(state: SkillState) -> &'static str {
    match state {
        SkillState::NotInstalled => "not installed",
        SkillState::Current => "current",
        SkillState::Outdated => "outdated",
        SkillState::Linked => "symlink",
        SkillState::Modified => "changed since herdr installed it",
        SkillState::Foreign => "not installed by herdr",
    }
}

/// Returns whether the outcome succeeded, with the line to print.
fn describe_install(target: SkillTarget, path: &str, outcome: InstallOutcome) -> (bool, String) {
    let label = target.label();
    match outcome {
        InstallOutcome::Installed => (true, format!("{label}: installed {path}")),
        InstallOutcome::Updated => (true, format!("{label}: updated {path}")),
        InstallOutcome::AlreadyCurrent => (true, format!("{label}: current {path}")),
        InstallOutcome::Refused(state) => (
            false,
            format!(
                "{label}: kept {path} ({}); rerun with --force to replace it",
                describe_state(state)
            ),
        ),
    }
}

/// `None` hides targets that were never installed when the user named none.
fn describe_uninstall(
    target: SkillTarget,
    path: &str,
    outcome: UninstallOutcome,
    explicit: bool,
) -> Option<(bool, String)> {
    let label = target.label();
    match outcome {
        UninstallOutcome::Removed => Some((true, format!("{label}: removed {path}"))),
        UninstallOutcome::Unlinked => Some((true, format!("{label}: removed symlink {path}"))),
        UninstallOutcome::NotInstalled if explicit => {
            Some((true, format!("{label}: not installed")))
        }
        UninstallOutcome::NotInstalled => None,
        UninstallOutcome::Refused(state) if explicit => Some((
            false,
            format!("{label}: kept {path} ({})", describe_state(state)),
        )),
        UninstallOutcome::Refused(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn install_without_targets_means_detected_agents() {
        assert_eq!(
            parse_skill_args(&args(&["install"])),
            Ok(SkillCommand::Install {
                targets: vec![],
                force: false
            })
        );
    }

    #[test]
    fn install_accepts_targets_and_force_in_any_order() {
        assert_eq!(
            parse_skill_args(&args(&["install", "--force", "claude", "agents", "claude"])),
            Ok(SkillCommand::Install {
                targets: vec![SkillTarget::Claude, SkillTarget::Agents],
                force: true
            })
        );
    }

    #[test]
    fn unknown_targets_and_misplaced_flags_are_rejected() {
        assert!(parse_skill_args(&args(&["install", "codex"])).is_err());
        assert!(parse_skill_args(&args(&["uninstall", "--force"])).is_err());
        assert!(parse_skill_args(&args(&["status", "claude"])).is_err());
        assert!(parse_skill_args(&args(&["remove"])).is_err());
        assert!(parse_skill_args(&args(&[])).is_err());
    }

    #[test]
    fn refused_install_points_at_force() {
        let (ok, message) = describe_install(
            SkillTarget::Claude,
            "/h/.claude/skills/herdr/SKILL.md",
            InstallOutcome::Refused(SkillState::Linked),
        );

        assert!(!ok);
        assert_eq!(
            message,
            "claude: kept /h/.claude/skills/herdr/SKILL.md (symlink); rerun with --force to replace it"
        );
    }

    #[test]
    fn a_changed_copy_is_described_as_changed() {
        let (ok, message) = describe_install(
            SkillTarget::Agents,
            "p",
            InstallOutcome::Refused(SkillState::Modified),
        );

        assert!(!ok);
        assert_eq!(
            message,
            "agents: kept p (changed since herdr installed it); rerun with --force to replace it"
        );
    }

    #[test]
    fn implicit_uninstall_stays_quiet_about_targets_it_does_not_own() {
        for outcome in [
            UninstallOutcome::NotInstalled,
            UninstallOutcome::Refused(SkillState::Foreign),
        ] {
            assert_eq!(
                describe_uninstall(SkillTarget::Cursor, "p", outcome, false),
                None
            );
        }
        assert_eq!(
            describe_uninstall(
                SkillTarget::Cursor,
                "p",
                UninstallOutcome::Refused(SkillState::Foreign),
                true
            ),
            Some((false, "cursor: kept p (not installed by herdr)".to_string()))
        );
    }
}
