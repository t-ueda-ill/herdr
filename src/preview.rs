//! Live preview: serves a folder from this machine, an ssh pane, or a
//! container pane to the browser over loopback HTTP, renders formats such as
//! Markdown, and reloads the page when a file under the folder changes.

pub(crate) mod http;
pub(crate) mod page;
pub(crate) mod registry;
pub(crate) mod route;
pub(crate) mod server;
pub(crate) mod source;
