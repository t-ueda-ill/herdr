pub(crate) mod clipboard;
pub(crate) mod docker;
pub(crate) mod labels;
pub(crate) mod launch;
pub(crate) mod types;
pub(crate) mod workdir;

pub(crate) use types::{
    ContainerInfo, ContainerLaunch, ContainerListError, ContainerRef, ContainerShell,
};
// Why not export these in every build: outside this module only tests name
// them, so an unconditional re-export fails `-D warnings` as unused.
#[cfg(test)]
pub(crate) use types::{ComposeInfo, HerdrLabels};
