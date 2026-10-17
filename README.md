<p align="center">
  <img src="assets/wisp.svg" width="128" height="128" alt="Wisp logo">
</p>

<h1 align="center">Wisp</h1>

<p align="center">
  A voice chat client for TeamSpeak 3 servers, written in Rust.
</p>

<p align="center">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT-8fb3d9?style=flat-square" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/rust-1.88%2B-8fb3d9?style=flat-square&logo=rust&logoColor=white" alt="Rust 1.88+">
  <img src="https://img.shields.io/badge/platform-Linux-8fb3d9?style=flat-square&logo=linux&logoColor=white" alt="Platform: Linux">
  <img src="https://img.shields.io/badge/Hyprland-ready-8fb3d9?style=flat-square&logo=hyprland&logoColor=white" alt="Hyprland ready">
  <img src="https://img.shields.io/badge/UI-GPUI-8fb3d9?style=flat-square" alt="UI: GPUI">
  <img src="https://img.shields.io/badge/protocol-TeamSpeak%203-8fb3d9?style=flat-square&logo=teamspeak&logoColor=white" alt="Protocol: TeamSpeak 3">
  <img src="https://img.shields.io/badge/vibe-coded-d98f8f?style=flat-square" alt="Vibe coded">
</p>

## Why

The official TeamSpeak 3 client and I never got along on Linux. Global hotkeys
basically don't work under Wayland, and on Hyprland push-to-talk only fires
while the TeamSpeak window has focus, which defeats the point.

So I wrote my own client. It talks the TeamSpeak 3 protocol (through
[tsclientlib](https://github.com/ReSpeak/tsclientlib)), so it works with any
normal TS3 server, and push-to-talk goes through Hyprland key bindings, so it
works no matter which window is focused. Along the way it picked up a darker,
calmer look than the original and a few features I wanted for myself.

## Heads up: vibe coded

Most of this was written together with an AI coding assistant, with me steering,
testing and fixing. It runs my daily voice chat and works well for me, but it
hasn't had much review or testing beyond my own setup. Take it with a grain of
salt, expect rough edges, and please open an issue when something breaks.

## What it does

The usual TeamSpeak things:

- Several servers at once, channel tree with drag and drop, chat tabs with
  history, private messages, pokes, offline messages
- Push-to-talk or voice activation, whisper lists, per-user volume
- Channel editor, permissions, server and channel groups, bans, privilege keys,
  complaints, server settings and log, channel files, icons
- Identities, including importing and exporting TeamSpeak 3 ones, avatars

And some extras I found useful:

- Global push-to-talk with a release delay that works on Wayland (Hyprland)
- Noise suppression (RNNoise), echo cancellation and automatic gain
- Evening out loud and quiet voices, so nobody blows out your ears
- Turning other apps down while someone talks
- Recording to Ogg Opus
- Ctrl+K to jump to any server, channel or person
- Friends and blocked people, notifications, sounds and spoken events
- Tray icon, `ts3server://` links, autostart, and a command line for scripts
  and key bindings (`wisp status`, `wisp join`, `wisp ptt down`, see `wisp help`)

## Status

Runs on Linux with PipeWire. Hotkeys use Hyprland key bindings; on other
desktops you can bind `wisp ptt down` / `wisp ptt up` yourself. macOS and
Windows are planned, see [docs/PLAN.md](docs/PLAN.md).

Wisp is an independent project. It is not made by, affiliated with or endorsed
by TeamSpeak Systems GmbH. "TeamSpeak" is their trademark and is only used here
to say which servers Wisp talks to. Like every third-party TeamSpeak client, Wisp
connects with the version information of an official client (through
tsclientlib), because servers expect it.

## Building

Needs Rust 1.88 or newer, CMake and a C compiler (for Opus).

```sh
cargo build --release
./target/release/wisp
```

On first start Wisp adds itself to your launcher and can take over
`ts3server://` links (Settings).

## License

[MIT](LICENSE). Third-party licenses are listed in
[THIRD-PARTY.md](THIRD-PARTY.md).

Recording a conversation can need everyone's consent where you live; Wisp shows
others that you are recording, the rest is up to you.
