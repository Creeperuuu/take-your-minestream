// Take Your Stream Chat for Horizon: live Twitch and TikTok chat on the HUD and floating
// in the world. This file is both the mod's Cargo library (its tests) and the module the
// bundle adds to the game, so it uses only `std`; HTTPS and gzip live in `tiktok_net.rs`.
//
// Twitch is read anonymously (a `justinfan` nick, no token) over plain IRC on port 6667.
// TikTok is read by polling its web `im/fetch` endpoint, as the original mod's bootstrap does.

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The mod's id (its settings folder), name and version.
pub const ID: &str = "take_your_stream_chat";
pub const NAME: &str = "Take Your Stream Chat";
pub const VERSION: &str = "0.2.0";
pub const DESCRIPTION: &str = "Live Twitch and TikTok chat on your HUD and floating in the world. /streamchat channel <name>, /streamchat tiktok <name>, /streamchat chat start.";

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

/// A chat service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Twitch,
    TikTok,
}

impl Platform {
    pub const ALL: [Platform; 2] = [Platform::Twitch, Platform::TikTok];
    pub fn parse(s: &str) -> Option<Platform> {
        match s.trim().to_ascii_lowercase().as_str() {
            "twitch" => Some(Platform::Twitch),
            "tiktok" | "tt" => Some(Platform::TikTok),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Platform::Twitch => "Twitch",
            Platform::TikTok => "TikTok",
        }
    }
    /// Shown before the user's name, so the services can be told apart; Twitch, the first
    /// one, has none.
    pub fn tag(self) -> &'static str {
        match self {
            Platform::Twitch => "",
            Platform::TikTok => "[TT] ",
        }
    }
    fn index(self) -> usize {
        self as usize
    }
    fn link(self) -> &'static Link {
        match self {
            Platform::Twitch => &TWITCH,
            Platform::TikTok => &TIKTOK,
        }
    }
}

/// One platform's connection thread. The count is odd while it should run, even when off;
/// every start and stop changes it, so a thread ends when it is no longer the current one.
struct Link(AtomicU64);

