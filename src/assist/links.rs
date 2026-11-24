//! Which assist reads which pane. Keyed by terminal, which a pane keeps
//! through swaps and moves.

use std::path::PathBuf;

use crate::api::schema::AssistProduct;
use crate::terminal::TerminalId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssistLink {
    pub(crate) helper: TerminalId,
    /// `None` once the read pane has closed. The assist stays open so the
    /// user can read its answers.
    pub(crate) target: Option<TerminalId>,
    pub(crate) product: AssistProduct,
    pub(crate) context_path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AssistLinks {
    links: Vec<AssistLink>,
    /// Context files of closed assists, for the App to delete.
    removed_files: Vec<PathBuf>,
}

impl AssistLinks {
    pub(crate) fn insert(&mut self, link: AssistLink) {
        self.links.retain(|existing| existing.helper != link.helper);
        self.links.push(link);
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &AssistLink> {
        self.links.iter()
    }

    pub(crate) fn for_target(&self, target: &TerminalId) -> Option<&AssistLink> {
        self.links
            .iter()
            .find(|link| link.target.as_ref() == Some(target))
    }

    pub(crate) fn for_helper(&self, helper: &TerminalId) -> Option<&AssistLink> {
        self.links.iter().find(|link| &link.helper == helper)
    }

    /// The link of an assist, or of the pane an assist reads.
    pub(crate) fn for_either(&self, terminal: &TerminalId) -> Option<&AssistLink> {
        self.for_helper(terminal)
            .or_else(|| self.for_target(terminal))
    }

    pub(crate) fn is_helper(&self, terminal: &TerminalId) -> bool {
        self.for_helper(terminal).is_some()
    }

    /// Forgets a closed terminal: an assist goes with its context file, a read
    /// pane leaves its assist without a target.
    pub(crate) fn forget_terminal(&mut self, terminal: &TerminalId) {
        if let Some(index) = self.links.iter().position(|link| &link.helper == terminal) {
            let link = self.links.remove(index);
            self.removed_files.push(link.context_path);
        }
        for link in &mut self.links {
            if link.target.as_ref() == Some(terminal) {
                link.target = None;
            }
        }
    }

    pub(crate) fn take_removed_files(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.removed_files)
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.links.is_empty() && self.removed_files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(helper: &TerminalId, target: &TerminalId) -> AssistLink {
        AssistLink {
            helper: helper.clone(),
            target: Some(target.clone()),
            product: AssistProduct::Claude,
            context_path: PathBuf::from(format!("/run/{helper}.md")),
        }
    }

    #[test]
    fn a_link_is_found_from_the_assist_and_from_the_read_pane() {
        let (helper, target) = (TerminalId::alloc(), TerminalId::alloc());
        let mut links = AssistLinks::default();
        links.insert(link(&helper, &target));

        assert_eq!(links.for_either(&helper), links.for_either(&target));
        assert!(links.for_either(&helper).is_some());
        assert!(links.is_helper(&helper));
        assert!(!links.is_helper(&target));
    }

    #[test]
    fn closing_the_assist_drops_the_link_and_queues_its_file() {
        let (helper, target) = (TerminalId::alloc(), TerminalId::alloc());
        let mut links = AssistLinks::default();
        links.insert(link(&helper, &target));

        links.forget_terminal(&helper);

        assert!(links.for_target(&target).is_none());
        assert_eq!(
            links.take_removed_files(),
            [PathBuf::from(format!("/run/{helper}.md"))]
        );
        assert!(links.is_empty());
    }

    #[test]
    fn closing_the_read_pane_keeps_the_assist_without_a_target() {
        let (helper, target) = (TerminalId::alloc(), TerminalId::alloc());
        let mut links = AssistLinks::default();
        links.insert(link(&helper, &target));

        links.forget_terminal(&target);

        assert_eq!(links.for_helper(&helper).unwrap().target, None);
        assert!(links.take_removed_files().is_empty());
    }
}
