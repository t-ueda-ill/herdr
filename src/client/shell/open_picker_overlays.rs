use super::*;

pub(super) fn render_open_picker_overlay(
    b: &mut Buffer,
    open: &ClientOpenPickerOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let (title, filter_hint, count_noun, empty_message) = match open.source {
        OpenPickerSource::SshHost => (
            "open ssh host",
            "filter hosts",
            "hosts",
            " no matching hosts",
        ),
        OpenPickerSource::Container => (
            "open container",
            "filter containers",
            "containers",
            " no matching containers",
        ),
    };
    super::list_picker_overlay::render_list_picker_overlay(
        b,
        &super::list_picker_overlay::ListPicker {
            title,
            filter_hint,
            count_noun,
            empty_message,
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
                title: &entry.title,
                status: entry.status,
                detail: &entry.detail,
            }
        },
        p,
    )
}
