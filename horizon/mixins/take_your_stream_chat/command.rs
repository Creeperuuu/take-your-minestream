// /streamchat (and the original mod's /minestream alias).
fn take_your_stream_chat_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    use crate::game::take_your_stream_chat as m;
    let (name, args) = command.split_once(' ').unwrap_or((command, ""));
    if !game.native_on(m::ID) || !matches!(name, "streamchat" | "minestream") {
        return;
    }
    *ran = true;
    m::ensure_loaded(|key| game.native_setting(m::ID, key));
    let outcome = m::run_command(args);
    for (key, value) in &outcome.save {
        game.native_set_setting(m::ID, key, value);
    }
    for line in &outcome.lines {
        game.native_chat(line);
    }
}
