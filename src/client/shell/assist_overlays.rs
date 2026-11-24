use super::*;

pub(super) fn render_assist_overlay(
    b: &mut Buffer,
    picker: &ClientAssistOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let names: Vec<&str> = picker
        .products
        .iter()
        .map(|found| super::assists::product_name(found.product))
        .collect();
    super::list_picker_overlay::render_list_picker_overlay(
        b,
        &super::list_picker_overlay::ListPicker {
            title: "ask AI",
            filter_hint: "",
            count_noun: "CLIs",
            empty_message: " no AI CLI on the server",
            total: picker.products.len(),
            counted: None,
            filtered: (0..picker.products.len()).collect(),
            selected: picker.selected,
            query: &picker.query,
            search_focused: false,
            opening: picker.starting,
            error: picker.error.as_deref(),
            favorites: false,
            action: Some(super::assists::placement_button(picker.placement)),
        },
        |index| super::list_picker_overlay::PickerRow {
            title: names[index],
            status: "",
            detail: &picker.products[index].path,
            favorite: false,
            unstarred: false,
        },
        p,
    )
}
