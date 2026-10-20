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
    let filtered = open.filtered_indices();
    super::list_picker_overlay::render_list_picker_overlay(
        b,
        &super::list_picker_overlay::ListPicker {
            title,
            filter_hint,
            count_noun,
            empty_message,
            total: open.entries.len(),
            counted: (open.source == OpenPickerSource::ContainerHost).then(|| {
                // Favorite container rows are not hosts, so the "hosts" label skips them.
                let is_host = |entry: &ClientOpenPickerEntry| {
                    matches!(entry.kind, super::OpenPickerRowKind::Host(_))
                };
                (
                    filtered
                        .iter()
                        .filter(|index| open.entries.get(**index).is_some_and(is_host))
                        .count(),
                    open.entries.iter().filter(|entry| is_host(entry)).count(),
                )
            }),
            filtered,
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
                unstarred: !entry.favorite
                    && matches!(entry.kind, super::OpenPickerRowKind::FavoriteContainer(_)),
            }
        },
        p,
    )
}
