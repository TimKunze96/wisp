# Wisp

A voice chat client for TeamSpeak 3 servers, written in Rust with
[GPUI](https://www.gpui.rs) and [tsclientlib](https://github.com/ReSpeak/tsclientlib).

Wisp is an independent project. It is not made by, affiliated with or endorsed
by TeamSpeak Systems GmbH. "TeamSpeak" is their trademark and is only used here
to say which servers Wisp talks to.

## What it does

- Several servers at once, channel tree with drag and drop, chat tabs with
  history, private messages, pokes, offline messages
- Push-to-talk (global, with release delay) or voice activation, whisper lists,
  noise suppression, echo cancellation, per-user volume, evening out loud and
  quiet voices, turning other apps down while someone talks, recording
- Channel editor, permissions, server and channel groups, bans, privilege keys,
  complaints, server settings and log, channel files
- Identities (import and export TeamSpeak 3 identities), avatars, friends and
  blocked people, notifications and sounds
- Tray icon, `ts3server://` links, and a command line for scripts
  (`wisp status`, `wisp join`, `wisp ptt down`, ... see `wisp help`)

## Status

Runs on Linux. Push-to-talk and hotkeys use Hyprland key bindings; the audio
extras (device routing, echo cancellation, ducking) use PipeWire. macOS and
Windows are planned, see [docs/PLAN.md](docs/PLAN.md).

Like every third-party TeamSpeak client, Wisp connects with the version
information of an official client (through tsclientlib), because servers expect
it.

## Building

Needs Rust 1.88 or newer, CMake and a C compiler (for Opus).

```sh
cargo build --release
./target/release/wisp
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT) at your option. Third-party licenses are listed in
[THIRD-PARTY.md](THIRD-PARTY.md).

Recording a conversation can need everyone's consent where you live; Wisp shows
others that you are recording, the rest is up to you.
