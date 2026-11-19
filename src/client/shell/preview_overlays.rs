use super::*;

/// What the stop button says; Delete is its key.
const STOP_BUTTON: &str = " del stop ";

pub(super) fn render_previews_overlay(
    b: &mut Buffer,
    previews: &ClientPreviewsOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let places: Vec<String> = previews.entries.iter().map(preview_place).collect();
    let addresses: Vec<String> = previews
        .entries
        .iter()
        .map(|entry| format!("http://127.0.0.1:{}", entry.port))
        .collect();
    super::list_picker_overlay::render_list_picker_overlay(
        b,
        &super::list_picker_overlay::ListPicker {
            title: "previews",
            filter_hint: "filter previews",
            count_noun: "previews",
            empty_message: " no previews running",
            total: previews.entries.len(),
            counted: None,
            filtered: previews.filtered_indices(),
            selected: previews.selected_entry_index().unwrap_or(0),
            query: &previews.query,
            search_focused: previews.search_focused,
            opening: false,
            error: previews.error.as_deref(),
            favorites: false,
            action: (!previews.entries.is_empty()).then_some(STOP_BUTTON),
        },
        |index| super::list_picker_overlay::PickerRow {
            title: &previews.entries[index].root,
            status: &places[index],
            detail: &addresses[index],
            favorite: false,
            unstarred: false,
        },
        p,
    )
}
