//! Dropping files onto a terminal pane uploads them into the pane's
//! environment and pastes their remote paths (spec Q1-Q20, design §3).
//!
//! Pure decisions live in `drop_text`, `gate`, `limits`, `paste`, `pending`,
//! and `transfer`; file reads happen on the `worker` thread, and the client
//! loop only executes the effects `transfer` returns.

pub(super) mod drop_text;
pub(super) mod gate;
pub(super) mod limits;
pub(super) mod paste;
pub(super) mod pending;
pub(super) mod runtime;
pub(super) mod transfer;
pub(super) mod worker;

/// The `ui.upload_*` settings the client reads (spec Q7, Q16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UploadSettings {
    pub(crate) upload_on_drop: bool,
    pub(crate) limits: crate::config::UploadLimits,
}

impl UploadSettings {
    pub(crate) fn from_config(config: &crate::config::Config) -> Self {
        Self {
            upload_on_drop: config.ui.upload_on_drop,
            limits: config.ui.upload_limits(),
        }
    }
}
