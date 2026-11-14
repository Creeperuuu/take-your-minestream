// Take Your Stream Chat for Horizon: live Twitch chat on the HUD and floating in the
// world. This file is both the mod's Cargo library (its tests) and the module the
// bundle adds to the game, so it uses only `std`.
//
// Twitch is read anonymously (a `justinfan` nick, no token) over plain IRC on port 6667.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The mod's id (its settings folder), name and version.
pub const ID: &str = "take_your_stream_chat";
pub const NAME: &str = "Take Your Stream Chat";
pub const VERSION: &str = "0.1.0";
pub const DESCRIPTION: &str = "Live Twitch chat on your HUD and floating in the world. /streamchat chat start, /streamchat channel <name>.";

/// Messages kept, lines of the HUD, and how long each kind of message stays.
pub const MAX_MESSAGES: usize = 50;
pub const HUD_ENTRIES: usize = 8;
pub const HUD_WRAP: usize = 48;
pub const HUD_LIFETIME: Duration = Duration::from_secs(30);
pub const WORLD_LIFETIME: Duration = Duration::from_secs(20);
pub const WORLD_FADE: Duration = Duration::from_secs(3);
pub const WORLD_MAX_CHARS: usize = 80;
pub const BACKGROUND: u32 = 0x6000_0000;
pub const INK: u32 = 0xFF_FFFF;

const IRC_HOST: &str = "irc.chat.twitch.tv";
const IRC_PORT: u16 = 6667;

/// Where messages show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Hud,
    World,
    Both,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "hud" => Some(Mode::Hud),
            "world" | "3d" => Some(Mode::World),
            "both" => Some(Mode::Both),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Mode::Hud => "hud",
            Mode::World => "world",
            Mode::Both => "both",
        }
    }
    pub fn next(self) -> Mode {
        match self {
            Mode::Hud => Mode::World,
            Mode::World => Mode::Both,
            Mode::Both => Mode::Hud,
        }
    }
    pub fn hud(self) -> bool {
        matches!(self, Mode::Hud | Mode::Both)
    }
    pub fn world(self) -> bool {
        matches!(self, Mode::World | Mode::Both)
    }
}

/// One chat message. `anchor` is where it floats in the world, set when first drawn.
#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub user: String,
    pub text: String,
    /// `0xFFRRGGBB`.
    pub color: u32,
    pub born: Instant,
    pub anchor: Option<[f64; 3]>,
    seq: u64,
}

struct State {
    messages: VecDeque<ChatMessage>,
    mode: Mode,
    channel: String,
    status: String,
    banwords: Vec<String>,
    blocked: Vec<String>,
    loaded: bool,
    seq: u64,
}

static STATE: Mutex<State> = Mutex::new(State {
    messages: VecDeque::new(),
    mode: Mode::Both,
    channel: String::new(),
    status: String::new(),
    banwords: Vec::new(),
    blocked: Vec::new(),
    loaded: false,
    seq: 0,
});

/// Bumped by every connect and disconnect: a connection thread ends when it is no longer
/// the current one.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- settings