impl Link {
    fn is_on(&self) -> bool {
        self.0.load(Ordering::SeqCst) % 2 == 1
    }
    fn current(&self, epoch: u64) -> bool {
        self.0.load(Ordering::SeqCst) == epoch
    }
    /// A new generation, odd, dropping any earlier connection.
    fn start(&self) -> u64 {
        let mut epoch = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        if epoch % 2 == 0 {
            epoch = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        }
        epoch
    }
    fn stop(&self) {
        if self.is_on() {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

static TWITCH: Link = Link(AtomicU64::new(0));
static TIKTOK: Link = Link(AtomicU64::new(0));

/// One chat message. `anchor` is where it floats in the world, set when first drawn.
#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub platform: Platform,
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
    /// The Twitch channel and the TikTok user to read.
    channel: String,
    tiktok_user: String,
    /// TikTok session cookie the player pasted, optional.
    tiktok_session: String,
    /// One line per platform, by `Platform::index`.
    status: [String; 2],
    banwords: Vec<String>,
    blocked: Vec<String>,
    loaded: bool,
    seq: u64,
}

static STATE: Mutex<State> = Mutex::new(State {
    messages: VecDeque::new(),
    mode: Mode::Both,
    channel: String::new(),
    tiktok_user: String::new(),
    tiktok_session: String::new(),
    status: [String::new(), String::new()],
    banwords: Vec::new(),
    blocked: Vec::new(),
    loaded: false,
    seq: 0,
});

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
    if let Some(user) = get("tiktok_user").as_deref().and_then(clean_tiktok_user) {
        s.tiktok_user = user;
    }
    s.tiktok_session = get("tiktok_session").map(|v| clean_cookie(&v)).unwrap_or_default();
    s.banwords = split_list(get("banwords"));
    s.blocked = split_list(get("blocked"));
}

pub fn mode() -> Mode {
    state().mode
}

pub fn channel() -> String {
    state().channel.clone()
}

pub fn tiktok_user() -> String {
    state().tiktok_user.clone()
}

/// The name a platform reads, empty when none is set.
pub fn target(p: Platform) -> String {
    match p {
        Platform::Twitch => channel(),
        Platform::TikTok => tiktok_user(),
    }
}

pub fn is_connected(p: Platform) -> bool {
    p.link().is_on()
}

pub fn status(p: Platform) -> String {
    state().status[p.index()].clone()
}

fn set_status(p: Platform, epoch: u64, text: &str) {
    if p.link().current(epoch) {
        state().status[p.index()] = text.to_string();
    }
}

/// A channel name as Twitch has them: letters, digits and `_`, up to 25.
pub fn clean_channel(raw: &str) -> Option<String> {
    let name: String = raw.trim().trim_start_matches('#').to_ascii_lowercase();
    let ok = !name.is_empty()
        && name.len() <= 25
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(name)
}

/// A TikTok `@name`: letters, digits, `_` and `.`, up to 24.
pub fn clean_tiktok_user(raw: &str) -> Option<String> {
    let name: String = raw.trim().trim_start_matches('@').to_ascii_lowercase();
    let ok = !name.is_empty()
        && name.len() <= 24
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    ok.then_some(name)
}

/// A pasted cookie without line breaks or other control characters.
pub fn clean_cookie(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()
}

// ---------------------------------------------------------------- IRC (Twitch)

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

/// Starts reading `p`'s chat on a background thread, dropping any earlier connection.
/// False when no name is set for it.
pub fn connect(p: Platform) -> bool {
    let name = target(p);
    if name.is_empty() {
        return false;
    }
    let epoch = p.link().start();
    state().status[p.index()] = format!("Connecting to {}...", name);
    match p {
        Platform::Twitch => {
            std::thread::spawn(move || run_twitch(&name, epoch));
        }
        Platform::TikTok => {
            let session = state().tiktok_session.clone();
            std::thread::spawn(move || run_tiktok(&name, &session, epoch));
        }
    }
    true
}

/// Ends `p`'s connection.
pub fn disconnect(p: Platform) {
    p.link().stop();
    state().status[p.index()] = "Disconnected".to_string();
}

/// Sleeps up to `ms`, in short steps; false when `p`'s connection was replaced or ended.
fn pause(p: Platform, epoch: u64, ms: u64) -> bool {
    let mut left = ms;
    while left > 0 {
        if !p.link().current(epoch) {
            return false;
        }
        let step = left.min(250);
        std::thread::sleep(Duration::from_millis(step));
        left -= step;
    }
    p.link().current(epoch)
}

fn run_twitch(channel: &str, epoch: u64) {
    let p = Platform::Twitch;
    while p.link().current(epoch) {
        match read_twitch(channel, epoch) {
            Ok(()) => set_status(p, epoch, "Connection closed, retrying..."),
            Err(e) => set_status(p, epoch, &format!("Connection failed ({e}), retrying...")),
        }
        if !pause(p, epoch, 5000) {
            return;
        }
    }
}

fn read_twitch(channel: &str, epoch: u64) -> std::io::Result<()> {
    let p = Platform::Twitch;
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
    set_status(p, epoch, &format!("Connected to #{channel}"));
    while p.link().current(epoch) {
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(()),
            Ok(_) => {
                if line.ends_with('\n') {
                    if line.starts_with("PING") {
                        stream.write_all(b"PONG :tmi.twitch.tv\r\n")?;
                    } else if let Some((user, text, color)) = parse_privmsg(&line) {
                        push_message(p, &user, &text, color);
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

// ---------------------------------------------------------------- TikTok

/// The HTTP and gzip the TikTok reader needs. In the game they come from
/// `tiktok_net.rs` (ureq and flate2, which the game already has); tests get no network.
#[cfg(test)]
mod net {
    pub struct Reply {
        pub body: Vec<u8>,
        pub set_cookies: Vec<String>,
    }
    pub fn request(_post_json: Option<&str>, _url: &str, _headers: &[(&str, &str)]) -> Result<Reply, String> {
        Err("no network in tests".into())
    }
    pub fn gunzip(_data: &[u8]) -> Result<Vec<u8>, String> {
        Err("no gzip in tests".into())
    }
}
#[cfg(not(test))]
use super::take_your_stream_chat_net as net;

/// A TikTok live's chat is read through the web client's `webcast/im/fetch` endpoint and
/// its protobuf, as the original mod's bootstrap and fallback polling do.
pub mod tiktok {
    use super::readable_text;

    // ------------------------------------------------ protobuf wire format

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Val<'a> {
        Varint(u64),
        Bytes(&'a [u8]),
        Fixed(u64),
    }

    /// The fields of one protobuf message, in order. Only what the chat needs is read.
    #[derive(Clone, Debug, Default)]
    pub struct Proto<'a> {
        fields: Vec<(u32, Val<'a>)>,
    }

    fn varint(data: &[u8], pos: &mut usize) -> Result<u64, String> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = *data.get(*pos).ok_or("truncated varint")?;
            *pos += 1;
            if shift < 64 {
                value |= u64::from(byte & 0x7F) << shift;
            }
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("varint too long".into())
    }

    impl<'a> Proto<'a> {
        pub fn parse(data: &'a [u8]) -> Result<Proto<'a>, String> {
            let mut fields = Vec::new();
            let mut pos = 0;
            while pos < data.len() {
                let key = varint(data, &mut pos)?;
                let field = (key >> 3) as u32;
                let val = match key & 7 {
                    0 => Val::Varint(varint(data, &mut pos)?),
                    1 => {
                        let b = data.get(pos..pos + 8).ok_or("truncated fixed64")?;
                        pos += 8;
                        Val::Fixed(u64::from_le_bytes(b.try_into().map_err(|_| "fixed64")?))
                    }
                    2 => {
                        let len = varint(data, &mut pos)? as usize;
                        let end = pos.checked_add(len).filter(|e| *e <= data.len());
                        let end = end.ok_or("field longer than the message")?;
                        let b = &data[pos..end];
                        pos = end;
                        Val::Bytes(b)
                    }
                    5 => {
                        let b = data.get(pos..pos + 4).ok_or("truncated fixed32")?;
                        pos += 4;
                        Val::Fixed(u64::from(u32::from_le_bytes(b.try_into().map_err(|_| "fixed32")?)))
                    }
                    other => return Err(format!("unsupported wire type {other}")),
                };
                fields.push((field, val));
            }
            Ok(Proto { fields })
        }

        pub fn bytes(&self, field: u32) -> &'a [u8] {
            self.fields
                .iter()
                .find_map(|(f, v)| match v {
                    Val::Bytes(b) if *f == field => Some(*b),
                    _ => None,
                })
                .unwrap_or(&[])
        }

        pub fn string(&self, field: u32) -> String {
            String::from_utf8_lossy(self.bytes(field)).into_owned()
        }

        pub fn varint(&self, field: u32) -> u64 {
            self.fields
                .iter()
                .find_map(|(f, v)| match v {
                    Val::Varint(n) | Val::Fixed(n) if *f == field => Some(*n),
                    _ => None,
                })
                .unwrap_or(0)
        }

        /// A nested message, empty when the field is missing or is not one.
        pub fn message(&self, field: u32) -> Proto<'a> {
            Proto::parse(self.bytes(field)).unwrap_or_default()
        }

        pub fn messages(&self, field: u32) -> Vec<Proto<'a>> {
            self.fields
                .iter()
                .filter_map(|(f, v)| match v {
                    Val::Bytes(b) if *f == field => Proto::parse(b).ok(),
                    _ => None,
                })
                .collect()
        }
    }

    // ------------------------------------------------ the fetch request

    fn encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    /// `webcast/im/fetch` for `room_id`, from `cursor` on (empty the first time).
    pub fn fetch_url(room_id: &str, cursor: &str, logged_in: bool) -> String {
        let mut params: Vec<(&str, &str)> = vec![
            ("aid", "1988"),
            ("app_language", "en"),
            ("app_name", "tiktok_web"),
            ("browser_language", "en-US"),
            ("browser_name", "Mozilla"),
            ("browser_online", "true"),
            ("browser_platform", "Win32"),
            ("browser_version", "5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"),
            ("cookie_enabled", "true"),
            ("cursor", cursor),
            ("device_platform", "web"),
            ("focus_state", "true"),
            ("from_page", "user"),
            ("history_comment_count", "36"),
            ("is_fullscreen", "false"),
            ("is_page_visible", "true"),
            ("did_rule", "3"),
            ("fetch_rule", "1"),
            ("last_rtt", "0"),
            ("live_id", "12"),
            ("resp_content_type", "protobuf"),
            ("room_id", room_id),
            ("screen_height", "1080"),
            ("screen_width", "1920"),
            ("tz_name", "UTC"),
            ("version_code", "180800"),
            ("webcast_language", "en"),
            ("identity", "audience"),
        ];
        if logged_in {
            params.push(("user_is_login", "true"));
        }
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        format!("https://webcast.tiktok.com/webcast/im/fetch/?{}", query.join("&"))
    }

    pub fn room_url(user: &str) -> String {
        format!(
            "https://www.tiktok.com/api-live/user/room/?aid=1988&sourceType=54&uniqueId={}",
            encode(user)
        )
    }

    /// The `Cookie` header: the `ttwid` and, when pasted, the player's session cookie (a
    /// whole `name=value; ...` string, or a bare `sessionid`).
    pub fn cookie_header(ttwid: &str, session: &str) -> String {
        let mut parts = vec![format!("ttwid={ttwid}")];
        let session = session.trim();
        if !session.is_empty() {
            if session.contains('=') {
                parts.push(session.to_string());
            } else {
                parts.push(format!("sessionid={session}"));
            }
        }
        parts.join("; ")
    }

    /// The `ttwid` of a response's `Set-Cookie` headers.
    pub fn ttwid_from(set_cookies: &[String]) -> Option<String> {
        set_cookies.iter().find_map(|c| {
            let value = c.trim().strip_prefix("ttwid=")?;
            let value = value.split(';').next()?.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
    }

    pub const TTWID_REGISTER_URL: &str = "https://www.tiktok.com/ttwid/union/register/";
    pub const TTWID_REGISTER_BODY: &str = "{\"region\":\"us\",\"aid\":1988,\"needFid\":false,\"service\":\"www.tiktok.com\",\"migrate_info\":{\"ticket\":\"\",\"source\":\"node\"},\"cbUrlProtocol\":\"https\",\"union\":true}";

    /// The live room of the room lookup's JSON (`data.user.roomId` and `status`; 2 is live).
    pub fn parse_room(json: &str) -> Result<String, String> {
        let status = json
            .split_once("\"status\":")
            .map(|(_, rest)| rest.trim_start().chars().take_while(char::is_ascii_digit).collect::<String>())
            .and_then(|digits| digits.parse::<u32>().ok());
        if let Some(status) = status {
            if status != 2 {
                return Err(format!("not live right now (status {status})"));
            }
        }
        let room = json.split_once("\"roomId\":").map(|(_, rest)| {
            rest.trim_start()
                .trim_start_matches('"')
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        });
        match room {
            Some(id) if !id.is_empty() && id != "0" => Ok(id),
            _ => Err("no live room found (user not found or not live)".into()),
        }
    }

    // ------------------------------------------------ what comes back

    /// Something that happened in the live.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Event {
        Chat { id: u64, user: String, text: String },
        Gift { id: u64, user: String, text: String },
        LiveEnded,
    }

    /// One fetch's events, the cursor for the next and how long to wait before it.
    #[derive(Debug, PartialEq)]
    pub struct Fetched {
        pub events: Vec<Event>,
        pub cursor: String,
        pub interval_ms: u64,
    }

    pub fn looks_like_html(body: &[u8]) -> bool {
        let head: String = body.iter().take(64).map(|b| *b as char).collect();
        let head = head.trim().to_ascii_lowercase();
        head.starts_with("<!doctype") || head.starts_with("<html") || head.starts_with('{')
    }

    pub fn is_gzip(data: &[u8]) -> bool {
        data.starts_with(&[0x1F, 0x8B])
    }

    /// Decodes a fetch response (already gunzipped). `cursor` is kept when it has none.
    pub fn parse_fetch(body: &[u8], cursor: &str) -> Result<Fetched, String> {
        let response = Proto::parse(body)?;
        let next = response.string(2);
        let interval = response.varint(3);
        let events = response
            .messages(1)
            .iter()
            .filter_map(parse_message)
            .collect();
        Ok(Fetched {
            events,
            cursor: if next.trim().is_empty() { cursor.to_string() } else { next },
            interval_ms: if interval > 0 { interval.max(1500) } else { 2000 },
        })
    }

    fn first_nonblank(values: &[String]) -> String {
        values
            .iter()
            .find(|v| !v.trim().is_empty())
            .cloned()
            .unwrap_or_default()
    }

    fn user_name(message: &Proto, field: u32) -> String {
        let user = message.message(field);
        first_nonblank(&[user.string(3), user.string(38), "Viewer".to_string()])
    }

    /// The text of a chat message: its content field, or the newer nested ones.
    fn chat_text(chat: &Proto) -> String {
        for field in [3u32, 4, 22, 29, 30, 31] {
            let direct = readable_text(&chat.string(field));
            if !direct.is_empty() {
                return direct;
            }
            let nested = chat.message(field);
            for inner in [1u32, 2, 3] {
                let text = readable_text(&nested.string(inner));
                if !text.is_empty() {
                    return text;
                }
            }
        }
        String::new()
    }

    fn message_id(inner: &Proto, wrapper: u64) -> u64 {
        match inner.message(1).varint(2) {
            0 => wrapper,
            id => id,
        }
    }

    /// One `Message` of the response: its method name, payload and id.
    pub fn parse_message(message: &Proto) -> Option<Event> {
        let method = message.string(1);
        let payload = message.bytes(2);
        let wrapper = message.varint(3);
        let inner = Proto::parse(payload).ok()?;
        if method.ends_with("WebcastChatMessage") {
            let text = chat_text(&inner);
            if text.is_empty() {
                return None;
            }
            Some(Event::Chat {
                id: message_id(&inner, wrapper),
                user: user_name(&inner, 2),
                text,
            })
        } else if method.ends_with("WebcastGiftMessage") {
            let details = inner.message(15);
            // A gift streak sends a message per tap: only the finished one is shown.
            if details.varint(11) == 1 && inner.varint(9) == 0 {
                return None;
            }
            let name = readable_text(&details.string(16));
            let count = inner.varint(5).max(1);
            let what = match (name.is_empty(), count > 1) {
                (true, true) => format!("x{count}"),
                (true, false) => String::new(),
                (false, true) => format!("{name} x{count}"),
                (false, false) => name,
            };
            Some(Event::Gift {
                id: message_id(&inner, wrapper),
                user: user_name(&inner, 7),
                text: if what.is_empty() { "sent a gift".into() } else { format!("sent {what}") },
            })
        } else if method.ends_with("WebcastControlMessage") && inner.varint(2) == 3 {
            Some(Event::LiveEnded)
        } else {
            None
        }
    }
}

/// Text that is text: protobuf read as UTF-8 comes out full of replacement and control
/// characters, which this drops (the original mod's `readableText`).
pub fn readable_text(value: &str) -> String {
    if value.trim().is_empty() {
        return String::new();
    }
    let (mut total, mut printable, mut replacement) = (0usize, 0usize, 0usize);
    for c in value.chars() {
        total += 1;
        if c == '\u{FFFD}' {
            replacement += 1;
        } else if !c.is_control() || matches!(c, '\n' | '\r' | '\t') {
            printable += 1;
        }
    }
    if replacement * 2 >= total || printable * 2 < total {
        return String::new();
    }
    value.trim().to_string()
}

const TIKTOK_MAX_SEEN: usize = 5000;

fn run_tiktok(user: &str, session: &str, epoch: u64) {
    let p = Platform::TikTok;
    while p.link().current(epoch) {
        match read_tiktok(user, session, epoch) {
            Ok(Ended::Stopped) => return,
            Ok(Ended::LiveOver) => {
                set_status(p, epoch, "The live ended");
                return;
            }
            Err(e) => set_status(p, epoch, &format!("{e}, retrying...")),
        }
        if !pause(p, epoch, 10_000) {
            return;
        }
    }
}

enum Ended {
    Stopped,
    LiveOver,
}

fn read_tiktok(user: &str, session: &str, epoch: u64) -> Result<Ended, String> {
    use tiktok::Event;
    let p = Platform::TikTok;
    const REFERER: (&str, &str) = ("Referer", "https://www.tiktok.com/");

    // Room, then the `ttwid` cookie the web client gets first.
    let reply = net::request(None, &tiktok::room_url(user), &[REFERER])?;
    let room = tiktok::parse_room(&String::from_utf8_lossy(&reply.body))?;
    let ttwid = match net::request(None, "https://www.tiktok.com/", &[])
        .ok()
        .and_then(|r| tiktok::ttwid_from(&r.set_cookies))
    {
        Some(t) => t,
        None => {
            let reply = net::request(
                Some(tiktok::TTWID_REGISTER_BODY),
                tiktok::TTWID_REGISTER_URL,
                &[REFERER],
            )?;
            tiktok::ttwid_from(&reply.set_cookies).ok_or("no ttwid cookie from TikTok")?
        }
    };
    let cookie = tiktok::cookie_header(&ttwid, session);
    set_status(p, epoch, &format!("Connected to @{user}"));

    let mut seen: HashSet<u64> = HashSet::new();
    let mut order: VecDeque<u64> = VecDeque::new();
    let mut cursor = String::new();
    let mut first = true;
    loop {
        if !p.link().current(epoch) {
            return Ok(Ended::Stopped);
        }
        let url = tiktok::fetch_url(&room, &cursor, !session.trim().is_empty());
        let reply = net::request(
            None,
            &url,
            &[REFERER, ("Origin", "https://www.tiktok.com"), ("Cookie", &cookie)],
        )?;
        let mut body = reply.body;
        if body.is_empty() || tiktok::looks_like_html(&body) {
            return Err("TikTok answered with a page, not chat".into());
        }
        if tiktok::is_gzip(&body) {
            body = net::gunzip(&body)?;
        }
        let fetched = tiktok::parse_fetch(&body, &cursor)?;
        cursor = fetched.cursor;
        for event in fetched.events {
            let (id, user, text, color) = match event {
                Event::LiveEnded => return Ok(Ended::LiveOver),
                Event::Chat { id, user, text } => (id, user, text, None),
                Event::Gift { id, user, text } => (id, user, text, Some(0xFF_FFD700)),
            };
            if id != 0 {
                if !seen.insert(id) {
                    continue;
                }
                order.push_back(id);
                while order.len() > TIKTOK_MAX_SEEN {
                    if let Some(old) = order.pop_front() {
                        seen.remove(&old);
                    }
                }
            }
            // The first fetch is the chat's recent history: remembered, not shown.
            if !first {
                push_message(p, &user, &text, color.unwrap_or_else(|| default_color(&user)));
            }
        }
        first = false;
        if !pause(p, epoch, fetched.interval_ms) {
            return Ok(Ended::Stopped);
        }
    }
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
pub fn push_message(platform: Platform, user: &str, text: &str, color: u32) {
    let mut s = state();
    if !passes(user, text, &s.banwords, &s.blocked) {
        return;
    }
    add(&mut s, platform, user, text, color);
}

fn add(s: &mut State, platform: Platform, user: &str, text: &str, color: u32) {
    s.seq += 1;
    let seq = s.seq;
    s.messages.push_back(ChatMessage {
        platform,
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
            user: format!("{}{}", m.platform.tag(), m.user),
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
            text: shorten(
                &format!("{}{}", m.platform.tag(), m.user),
                &m.text,
                WORLD_MAX_CHARS,
            ),
            color: (world_alpha(age) << 24) | (m.color & 0x00FF_FFFF),
        });
    }
    out
}

// ---------------------------------------------------------------- commands

/// What a `/streamchat` command did: lines for the chat box and settings to keep.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub lines: Vec<String>,
    pub save: Vec<(&'static str, String)>,
}

pub fn help() -> Vec<String> {
    [
        "Take Your Stream Chat (Twitch, TikTok)",
        "/streamchat channel <name>: the Twitch channel to read",
        "/streamchat tiktok <@name>: the TikTok live to read",
        "/streamchat tiktok session <cookie> | clear: optional TikTok login cookie",
        "/streamchat chat start | stop [twitch|tiktok]: connect or disconnect",
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

fn chat_start(which: &str, out: &mut Outcome) {
    let platforms: Vec<Platform> = match Platform::parse(which) {
        Some(p) => vec![p],
        None if which.is_empty() => Platform::ALL.to_vec(),
        None => {
            out.lines.push("Usage: /streamchat chat start [twitch|tiktok]".into());
            return;
        }
    };
    let mut started = false;
    for p in platforms {
        let name = target(p);
        if connect(p) {
            started = true;
            out.lines.push(format!("Connecting to {} {name}...", p.name()));
        } else if which.is_empty() {
            // Both asked for and this one isn't set up: not an error.
        } else {
            out.lines.push(format!("Set the {} name first (see /streamchat)", p.name()));
        }
    }
    if !started && which.is_empty() {
        out.lines
            .push("Set a channel first: /streamchat channel <name> or /streamchat tiktok <@name>".into());
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
                if is_connected(Platform::Twitch) {
                    connect(Platform::Twitch);
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
        "tiktok" => {
            let (what, value) = rest.split_once(' ').unwrap_or((rest, ""));
            match what.to_ascii_lowercase().as_str() {
                "session" => {
                    let cookie = if value.trim().eq_ignore_ascii_case("clear") {
                        String::new()
                    } else {
                        clean_cookie(value)
                    };
                    state().tiktok_session = cookie.clone();
                    let set = !cookie.is_empty();
                    out.save.push(("tiktok_session", cookie));
                    out.lines.push(if set {
                        "TikTok session saved (used at the next connection)".into()
                    } else {
                        "TikTok session cleared".into()
                    });
                }
                _ => match clean_tiktok_user(rest) {
                    Some(name) => {
                        state().tiktok_user = name.clone();
                        out.save.push(("tiktok_user", name.clone()));
                        if is_connected(Platform::TikTok) {
                            connect(Platform::TikTok);
                            out.lines.push(format!("Switching to @{name}"));
                        } else {
                            out.lines.push(format!(
                                "TikTok set to @{name}. /streamchat chat start tiktok"
                            ));
                        }
                    }
                    None => out
                        .lines
                        .push("Usage: /streamchat tiktok <@name> | session <cookie>".into()),
                },
            }
        }
        "chat" => {
            let (action, which) = rest.split_once(' ').unwrap_or((rest, ""));
            let which = which.trim();
            match action.to_ascii_lowercase().as_str() {
                "start" => chat_start(which, &mut out),
                "stop" => match (which.is_empty(), Platform::parse(which)) {
                    (true, _) => {
                        Platform::ALL.iter().for_each(|p| disconnect(*p));
                        out.lines.push("Chat disconnected".into());
                    }
                    (false, Some(p)) => {
                        disconnect(p);
                        out.lines.push(format!("{} disconnected", p.name()));
                    }
                    (false, None) => out
                        .lines
                        .push("Usage: /streamchat chat stop [twitch|tiktok]".into()),
                },
                _ => out
                    .lines
                    .push("Usage: /streamchat chat start | stop [twitch|tiktok]".into()),
            }
        }
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
            add(&mut s, Platform::Twitch, "Test", rest, 0xFF_FFAA00);
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
            out.lines.push(format!("Mode: {}", s.mode.name()));
            for p in Platform::ALL {
                let name = match p {
                    Platform::Twitch if !s.channel.is_empty() => format!("#{}", s.channel),
                    Platform::TikTok if !s.tiktok_user.is_empty() => format!("@{}", s.tiktok_user),
                    _ => "not set".to_string(),
                };
                let status = &s.status[p.index()];
                out.lines.push(format!(
                    "{}: {name}, {}",
                    p.name(),
                    if status.is_empty() { "not connected" } else { status }
                ));
            }
        }
        _ => out.lines = help(),
    }
    out
}

// ---------------------------------------------------------------- settings page

pub fn connection_label(p: Platform) -> String {
    format!(
        "{}: {}",
        p.name(),
        if is_connected(p) { "ON" } else { "OFF" }
    )
}

pub fn mode_label() -> String {
    format!("Show: {}", mode().name())
}

pub fn names_label() -> String {
    let (c, t) = (channel(), tiktok_user());
    format!(
        "Twitch {}, TikTok {} (/streamchat channel, tiktok)",
        if c.is_empty() { "none".to_string() } else { format!("#{c}") },
        if t.is_empty() { "none".to_string() } else { format!("@{t}") },
    )
}

/// A click on a settings button. Returns the setting to keep, if any.
pub fn click(button: usize) -> Option<(&'static str, String)> {
    match button {
        0 | 1 => {
            let p = Platform::ALL[button];
            if is_connected(p) {
                disconnect(p);
            } else {
                connect(p);
            }
            None
        }
        2 => {
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

    // ---- TikTok

    fn varint_bytes(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }
    fn field_varint(f: u32, v: u64) -> Vec<u8> {
        let mut out = varint_bytes(u64::from(f) << 3);
        out.extend(varint_bytes(v));
        out
    }
    fn field_bytes(f: u32, b: &[u8]) -> Vec<u8> {
        let mut out = varint_bytes(u64::from(f) << 3 | 2);
        out.extend(varint_bytes(b.len() as u64));
        out.extend_from_slice(b);
        out
    }
    /// A `Message` of the response: method, payload and wrapper id.
    fn wrapper(method: &str, payload: &[u8], id: u64) -> Vec<u8> {
        let mut m = field_bytes(1, method.as_bytes());
        m.extend(field_bytes(2, payload));
        m.extend(field_varint(3, id));
        m
    }
    fn chat_payload(text: &str, nick: &str, id: u64) -> Vec<u8> {
        let mut p = field_bytes(1, &field_varint(2, id));
        p.extend(field_bytes(2, &field_bytes(3, nick.as_bytes())));
        p.extend(field_bytes(3, text.as_bytes()));
        p
    }

    #[test]
    fn protobuf_fields_read_back() {
        let mut data = field_varint(1, 300);
        data.extend(field_bytes(2, b"hi"));
        data.extend(field_bytes(3, &field_varint(1, 7)));
        let p = tiktok::Proto::parse(&data).unwrap();
        assert_eq!(p.varint(1), 300);
        assert_eq!(p.string(2), "hi");
        assert_eq!(p.message(3).varint(1), 7);
        assert_eq!(p.varint(9), 0);
        assert!(tiktok::Proto::parse(&[0x0A, 0x05, 1]).is_err());
    }

    #[test]
    fn a_fetch_response_gives_chat_and_gifts() {
        let gift_user = field_bytes(7, &field_bytes(3, b"Fan"));
        let mut gift = field_bytes(15, &field_bytes(16, b"Rose"));
        gift.extend(field_varint(5, 3));
        gift.extend(gift_user);
        let mut body = field_bytes(
            1,
            &wrapper("WebcastChatMessage", &chat_payload("hello tiktok", "Ann", 42), 1),
        );
        body.extend(field_bytes(1, &wrapper("WebcastGiftMessage", &gift, 2)));
        body.extend(field_bytes(1, &wrapper("WebcastMemberMessage", b"", 3)));
        body.extend(field_bytes(2, b"cursor-1"));
        body.extend(field_varint(3, 1000));
        let f = tiktok::parse_fetch(&body, "old").unwrap();
        assert_eq!(f.cursor, "cursor-1");
        assert_eq!(f.interval_ms, 1500);
        assert_eq!(
            f.events,
            vec![
                tiktok::Event::Chat { id: 42, user: "Ann".into(), text: "hello tiktok".into() },
                tiktok::Event::Gift { id: 2, user: "Fan".into(), text: "sent Rose x3".into() },
            ]
        );
    }

    #[test]
    fn the_cursor_is_kept_and_the_live_can_end() {
        let control = field_bytes(1, &wrapper("WebcastControlMessage", &field_varint(2, 3), 9));
        let f = tiktok::parse_fetch(&control, "keep").unwrap();
        assert_eq!(f.cursor, "keep");
        assert_eq!(f.interval_ms, 2000);
        assert_eq!(f.events, vec![tiktok::Event::LiveEnded]);
    }

    #[test]
    fn rooms_come_from_the_lookup_json() {
        let live = r#"{"data":{"user":{"roomId":"7123456789","status":2}}}"#;
        assert_eq!(tiktok::parse_room(live).unwrap(), "7123456789");
        let off = r#"{"data":{"user":{"roomId":"0","status":4}}}"#;
        assert!(tiktok::parse_room(off).unwrap_err().contains("not live"));
        assert!(tiktok::parse_room("{}").is_err());
    }

    #[test]
    fn the_ttwid_and_cookie_header() {
        let cookies = vec!["foo=1".to_string(), "ttwid=abc%7Cdef; Domain=.tiktok.com".to_string()];
        assert_eq!(tiktok::ttwid_from(&cookies), Some("abc%7Cdef".into()));
        assert_eq!(tiktok::cookie_header("t", ""), "ttwid=t");
        assert_eq!(tiktok::cookie_header("t", "tok"), "ttwid=t; sessionid=tok");
        assert_eq!(tiktok::cookie_header("t", "a=1; b=2"), "ttwid=t; a=1; b=2");
    }

    #[test]
    fn the_fetch_url_is_encoded() {
        let url = tiktok::fetch_url("123", "cur sor", false);
        assert!(url.starts_with("https://webcast.tiktok.com/webcast/im/fetch/?aid=1988"));
        assert!(url.contains("room_id=123") && url.contains("cursor=cur%20sor"));
        assert!(!url.contains("user_is_login"));
        assert!(tiktok::fetch_url("1", "", true).contains("user_is_login=true"));
    }

    #[test]
    fn pages_and_garbage_are_told_apart_from_chat() {
        assert!(tiktok::looks_like_html(b"<!DOCTYPE html>"));
        assert!(tiktok::looks_like_html(b"  {\"error\":1}"));
        assert!(!tiktok::looks_like_html(&[0x0A, 0x02, 0x08, 0x01]));
        assert!(tiktok::is_gzip(&[0x1F, 0x8B, 8]));
        assert_eq!(readable_text("  hi \u{1F602} "), "hi \u{1F602}");
        assert_eq!(readable_text("\u{FFFD}\u{FFFD}a"), "");
        assert_eq!(readable_text("\u{1}\u{2}\u{3}"), "");
    }

    #[test]
    fn tiktok_names_are_cleaned() {
        assert_eq!(clean_tiktok_user(" @Some.User_1 "), Some("some.user_1".into()));
        assert_eq!(clean_tiktok_user("bad name"), None);
        assert_eq!(clean_cookie(" a=1\r\n; b=2 "), "a=1; b=2");
    }
}
