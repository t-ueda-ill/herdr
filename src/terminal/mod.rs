mod history_read;
mod id;
mod runtime;
mod runtime_registry;
pub mod state;
mod title;

pub(crate) use history_read::{merge_scrolled_up, snapshot_text, ScreenSnapshot, UpwardMerge};
pub use id::TerminalId;
pub use runtime::TerminalRuntime;
pub(crate) use runtime_registry::TerminalRuntimeRegistry;
pub use state::{
    inherited_cwd, AgentMetadataReport, EffectivePresentation, EffectiveStateChange, TerminalState,
    TerminalStateMutation,
};
// Why test-only: outside the terminal module only tests build one by name.
#[cfg(test)]
pub use state::PendingOffHostConnect;
pub(crate) use title::stripped_terminal_title;
