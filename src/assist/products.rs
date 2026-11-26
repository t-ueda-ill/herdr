//! Which coding-agent CLIs the server's machine has, and the argv that opens
//! one as an assist.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::api::schema::AssistProduct;

/// Products in the order offered; the first one found is the default.
const PRODUCTS: [AssistProduct; 3] = [
    AssistProduct::Claude,
    AssistProduct::Codex,
    AssistProduct::Copilot,
];

/// Why only `.exe` on Windows: a `.cmd` shim needs `cmd /c` to start, and the
/// assist starts its argv directly.
fn executable_name(product: AssistProduct) -> Option<String> {
    let name = match product {
        AssistProduct::Claude => "claude",
        AssistProduct::Codex => "codex",
        AssistProduct::Copilot => "copilot",
        AssistProduct::Unknown => return None,
    };
    Some(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    })
}

/// The products whose executable is in a directory of `path`, with the first
/// match for each, in the order of [`PRODUCTS`].
pub(crate) fn find_products(
    path: Option<&OsStr>,
    is_executable: impl Fn(&Path) -> bool,
) -> Vec<(AssistProduct, PathBuf)> {
    let Some(path) = path else {
        return Vec::new();
    };
    let dirs: Vec<PathBuf> = std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .collect();
    PRODUCTS
        .into_iter()
        .filter_map(|product| {
            let name = executable_name(product)?;
            dirs.iter()
                .map(|dir| dir.join(&name))
                .find(|candidate| is_executable(candidate))
                .map(|found| (product, found))
        })
        .collect()
}

/// The first prompt the assist gets. The pane's text stays in the file, so it
/// is not in the argv that `ps` shows.
pub(crate) fn ask_line(context_path: &Path, pane_id: &str) -> String {
    format!(
        "Read {}. It holds what the herdr pane {pane_id} shows: its recent commands, \
         their output and its prompt. Help the user with what they do next in that pane. \
         Do not type into that pane and do not run herdr commands; suggest commands for the \
         user to run. Then wait for the user's question.",
        context_path.display()
    )
}

/// The prompt that tells the assist its context file has new content.
pub(crate) fn refresh_line(context_path: &Path) -> String {
    format!(
        "herdr wrote the latest output of the pane to {}. Read it again before you answer.",
        context_path.display()
    )
}

/// What the assist may run besides reading files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellAccess {
    /// The product's shell, minus `herdr`.
    WithoutHerdr,
    /// No shell and no MCP tools, for a pane on a protected ssh host: a
    /// shell could reach the host with the user's own `ssh` and keys.
    None,
}

