use super::*;

pub(super) fn render_ssh_open_overlay(
    b: &mut Buffer,
    open: &ClientSshOpenOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    super::list_picker_overlay::render_list_picker_overlay(
        b,
        &super::list_picker_overlay::ListPicker {
            title: "open ssh host",
            filter_hint: "filter hosts",
            count_noun: "hosts",
            empty_message: " no matching hosts",
            total: open.entries.len(),
            filtered: open.filtered_indices(),
            selected: open.selected,
            query: &open.query,
            search_focused: open.search_focused,
            opening: open.opening,
            error: open.error.as_deref(),
        },
        |index| {
            let entry = &open.entries[index];
            super::list_picker_overlay::PickerRow {
                title: &entry.alias,
                status: "",
                detail: &entry.detail,
            }
        },
        p,
    )
}
