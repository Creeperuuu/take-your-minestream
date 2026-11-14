// The chat on the HUD: a box in the top-right corner, newest message at the bottom.
fn take_your_stream_chat_hud(
    game: &mut crate::game::Game<'_>,
    batch: &mut horizon_client::hud::HudBatch,
    corners: &mut horizon_client::extras_hud::Corners,
) {
    use crate::game::take_your_stream_chat as m;
    use horizon_client::extras::Corner;
    if !game.native_on(m::ID) || !game.native_corner_shown(Corner::TopRight) {
        return;
    }
    m::ensure_loaded(|key| game.native_setting(m::ID, key));
    if !m::mode().hud() {
        return;
    }
    let entries = m::hud_entries();
    if entries.is_empty() {
        return;
    }
    const LINE: f32 = 10.0;
    const INDENT: f32 = 6.0;
    // Width: each entry's widest line, the first one after its "user: ".
    let mut width = 0.0f32;
    let mut rows = 0usize;
    for e in &entries {
        let prefix = game.native_text_width(&format!("{}: ", e.user));
        for (i, line) in e.lines.iter().enumerate() {
            let x = if i == 0 { prefix } else { INDENT };
            width = width.max(x + game.native_text_width(line));
            rows += 1;
        }
    }
    let w = width + 4.0;
    let h = LINE * rows as f32 + 2.0;
    let (x, y) = corners.place(Corner::TopRight, w, h);
    game.native_fill(batch, [x, y, w, h], m::BACKGROUND);
    let mut row = 0usize;
    for e in &entries {
        let prefix = game.native_text_width(&format!("{}: ", e.user));
        for (i, line) in e.lines.iter().enumerate() {
            let top = y + 2.0 + LINE * row as f32;
            if i == 0 {
                game.native_text(batch, &format!("{}: ", e.user), [x + 2.0, top], e.color, true);
                game.native_text(batch, line, [x + 2.0 + prefix, top], m::INK, true);
            } else {
                game.native_text(batch, line, [x + 2.0 + INDENT, top], m::INK, true);
            }
            row += 1;
        }
    }
}
