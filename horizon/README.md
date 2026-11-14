# Take Your Stream Chat for Horizon (native mod)

A port of Take Your Stream Chat to [Horizon](https://github.com/horizon-rust/horizon), the Rust
reimplementation of Minecraft. Horizon has no Java, so the Fabric mod can't run there; this is a
[horizon-mixins](https://github.com/horizon-rust/horizon-mixins) native mod (`.hnmod`) written in Rust
against Horizon's native hook points.

**Status: not yet built or run in the game.** `runtime.rs` has 18 unit tests that pass (`cargo test` in this
folder; they cover the Twitch and TikTok parsing, filters and layout, not the network). The hook files and
`tiktok_net.rs` were written from Horizon's source and docs and have not been compiled against it. Expect to fix
signature mismatches on the first `native-build`.

## What works

- **Twitch chat**, read anonymously (no login, no token), and **TikTok live chat** (new in 0.2.0), as a HUD box
  in the top-right corner and/or as text floating in the world in front of where you were looking, rising and
  fading out. TikTok messages carry a `[TT]` tag; TikTok gifts show as gold "sent Rose x3" lines.
- Twitch name colours (a stable fallback colour otherwise), `/me` actions.
- Banned words and blocked users.
- Commands, also under the original alias `/minestream`:
  - `/streamchat channel <name>` (Twitch)
  - `/streamchat tiktok <@name>` (TikTok live), `/streamchat tiktok session <cookie>` | `clear` (optional)
  - `/streamchat chat start` | `chat stop`, each with an optional `twitch` or `tiktok`
  - `/streamchat mode hud|world|both`
  - `/streamchat test <text>`
  - `/streamchat banword <word>` | `blockuser <name>` (run again to remove)
  - `/streamchat clear` | `status`
- A settings page under Options > Mods (chat on/off, display mode, channel).
- Settings are kept in the mod's settings folder.

## What is not ported (yet)

- **YouTube and Kick.** Only Twitch and TikTok are implemented.
- **TikTok follows, subscriptions, shares and sticker/emote messages.** Only chat, gifts and the end of the live.
- **TikTok's WebSocket.** Horizon has no WebSocket client, so TikTok is read by polling the web client's
  `webcast/im/fetch` endpoint every 1.5 to 2 seconds, the request the original mod uses to bootstrap its socket
  and as its fallback. The chat's recent history (the first fetch) is remembered but not shown.
- **Emotes, badges, platform borders and the message textures.** Horizon's world primitives draw text only.
- Pinning, chat history and replay screens, the in-game editors, the regex and role filters, Mod Menu.
- Twitch is read over plain IRC (port 6667, no TLS) because native mods here have only `std`. Nothing
  private is sent: the connection is anonymous and read-only.

## Build the .hnmod

The `horizon-hnmod` workflow (`.github/workflows/horizon-hnmod.yml`) builds `take_your_stream_chat-<version>.hnmod`
on every push or pull request that touches `horizon/`, and on request from the Actions tab (Run workflow). It runs
the unit tests, packages the archive, checks it with `cargo horizon-mod check` (the registry's own checks) and
uploads it as the `take_your_stream_chat-hnmod` artifact of the run. Publish a GitHub release and the file is
attached to it.

To build it yourself (Python 3.12+, nothing else): `python3 horizon/tools/package.py` writes
`horizon/target/take_your_stream_chat-<version>.hnmod`. The package is source only; Horizon's launcher compiles it
on the pinned Horizon revision. Raise `version` in `horizon-mixin.toml` for a new release.

## Install

Build it into the game with the launcher (see Horizon's
[native mods](https://github.com/horizon-rust/horizon/blob/main/docs/native-mods.md)), or start from
[horizon-native-mod-template](https://github.com/horizon-rust/horizon-native-mod-template) and copy
`mixins/take_your_stream_chat/` into it. It needs `"hooks": 427` (`native_world_primitives`).

## Files

| File | What it is |
|---|---|
| `mixins/take_your_stream_chat/horizon-mixin.toml` | The bundle: two modules and six HEAD hooks |
| `mixins/take_your_stream_chat/runtime.rs` | Twitch IRC, TikTok protocol, filters, layout, commands; also the Cargo library, for tests |
| `mixins/take_your_stream_chat/tiktok_net.rs` | HTTPS and gzip for TikTok, with `ureq` and `flate2` from the game's own dependencies |
| `mixins/take_your_stream_chat/{hud,world,command,list,settings,click}.rs` | One helper per hook |

Test the logic with `cargo test` in this folder (`cargo build` alone doesn't work: `runtime.rs` reaches the game's
`tiktok_net` module, which the tests replace with a stub).

## TikTok notes

- The streamer must be live; `/streamchat tiktok <@name>` then `/streamchat chat start tiktok`.
- TikTok's web endpoints are unofficial and can change or turn away anonymous clients. If the status line
  (`/streamchat status`) says TikTok answered with a page or an error, a logged-in session often helps, as in the
  original mod: paste your `sessionid` (or the whole `Cookie` header) with `/streamchat tiktok session <cookie>`.
  It is kept as plain text in the mod's settings folder, so use it only on your own computer, and run
  `/streamchat tiktok session clear` to remove it.
- This is not an official TikTok API and is unaffiliated with TikTok.
