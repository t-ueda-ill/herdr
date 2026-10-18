//! Panes whose shell is `ssh <alias>` for a `Host` alias of the user's own
//! ssh config. Why not under `src/remote/`: that module is the herdr-to-herdr
//! transport, while an ssh pane needs no herdr on the far side.

// Why unix only: ssh panes are unsupported on Windows hosts, whose OpenSSH
// cannot multiplex, and the side channel only joins the pane's master.
#[cfg(unix)]
pub(crate) mod clipboard;
pub(crate) mod config;
pub(crate) mod control;
pub(crate) mod hosts;
pub(crate) mod launch;

pub(crate) use config::SshHostInfo;
pub(crate) use hosts::SshHostError;
pub(crate) use launch::SshLaunch;
