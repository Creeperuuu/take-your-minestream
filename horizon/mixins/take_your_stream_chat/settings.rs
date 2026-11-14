// The mod's settings page: chat on or off, where it shows, and the channel.
fn take_your_stream_chat_page(game: &mut crate::game::Game<'_>, mod_id: &str, buttons: &mut Vec<String>) {
    use crate::game::take_your_stream_chat as m;
    if mod_id != m::ID {
        return;
    }
    m::ensure_loaded(|key| game.native_setting(m::ID, key));
    buttons.push(m::connection_label());
    buttons.push(m::mode_label());
    buttons.push(m::channel_label());
}
