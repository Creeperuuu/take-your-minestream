# Take Your Stream Chat for Horizon (native mod)

A port of Take Your Stream Chat to [Horizon](https://github.com/horizon-rust/horizon), the Rust
reimplementation of Minecraft. Horizon has no Java, so the Fabric mod can't run there; this is a
[horizon-mixins](https://github.com/horizon-rust/horizon-mixins) native mod (`.hnmod`) written in Rust
against Horizon's native hook points.

**Status: first port, not yet built or run in the game.** `runtime.rs` has unit tests that pass
(`rustc --test`); the hook files that call into the game were written from Horizon's source and docs and have
not been compiled against it. Expect to fix signature mismatches on the first `native-build`.

## What works

- **Twitch chat**, read anonymously (no login, no token), as a HUD box in the top-right corner and/or as
  text floating in the world in front of where you were looking, rising and fading out.
- Platform-less user colours (Twitch name colours, a stable fallback colour otherwise), `/me` actions.
- Banned words and blocked users.
- Commands, also under the original alias `/minestream`:
  - `/streamchat channel <name>`
  - `/streamchat chat start` | `chat stop`
  - `/streamchat mode hud|world|both`
  - `/streamchat test <text>`
  - `/streamchat banword <word>` | `blockuser <name>` (run again to remove)
  - `/streamchat clear` | `status`
- A settings page under Options > Mods (chat on/off, display mode, channel).
- Settings are kept in the mod's settings folder.

## What is not ported (yet)

- **YouTube, Kick and TikTok.** Only Twitch is implemented.
- **Emotes, badges, platform borders and the message textures.** Horizon's world primitives draw text only.
- Pinning, chat history and replay screens, the in-game editors, the regex and role filters, Mod Menu.
- Twitch is read over plain IRC (port 6667, no TLS) because native mods here have only `std`. Nothing
  private is sent: the connection is anonymous and read-only.

## Install

Build it into the game with the launcher (see Horizon's
[native mods](https://github.com/horizon-rust/horizon/blob/main/docs/native-mods.md)), or start from
[horizon-native-mod-template](https://github.com/horizon-rust/horizon-native-mod-template) and copy
`mixins/take_your_stream_chat/` into it. It needs `"hooks": 427` (`native_world_primitives`).

## Files

| File | What it is |
|---|---|
| `mixins/take_your_stream_chat/horizon-mixin.toml` | The bundle: one module and six HEAD hooks |
| `mixins/take_your_stream_chat/runtime.rs` | IRC client, filters, layout, commands; also the Cargo library, for tests |
| `mixins/take_your_stream_chat/{hud,world,command,list,settings,click}.rs` | One helper per hook |

Test the logic with `cargo test` in this folder.
