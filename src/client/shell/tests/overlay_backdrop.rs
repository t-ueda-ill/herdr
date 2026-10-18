use super::*;
use ratatui::style::{Color, Modifier, Style};

fn shell_state_with_surface_buffer(
    buffer: &Buffer,
    links: &[((u16, u16), String, String)],
) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut surface = surface();
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(buffer, None, links);
    let rect = SurfaceRect {
        x: 0,
        y: 0,
        width: buffer.area.width,
        height: buffer.area.height,
    };
    surface.panes[0].rect = rect;
    surface.panes[0].inner_rect = rect;
    state.set_pane_surface(surface);
    state
}

fn open_picker(state: &mut ClientShellState) {
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenSshWorkspace),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("open ssh host should list hosts through ssh.hosts");
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::SshHosts {
            hosts: ["prod", "lab", "staging"]
                .into_iter()
                .map(|alias| crate::api::schema::SshHostInfoDto {
                    alias: alias.into(),
                    hostname: None,
                    user: None,
                })
                .collect(),
        }),
    );
}

fn cell(frame: &FrameData, x: u16, y: u16) -> &crate::protocol::CellData {
    &frame.cells[usize::from(y) * usize::from(frame.width) + usize::from(x)]
}

const POPUP: Rect = Rect {
    x: 5,
    y: 8,
    width: 96,
    height: 13,
};

#[test]
fn modal_cells_carry_no_underline_or_strikethrough_from_the_background() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 28));
    buffer.set_style(
        buffer.area,
        Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::UNDERLINED | Modifier::CROSSED_OUT),
    );
    let mut state = shell_state_with_surface_buffer(&buffer, &[]);
    open_picker(&mut state);

    let frame = state.compose(106, 30).expect("compose").frame;

    let leaked = Modifier::UNDERLINED | Modifier::CROSSED_OUT;
    for y in POPUP.y..POPUP.bottom() {
        for x in POPUP.x..POPUP.right() {
            let c = cell(&frame, x, y);
            assert_eq!(
                c.modifier & leaked.bits(),
                0,
                "cell ({x},{y}) {:?} kept a background modifier",
                c.symbol
            );
        }
    }
}

#[test]
fn modal_blank_cells_do_not_keep_the_background_foreground() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 28));
    buffer.set_style(buffer.area, Style::default().fg(Color::Rgb(250, 1, 2)));
    let mut state = shell_state_with_surface_buffer(&buffer, &[]);
    open_picker(&mut state);

    let frame = state.compose(106, 30).expect("compose").frame;

    let leaked = crate::protocol::color_to_u32(Color::Rgb(250, 1, 2));
    assert_ne!(cell(&frame, 40, 19).fg, leaked);
}

#[test]
fn border_is_drawn_next_to_a_wide_character_on_its_left() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 38));
    buffer[(5, 13)].set_symbol("あ");
    buffer[(6, 13)].set_symbol("");
    let mut state = shell_state_with_surface_buffer(&buffer, &[]);
    let before = state.compose(160, 40).expect("compose").frame;
    open_picker(&mut state);

    let frame = state.compose(160, 40).expect("compose").frame;

    assert_eq!(cell(&frame, 31, 14).symbol, " ");
    assert_eq!(cell(&frame, 32, 14).symbol, "│");

    // The encoder never emits the cell after a width-2 head, so the byte
    // stream must contain the border glyph itself, not only the wide glyph.
    let mut encoder = crate::protocol::render_ansi::BlitEncoder::new();
    let first = encoder.encode(&before, true);
    encoder.commit(before, first);
    let out = encoder.encode(&frame, false);
    let text = String::from_utf8_lossy(&out.bytes);
    let row = text
        .split("\x1b[16;")
        .nth(1)
        .expect("row 15 should be repainted");
    let row = row.split("\x1b[16;").next().unwrap_or(row);
    assert!(row.contains('│'), "row 15 bytes: {row:?}");
    assert!(!row.contains('あ'), "row 15 bytes: {row:?}");
}

#[test]
fn modal_cells_carry_no_hyperlink_from_background_whitespace() {
    let buffer = Buffer::empty(Rect::new(0, 0, 80, 28));
    let links: Vec<((u16, u16), String, String)> = (0..28u16)
        .flat_map(|y| {
            (0..60u16).map(move |x| ((x, y), " ".to_string(), "https://example.com".to_string()))
        })
        .collect();
    let mut state = shell_state_with_surface_buffer(&buffer, &links);
    let before = state.compose(106, 30).expect("compose").frame;
    assert!(before.cells.iter().any(|c| c.hyperlink.is_some()));
    open_picker(&mut state);

    let frame = state.compose(106, 30).expect("compose").frame;

    for y in POPUP.y..POPUP.bottom() {
        for x in POPUP.x..POPUP.right() {
            assert!(
                cell(&frame, x, y).hyperlink.is_none(),
                "cell ({x},{y}) kept a background hyperlink"
            );
        }
    }
}
