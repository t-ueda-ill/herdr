use super::*;

pub(super) fn render_open_picker_overlay(
    b: &mut Buffer,
    open: &ClientOpenPickerOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let container_title = format!("open container · {}", open.host.display_name());
    let (title, filter_hint, count_noun, empty_message) = match open.source {
        OpenPickerSource::SshHost => (
            "open ssh host",
            "filter hosts",
            "hosts",
            " no matching hosts",
        ),
        OpenPickerSource::ContainerHost => (
            "open container",
            "filter hosts",
            "hosts",
            " no matching hosts",
        ),
        OpenPickerSource::Container => (
            container_title.as_str(),
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
            favorites: true,
        },
        |index| {
            let entry = &open.entries[index];
            super::list_picker_overlay::PickerRow {
                title: &entry.title,
                status: entry.status,
                detail: &entry.detail,
                favorite: entry.favorite,
            }
        },
        p,
    )
}
