// A click on the mod's settings page.
fn take_your_stream_chat_click(game: &mut crate::game::Game<'_>, mod_id: &str, button: usize) {
    use crate::game::take_your_stream_chat as m;
    if mod_id != m::ID {
        return;
    }
    m::ensure_loaded(|key| game.native_setting(m::ID, key));
    if let Some((key, value)) = m::click(button) {
        game.native_set_setting(m::ID, key, &value);
    }
}
