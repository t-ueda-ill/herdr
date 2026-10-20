//! Reading the filesystem a pane's shell sees when that shell runs off-host
//! (ssh, container). Everything here is blocking process I/O: callers run it on
//! a worker thread, never on the app thread or in render/layout loops.

pub(crate) mod editor;
pub(crate) mod exec;
pub(crate) mod ops;
pub(crate) mod parse;
pub(crate) mod scripts;

pub(crate) use exec::PaneEnvironment;

use crate::container::ContainerLaunch;

/// Where a pane is, from what the app thread already holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneTarget {
    pub(crate) environment: PaneEnvironment,
    /// Where the pane's shell is, when known without asking it: the OSC 7
    /// report, else the container's working directory, else `/` for a
    /// container. `None` for an ssh pane that has not reported, whose root is
    /// then the login directory.
    pub(crate) root: Option<String>,
    /// The directory the pane itself was started or last reported in.
    pub(crate) cwd: Option<String>,
}

impl PaneTarget {
    /// Why `/` for a container without a working directory: the image's
    /// `WORKDIR` is unknown there, and the filesystem root always lists. An ssh
    /// pane has no such default; its script starts in the login directory.
    pub(crate) fn resolve(environment: PaneEnvironment, reported_cwd: Option<String>) -> Self {
        let launch_dir = match &environment {
            PaneEnvironment::Container(ContainerLaunch { workdir, .. }) => workdir
                .as_ref()
                .and_then(|dir| dir.to_str())
                .map(str::to_owned),
            PaneEnvironment::Ssh(_) => None,
        };
        let cwd = reported_cwd.or(launch_dir);
        let root = cwd.clone().or_else(|| match &environment {
            PaneEnvironment::Container(_) => Some("/".to_owned()),
            PaneEnvironment::Ssh(_) => None,
        });
        Self {
            environment,
            root,
            cwd,
        }
    }
}

// The tests run the scripts with the local `/bin/sh`, git, and Unix paths.
#[cfg(all(test, unix))]
mod tests;
