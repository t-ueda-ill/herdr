//! ssh hosts the user marked as protected by naming their config alias with a
//! prefix (`[offhost] protected_host_prefix`, default `noref_`).
//!
//! A pane on a protected host shares no ssh connection, so a local process
//! cannot reach the host through herdr's `ControlPath`, and the herdr API
//! refuses to read or drive it. The prefix is fixed when the server starts.

use std::sync::OnceLock;

pub(crate) const DEFAULT_PREFIX: &str = "noref_";

/// Why fixed at start: `server.reload_config` is itself a public API method,
/// so a process that may not touch the host could otherwise edit the config
/// and reload it to lift the protection.
static PREFIX: OnceLock<String> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtectedHosts {
    prefix: String,
}

impl ProtectedHosts {
    pub(crate) fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_ascii_lowercase(),
        }
    }

    /// True when the config alias of `destination` (`alias` or `user@alias`)
    /// starts with the prefix. Why case-insensitive: OpenSSH matches `Host`
    /// patterns without case, so `NOREF_prod` reaches the same entry.
    pub(crate) fn covers(&self, destination: &str) -> bool {
        if self.prefix.is_empty() {
            return false;
        }
        let alias = destination
            .rsplit_once('@')
            .map_or(destination, |(_, alias)| alias);
        alias.to_ascii_lowercase().starts_with(&self.prefix)
    }
}

/// Fixes the prefix for this process. Later calls keep the first value.
pub(crate) fn init(prefix: &str) {
    if PREFIX.set(prefix.to_owned()).is_err() {
        tracing::debug!("protected ssh host prefix already fixed for this server");
    }
}

/// The prefix fixed at server start, or the default before [`init`].
pub(crate) fn current() -> ProtectedHosts {
    ProtectedHosts::new(PREFIX.get().map_or(DEFAULT_PREFIX, String::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alias_with_the_prefix_is_protected() {
        let hosts = ProtectedHosts::new("noref_");
        assert!(hosts.covers("noref_prod"));
        assert!(!hosts.covers("prod"));
        assert!(!hosts.covers("prod_noref_"));
    }

    #[test]
    fn the_user_part_of_a_destination_is_not_the_alias() {
        let hosts = ProtectedHosts::new("noref_");
        assert!(hosts.covers("deploy@noref_prod"));
        assert!(!hosts.covers("noref_deploy@prod"));
    }

    #[test]
    fn the_prefix_matches_without_case() {
        assert!(ProtectedHosts::new("noref_").covers("NoRef_Prod"));
        assert!(ProtectedHosts::new("NOREF_").covers("noref_prod"));
    }

    #[test]
    fn an_empty_prefix_protects_no_host() {
        assert!(!ProtectedHosts::new("").covers("prod"));
    }

    #[test]
    fn the_default_prefix_applies_before_the_server_fixes_one() {
        // Tests never call `init`, so the process keeps the default.
        assert!(current().covers("noref_prod"));
    }
}