/// The argv that opens `product` as an assist, or `None` for a product this
/// server does not know.
///
/// Why the deny rules: the assist only advises, and herdr's CLI is how a
/// process in a pane would type into another pane. The rules are a guard of
/// each product, not a boundary: a process in a pane can reach the socket.
pub(crate) fn product_argv(
    product: AssistProduct,
    executable: &Path,
    ask: &str,
    context_dir: &Path,
    shell: ShellAccess,
) -> Option<Vec<String>> {
    let executable = executable.to_string_lossy().into_owned();
    let context_dir = context_dir.to_string_lossy().into_owned();
    let ask = ask.to_owned();
    Some(match (product, shell) {
        // Why the prompt first: `--allowedTools` and `--disallowedTools`
        // take every argument after them up to the next option.
        // Why a read rule and not `--add-dir`: claude asks whether to trust
        // an added folder, and each assist has a new one. A leading `//`
        // makes the rule's path absolute.
        (AssistProduct::Claude, ShellAccess::WithoutHerdr) => vec![
            executable,
            ask,
            "--allowedTools".into(),
            format!("Read(/{context_dir}/**)"),
            "--disallowedTools".into(),
            "Bash(herdr:*)".into(),
        ],
        // Why `--tools` and not `--restricted`: restricted mode confines
        // reads to the working directory, and the context folder is outside
        // it. `mcp__*` also removes tools that plugins bring, which
        // `--strict-mcp-config` leaves in place.
        (AssistProduct::Claude, ShellAccess::None) => vec![
            executable,
            ask,
            "--tools".into(),
            "Read,Grep,Glob".into(),
            "--strict-mcp-config".into(),
            "--allowedTools".into(),
            format!("Read(/{context_dir}/**)"),
            "--disallowedTools".into(),
            "mcp__*".into(),
        ],
        // The read-only sandbox reads any path, so no directory is added.
        (AssistProduct::Codex, ShellAccess::WithoutHerdr) => {
            vec![executable, "--sandbox".into(), "read-only".into(), ask]
        }
        // Why keep codex's shell: it reads files with it. The read-only
        // sandbox refuses network and socket connections, so `ssh` cannot
        // reach the host; `never` keeps the model from asking to run a
        // command outside the sandbox.
        (AssistProduct::Codex, ShellAccess::None) => vec![
            executable,
            "--sandbox".into(),
            "read-only".into(),
            "--ask-for-approval".into(),
            "never".into(),
            ask,
        ],
        (AssistProduct::Copilot, ShellAccess::WithoutHerdr) => vec![
            executable,
            "-i".into(),
            ask,
            "--add-dir".into(),
            context_dir,
            "--deny-tool=shell(herdr:*)".into(),
        ],
        // A bare `shell` rule denies every shell command, and deny rules win
        // over every allow. MCP servers the user configured still ask first.
        (AssistProduct::Copilot, ShellAccess::None) => vec![
            executable,
            "-i".into(),
            ask,
            "--add-dir".into(),
            context_dir,
            "--deny-tool=shell".into(),
            "--disable-builtin-mcps".into(),
        ],
        (AssistProduct::Unknown, _) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_of(dirs: &[&str]) -> std::ffi::OsString {
        std::env::join_paths(dirs).unwrap()
    }

    // Why unix only: a path like `/opt/bin` is not absolute on Windows.
    #[cfg(unix)]
    #[test]
    fn products_are_found_in_path_order_and_listed_in_product_order() {
        let path = path_of(&["/opt/bin", "/usr/bin"]);
        let present = ["/usr/bin/copilot", "/opt/bin/claude", "/usr/bin/claude"].map(PathBuf::from);

        let found = find_products(Some(&path), |candidate| {
            present.iter().any(|p| p == candidate)
        });

        assert_eq!(
            found,
            vec![
                (AssistProduct::Claude, present[1].clone()),
                (AssistProduct::Copilot, present[0].clone()),
            ]
        );
    }

    #[test]
    fn no_path_finds_no_product() {
        assert!(find_products(None, |_| true).is_empty());
    }

    #[test]
    fn relative_path_entries_are_ignored() {
        let path = path_of(&["bin"]);

        assert!(find_products(Some(&path), |_| true).is_empty());
    }

    #[test]
    fn claude_may_read_only_the_context_folder_without_asking() {
        let argv = product_argv(
            AssistProduct::Claude,
            Path::new("/bin/claude"),
            "ASK",
            Path::new("/run/assist"),
            ShellAccess::WithoutHerdr,
        )
        .unwrap();

        assert_eq!(
            argv,
            [
                "/bin/claude",
                "ASK",
                "--allowedTools",
                "Read(//run/assist/**)",
                "--disallowedTools",
                "Bash(herdr:*)"
            ]
        );
    }

    #[test]
    fn codex_runs_in_the_read_only_sandbox() {
        let argv = product_argv(
            AssistProduct::Codex,
            Path::new("/bin/codex"),
            "ASK",
            Path::new("/run/assist"),
            ShellAccess::WithoutHerdr,
        )
        .unwrap();

        assert_eq!(argv, ["/bin/codex", "--sandbox", "read-only", "ASK"]);
    }

    #[test]
    fn copilot_starts_interactive_with_the_prompt_and_denies_herdr() {
        let argv = product_argv(
            AssistProduct::Copilot,
            Path::new("/bin/copilot"),
            "ASK",
            Path::new("/run/assist"),
            ShellAccess::WithoutHerdr,
        )
        .unwrap();

        assert_eq!(
            argv,
            [
                "/bin/copilot",
                "-i",
                "ASK",
                "--add-dir",
                "/run/assist",
                "--deny-tool=shell(herdr:*)"
            ]
        );
    }

    #[test]
    fn claude_for_a_protected_pane_gets_only_file_reading_tools() {
        let argv = product_argv(
            AssistProduct::Claude,
            Path::new("/bin/claude"),
            "ASK",
            Path::new("/run/assist"),
            ShellAccess::None,
        )
        .unwrap();

        assert_eq!(
            argv,
            [
                "/bin/claude",
                "ASK",
                "--tools",
                "Read,Grep,Glob",
                "--strict-mcp-config",
                "--allowedTools",
                "Read(//run/assist/**)",
                "--disallowedTools",
                "mcp__*"
            ]
        );
    }

    #[test]
    fn codex_for_a_protected_pane_never_leaves_the_read_only_sandbox() {
        let argv = product_argv(
            AssistProduct::Codex,
            Path::new("/bin/codex"),
            "ASK",
            Path::new("/run/assist"),
            ShellAccess::None,
        )
        .unwrap();

        assert_eq!(
            argv,
            [
                "/bin/codex",
                "--sandbox",
                "read-only",
                "--ask-for-approval",
                "never",
                "ASK"
            ]
        );
    }

    #[test]
    fn copilot_for_a_protected_pane_denies_every_shell_command() {
        let argv = product_argv(
            AssistProduct::Copilot,
            Path::new("/bin/copilot"),
            "ASK",
            Path::new("/run/assist"),
            ShellAccess::None,
        )
        .unwrap();

        assert_eq!(
            argv,
            [
                "/bin/copilot",
                "-i",
                "ASK",
                "--add-dir",
                "/run/assist",
                "--deny-tool=shell",
                "--disable-builtin-mcps"
            ]
        );
    }

    #[test]
    fn an_unknown_product_has_no_argv() {
        assert_eq!(
            product_argv(
                AssistProduct::Unknown,
                Path::new("/bin/x"),
                "ASK",
                Path::new("/run"),
                ShellAccess::WithoutHerdr,
            ),
            None
        );
    }

    #[test]
    fn the_first_prompt_names_the_file_and_forbids_typing_into_the_pane() {
        let ask = ask_line(Path::new("/run/assist/a.md"), "w1:p2");

        assert!(ask.contains("/run/assist/a.md"));
        assert!(ask.contains("pane w1:p2 shows"));
        assert!(ask.contains("Do not type into that pane"));
        assert!(!ask.contains('\n'));
    }
}
