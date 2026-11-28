//! Opens URLs in a browser off the client loop and reports back whether one
//! opened, so a failure reaches the screen instead of only a log.

use super::events::ClientLoopEvent;

/// Where an open came from, which decides the command and where a failure
/// is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlOrigin {
    /// A link in a pane.
    Link,
    /// A preview, started from that files tab or (`None`) the picker.
    Preview { tab: Option<u64> },
}

#[derive(Default)]
pub(crate) struct UrlLauncher {
    events: Option<tokio::sync::mpsc::Sender<ClientLoopEvent>>,
    /// `[preview] open_command`; empty uses the platform's openers.
    preview_command: Vec<String>,
}

impl UrlLauncher {
    pub(crate) fn new(preview_command: Vec<String>) -> Self {
        Self {
            events: None,
            preview_command,
        }
    }

    pub(crate) fn connect(&mut self, events: tokio::sync::mpsc::Sender<ClientLoopEvent>) {
        self.events = Some(events);
    }

    /// Why a thread: each opener may take seconds to answer, and the loop
    /// must keep drawing meanwhile.
    pub(crate) fn open(&self, url: String, origin: UrlOrigin) {
        let Some(events) = self.events.clone() else {
            tracing::debug!("no client loop to report a browser open to");
            return;
        };
        let command = match origin {
            UrlOrigin::Preview { .. } => self.preview_command.clone(),
            UrlOrigin::Link => Vec::new(),
        };
        let spawned = crate::thread_spawn::spawn_named("herdr-url-open", move || {
            let (result, child) = match crate::url_open::open(&url, &command) {
                Ok(child) => (Ok(()), child),
                Err(reason) => (Err(reason), None),
            };
            let _ = events.blocking_send(ClientLoopEvent::UrlOpened {
                url,
                origin,
                result,
            });
            // Reaps an opener that stays until its browser closes.
            if let Some(mut child) = child {
                let _ = child.wait();
            }
        });
        if let Err(err) = spawned {
            tracing::warn!(%err, "could not start a thread to open a browser");
        }
    }
}
