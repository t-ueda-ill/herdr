//! AI assist: a coding-agent CLI on the server's machine, opened beside a pane
//! to read what that pane shows and advise the user. Pure bookkeeping here;
//! the App spawns the pane and writes the files.

mod context;
mod links;
mod products;

pub(crate) use context::{render_context, AssistContext, ContextRead};
pub(crate) use links::{AssistLink, AssistLinks};
pub(crate) use products::{ask_line, find_products, product_argv, refresh_line};

use crate::api::schema::{AssistPlacement, SplitDirection};

/// Panes at least this wide get the assist on their right.
pub(crate) const WIDE_PANE_COLS: u16 = 120;
/// Commands written to the context file, newest last.
pub(crate) const CONTEXT_COMMANDS: usize = 3;
/// Lines kept from each command's output, and from the screen without marks.
pub(crate) const CONTEXT_LINES: usize = 200;

/// The split that opens the assist, and the share the read pane keeps.
pub(crate) fn split_for(placement: AssistPlacement, target_cols: u16) -> (SplitDirection, f32) {
    let right = match placement {
        AssistPlacement::Right => true,
        AssistPlacement::Below => false,
        AssistPlacement::Auto => target_cols >= WIDE_PANE_COLS,
    };
    if right {
        (SplitDirection::Right, 0.6)
    } else {
        (SplitDirection::Down, 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_pane_gets_the_assist_on_its_right_with_two_fifths_of_the_width() {
        assert_eq!(
            split_for(AssistPlacement::Auto, 120),
            (SplitDirection::Right, 0.6)
        );
    }

    #[test]
    fn a_narrow_pane_gets_the_assist_in_its_lower_half() {
        assert_eq!(
            split_for(AssistPlacement::Auto, 119),
            (SplitDirection::Down, 0.5)
        );
    }

    #[test]
    fn an_explicit_placement_wins_over_the_width() {
        assert_eq!(
            split_for(AssistPlacement::Below, 300).0,
            SplitDirection::Down
        );
        assert_eq!(
            split_for(AssistPlacement::Right, 40).0,
            SplitDirection::Right
        );
    }
}