fn split_list(stored: Option<String>) -> Vec<String> {
    stored
        .map(|s| {
            s.split(',')
                .map(|w| w.trim().to_ascii_lowercase())
                .filter(|w| !w.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Reads the saved settings once, through `get` (a settings file by name).
pub fn ensure_loaded(get: impl Fn(&str) -> Option<String>) {
    let mut s = state();
    if s.loaded {
        return;
    }
    s.loaded = true;
    if let Some(mode) = get("mode").as_deref().and_then(Mode::parse) {
        s.mode = mode;
    }
    if let Some(channel) = get("channel").as_deref().and_then(clean_channel) {
        s.channel = channel;
    }
    s.banwords = split_list(get("banwords"));
    s.blocked = split_list(get("blocked"));
}

pub fn mode() -> Mode {
    state().mode
}

pub fn channel() -> String {
    state().channel.clone()
}

pub fn is_connected() -> bool {
    GENERATION.load(Ordering::SeqCst) % 2 == 1
}

pub fn status() -> String {
    state().status.clone()
}

fn set_status(gen: u64, text: &str) {
    if GENERATION.load(Ordering::SeqCst) == gen {
        state().status = text.to_string();
    }
}

/// A channel name as Twitch has them: letters, digits and `_`, up to 25.
pub fn clean_channel(raw: &str) -> Option<String> {
    let name: String = raw
        .trim()
        .trim_start_matches('#')
        .to_ascii_lowercase();
    let ok = !name.is_empty()
        && name.len() <= 25
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(name)
}

// ---------------------------------------------------------------- IRC

fn unescape_tag(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some(':') => out.push(';'),
            Some('\\') => out.push('\\'),
            Some('r') | Some('n') => out.push(' '),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// A colour for a user who has none: the same name always gets the same one.
pub fn default_color(user: &str) -> u32 {
    const PALETTE: [u32; 8] = [
        0xFF_FF5555,
        0xFF_55FF55,
        0xFF_5555FF,
        0xFF_FFAA00,
        0xFF_55FFFF,
        0xFF_FF55FF,
        0xFF_FFFF55,
        0xFF_AAAAAA,
    ];
    let h = user
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[h as usize % PALETTE.len()]
}

/// `(user, text, colour)` of a `PRIVMSG` line, with or without IRCv3 tags.
pub fn parse_privmsg(line: &str) -> Option<(String, String, u32)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (tags, rest) = match line.strip_prefix('@') {
        Some(t) => t.split_once(' ')?,
        None => ("", line),
    };
    let rest = rest.strip_prefix(':')?;
    let (prefix, rest) = rest.split_once(' ')?;
    let rest = rest.strip_prefix("PRIVMSG ")?;
    let (_target, text) = rest.split_once(" :")?;

    let mut display = None;
    let mut color = None;
    for tag in tags.split(';') {
        match tag.split_once('=') {
            Some(("display-name", v)) if !v.is_empty() => display = Some(unescape_tag(v)),
            Some(("color", v)) => {
                color = v
                    .strip_prefix('#')
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .filter(|_| v.len() == 7)
                    .map(|rgb| 0xFF00_0000 | rgb);
            }
            _ => {}
        }
    }
    let nick = prefix.split('!').next().unwrap_or("").to_string();
    let user = display.unwrap_or(nick);
    if user.is_empty() {
        return None;
    }
    let text = text
        .strip_prefix("\u{1}ACTION ")
        .map(|t| t.trim_end_matches('\u{1}'))
        .unwrap_or(text)
        .to_string();
    let color = color.unwrap_or_else(|| default_color(&user));
    Some((user, text, color))
}

/// Connects to `channel`'s chat on a background thread, dropping any earlier connection.
pub fn connect(channel: &str) {
    // Odd generation = connected, even = off.
    let mut gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    if gen % 2 == 0 {
        gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    }
    let channel = channel.to_string();
    state().status = format!("Connecting to #{channel}...");
    std::thread::spawn(move || run_connection(&channel, gen));
}

/// Ends the connection.
pub fn disconnect() {
    let gen = GENERATION.load(Ordering::SeqCst);
    if gen % 2 == 1 {
        GENERATION.fetch_add(1, Ordering::SeqCst);
    }
    state().status = "Disconnected".to_string();
}

fn run_connection(channel: &str, gen: u64) {
    while GENERATION.load(Ordering::SeqCst) == gen {
        match read_chat(channel, gen) {
            Ok(()) => set_status(gen, "Connection closed, retrying..."),
            Err(e) => set_status(gen, &format!("Connection failed ({e}), retrying...")),
        }
        for _ in 0..10 {
            if GENERATION.load(Ordering::SeqCst) != gen {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

fn read_chat(channel: &str, gen: u64) -> std::io::Result<()> {
    let mut stream = TcpStream::connect((IRC_HOST, IRC_PORT))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let nick = format!(
        "justinfan{}",
        10000 + (Instant::now().elapsed().subsec_nanos() ^ std::process::id()) % 80000
    );
    write!(
        stream,
        "CAP REQ :twitch.tv/tags\r\nNICK {nick}\r\nJOIN #{channel}\r\n"
    )?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    set_status(gen, &format!("Connected to #{channel}"));
    while GENERATION.load(Ordering::SeqCst) == gen {
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(()),
            Ok(_) => {
                if line.ends_with('\n') {
                    if line.starts_with("PING") {
                        stream.write_all(b"PONG :tmi.twitch.tv\r\n")?;
                    } else if let Some((user, text, color)) = parse_privmsg(&line) {
                        push_message(&user, &text, color);
                    }
                    line.clear();
                }
            }
            // A timeout may leave a partial line in `line`; keep it and read on.
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- messages

/// Whether `user`'s `text` shows, given the banned words and blocked users.
pub fn passes(user: &str, text: &str, banwords: &[String], blocked: &[String]) -> bool {
    let user = user.to_ascii_lowercase();
    if blocked.iter().any(|b| *b == user) {
        return false;
    }
    let text = text.to_ascii_lowercase();
    !banwords.iter().any(|w| text.contains(w.as_str()))
}

/// Adds a message from the chat, unless it is filtered.
pub fn push_message(user: &str, text: &str, color: u32) {
    let mut s = state();
    if !passes(user, text, &s.banwords, &s.blocked) {
        return;
    }
    add(&mut s, user, text, color);
}

fn add(s: &mut State, user: &str, text: &str, color: u32) {
    s.seq += 1;
    let seq = s.seq;
    s.messages.push_back(ChatMessage {
        user: user.to_string(),
        text: text.to_string(),
        color,
        born: Instant::now(),
        anchor: None,
        seq,
    });
    while s.messages.len() > MAX_MESSAGES {
        s.messages.pop_front();
    }
}

/// Greedy word wrap at `width` characters; a longer word is cut.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        while word.chars().count() > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let head: String = word.chars().take(width).collect();
            word = word.chars().skip(width).collect();
            lines.push(head);
        }
        let need = if cur.is_empty() { 0 } else { cur.chars().count() + 1 };
        if need + word.chars().count() > width && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&word);
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// One message of the HUD: the user, then the wrapped lines of the text.
#[derive(Clone, Debug, PartialEq)]
pub struct HudEntry {
    pub user: String,
    pub lines: Vec<String>,
    pub color: u32,
}

/// The newest messages younger than `HUD_LIFETIME`, oldest first.
pub fn hud_entries() -> Vec<HudEntry> {
    let s = state();
    let mut out: Vec<HudEntry> = s
        .messages
        .iter()
        .rev()
        .filter(|m| m.born.elapsed() < HUD_LIFETIME)
        .take(HUD_ENTRIES)
        .map(|m| HudEntry {
            user: m.user.clone(),
            lines: wrap(&m.text, HUD_WRAP),
            color: m.color,
        })
        .collect();
    out.reverse();
    out
}

/// splitmix64, for a message's place in the world: no random crate here.
fn mix(mut x: u64) -> f64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((x ^ (x >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}

/// The unit vectors of a yaw (0 faces south, +Z; 90 west, -X): forward and right.
pub fn yaw_axes(yaw: f32) -> ([f64; 2], [f64; 2]) {
    let (s, c) = (yaw as f64).to_radians().sin_cos();
    ([-s, c], [-c, -s])
}

/// Where a new message floats: in front of the camera, spread to the sides and in height.
pub fn spawn_anchor(eye: [f64; 3], yaw: f32, seq: u64) -> [f64; 3] {
    let (fwd, right) = yaw_axes(yaw);
    let side = (mix(seq) - 0.5) * 4.0;
    let up = mix(seq ^ 0xABCD) * 1.2 - 0.3;
    let dist = 3.5;
    [
        eye[0] + fwd[0] * dist + right[0] * side,
        eye[1] + up,
        eye[2] + fwd[1] * dist + right[1] * side,
    ]
}

/// A message floating in the world: its place, text and `0xAARRGGBB` colour.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldText {
    pub at: [f64; 3],
    pub text: String,
    pub color: u32,
}

pub fn shorten(user: &str, text: &str, max: usize) -> String {
    let line = format!("{user}: {text}");
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Alpha of a floating message `age` after it came: opaque, then fading out.
pub fn world_alpha(age: Duration) -> u32 {
    let fade_from = WORLD_LIFETIME.saturating_sub(WORLD_FADE);
    if age <= fade_from {
        return 255;
    }
    let left = WORLD_LIFETIME.saturating_sub(age).as_secs_f64();
    (255.0 * (left / WORLD_FADE.as_secs_f64())).clamp(0.0, 255.0) as u32
}

/// The floating messages for this frame, placing new ones in front of `eye` facing `yaw`.
pub fn world_texts(eye: [f64; 3], yaw: f32) -> Vec<WorldText> {
    let mut s = state();
    let mut out = Vec::new();
    for m in s.messages.iter_mut() {
        let age = m.born.elapsed();
        if age >= WORLD_LIFETIME {
            continue;
        }
        let anchor = *m
            .anchor
            .get_or_insert_with(|| spawn_anchor(eye, yaw, m.seq));
        let rise = age.as_secs_f64() * 0.05;
        out.push(WorldText {
            at: [anchor[0], anchor[1] + rise, anchor[2]],
            text: shorten(&m.user, &m.text, WORLD_MAX_CHARS),
            color: (world_alpha(age) << 24) | (m.color & 0x00FF_FFFF),
        });
    }
    out
}

// ---------------------------------------------------------------- commands

/// What a `/streamchat` command did: lines for the chat box, settings to keep, and
/// whether the connection should be (re)started or stopped.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub lines: Vec<String>,
    pub save: Vec<(&'static str, String)>,
}

pub fn help() -> Vec<String> {
    [
        "Take Your Stream Chat (Twitch)",
        "/streamchat channel <name>: the channel to read",
        "/streamchat chat start | stop: connect or disconnect",
        "/streamchat mode hud | world | both: where messages show",
        "/streamchat test <text>: a test message",
        "/streamchat banword <word> | blockuser <name>: add or remove from the filters",
        "/streamchat clear | status",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn toggle(list: &mut Vec<String>, item: &str) -> bool {
    let item = item.trim().to_ascii_lowercase();
    if let Some(i) = list.iter().position(|w| *w == item) {
        list.remove(i);
        false
    } else {
        list.push(item);
        true
    }
}

/// Runs the arguments of `/streamchat` (or `/minestream`).
pub fn run_command(args: &str) -> Outcome {
    let mut out = Outcome::default();
    let args = args.trim();
    let (sub, rest) = args.split_once(' ').unwrap_or((args, ""));
    let rest = rest.trim();
    match sub.to_ascii_lowercase().as_str() {
        "channel" => match clean_channel(rest) {
            Some(name) => {
                state().channel = name.clone();
                out.save.push(("channel", name.clone()));
                if is_connected() {
                    connect(&name);
                    out.lines.push(format!("Switching to #{name}"));
                } else {
                    out.lines
                        .push(format!("Channel set to #{name}. /streamchat chat start"));
                }
            }
            None => out
                .lines
                .push("Usage: /streamchat channel <twitch name>".into()),
        },
        "chat" => match rest.to_ascii_lowercase().as_str() {
            "start" => {
                let channel = channel();
                if channel.is_empty() {
                    out.lines
                        .push("Set a channel first: /streamchat channel <name>".into());
                } else {
                    connect(&channel);
                    out.lines.push(format!("Connecting to #{channel}..."));
                }
            }
            "stop" => {
                disconnect();
                out.lines.push("Chat disconnected".into());
            }
            _ => out.lines.push("Usage: /streamchat chat start | stop".into()),
        },
        "mode" => match Mode::parse(rest) {
            Some(mode) => {
                state().mode = mode;
                out.save.push(("mode", mode.name().into()));
                out.lines.push(format!("Mode: {}", mode.name()));
            }
            None => out
                .lines
                .push("Usage: /streamchat mode hud | world | both".into()),
        },
        "test" if !rest.is_empty() => {
            let mut s = state();
            add(&mut s, "Test", rest, 0xFF_FFAA00);
        }
        "banword" | "blockuser" if !rest.is_empty() => {
            let word = sub.eq_ignore_ascii_case("banword");
            let mut s = state();
            let (list, key) = if word {
                (&mut s.banwords, "banwords")
            } else {
                (&mut s.blocked, "blocked")
            };
            let added = toggle(list, rest);
            out.save.push((key, list.join(",")));
            out.lines.push(format!(
                "{} {}: {}",
                if word { "Banword" } else { "Blocked user" },
                if added { "added" } else { "removed" },
                rest.to_ascii_lowercase()
            ));
        }
        "clear" => {
            state().messages.clear();
            out.lines.push("Chat cleared".into());
        }
        "status" => {
            let s = state();
            let channel = if s.channel.is_empty() {
                "none".to_string()
            } else {
                format!("#{}", s.channel)
            };
            let line = format!(
                "Channel: {channel}, mode: {}, {}",
                s.mode.name(),
                if s.status.is_empty() { "not connected" } else { &s.status }
            );
            out.lines.push(line);
        }
        _ => out.lines = help(),
    }
    out
}

// ---------------------------------------------------------------- settings page

pub fn connection_label() -> String {
    format!(
        "Chat: {}",
        if is_connected() { "ON" } else { "OFF" }
    )
}

pub fn mode_label() -> String {
    format!("Show: {}", mode().name())
}

pub fn channel_label() -> String {
    let c = channel();
    if c.is_empty() {
        "Channel: none (/streamchat channel <name>)".into()
    } else {
        format!("Channel: #{c}")
    }
}

/// A click on a settings button. Returns the setting to keep, if any.
pub fn click(button: usize) -> Option<(&'static str, String)> {
    match button {
        0 => {
            if is_connected() {
                disconnect();
            } else {
                let c = channel();
                if !c.is_empty() {
                    connect(&c);
                }
            }
            None
        }
        1 => {
            let mode = {
                let mut s = state();
                s.mode = s.mode.next();
                s.mode
            };
            Some(("mode", mode.name().into()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_tagged_privmsg() {
        let line = "@badge-info=;color=#1E90FF;display-name=Some\\sOne;id=1 :someone!someone@someone.tmi.twitch.tv PRIVMSG #chan :hello there\r\n";
        let (user, text, color) = parse_privmsg(line).unwrap();
        assert_eq!(user, "Some One");
        assert_eq!(text, "hello there");
        assert_eq!(color, 0xFF1E_90FF);
    }

    #[test]
    fn parses_an_untagged_action_and_defaults_the_colour() {
        let line = ":bob!bob@bob.tmi.twitch.tv PRIVMSG #chan :\u{1}ACTION waves\u{1}";
        let (user, text, color) = parse_privmsg(line).unwrap();
        assert_eq!((user.as_str(), text.as_str()), ("bob", "waves"));
        assert_eq!(color, default_color("bob"));
    }

    #[test]
    fn ignores_other_lines() {
        assert!(parse_privmsg(":tmi.twitch.tv 001 justinfan1 :Welcome, GLHF!").is_none());
        assert!(parse_privmsg("PING :tmi.twitch.tv").is_none());
    }

    #[test]
    fn channel_names_are_cleaned() {
        assert_eq!(clean_channel(" #Foo_1 "), Some("foo_1".into()));
        assert_eq!(clean_channel("bad name"), None);
        assert_eq!(clean_channel(""), None);
    }

    #[test]
    fn wrap_breaks_on_words_and_cuts_long_ones() {
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), [""]);
    }

    #[test]
    fn filters_hide_words_and_users() {
        let words = vec!["spam".to_string()];
        let users = vec!["troll".to_string()];
        assert!(passes("fan", "hi", &words, &users));
        assert!(!passes("fan", "buy SPAM now", &words, &users));
        assert!(!passes("Troll", "hi", &words, &users));
    }

    #[test]
    fn floating_text_is_in_front_of_the_camera() {
        // Facing south (+Z): in front means a larger z.
        let a = spawn_anchor([0.0, 64.0, 0.0], 0.0, 1);
        assert!(a[2] > 3.0);
        // Facing west (-X): in front means a smaller x.
        let b = spawn_anchor([0.0, 64.0, 0.0], 90.0, 1);
        assert!(b[0] < -3.0);
    }

    #[test]
    fn floating_text_fades_out() {
        assert_eq!(world_alpha(Duration::from_secs(1)), 255);
        assert_eq!(world_alpha(WORLD_LIFETIME), 0);
        let mid = world_alpha(WORLD_LIFETIME - Duration::from_millis(1500));
        assert!((120..=135).contains(&mid));
    }

    #[test]
    fn shorten_keeps_short_lines_and_cuts_long_ones() {
        assert_eq!(shorten("a", "b", 10), "a: b");
        assert_eq!(shorten("user", "0123456789", 8).chars().count(), 8);
    }

    #[test]
    fn modes_cycle_and_parse() {
        assert_eq!(Mode::parse("3D"), Some(Mode::World));
        assert_eq!(Mode::Hud.next().next().next(), Mode::Hud);
        assert!(Mode::Both.hud() && Mode::Both.world());
    }
}
