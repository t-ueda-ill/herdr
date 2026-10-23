//! What upload and download sessions share: the app thread's table of live
//! sessions. Each direction keeps its own instance, so a long upload cannot
//! starve downloads of slots.

pub(crate) mod registry;
