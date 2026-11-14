// The mod's settings page: each service on or off, where the chat shows, and the names.
fn take_your_stream_chat_page(game: &mut crate::game::Game<'_>, mod_id: &str, buttons: &mut Vec<String>) {
    use crate::game::take_your_stream_chat as m;
    if mod_id != m::ID {
        return;
    }
    m::ensure_loaded(|key| game.native_setting(m::ID, key));
    buttons.push(m::connection_label(m::Platform::Twitch));
    buttons.push(m::connection_label(m::Platform::TikTok));
    buttons.push(m::mode_label());
    buttons.push(m::names_label());
}
