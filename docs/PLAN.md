# Wisp plan

A voice chat client for TeamSpeak 3 servers: Rust, tsclientlib, GPUI via gpui-kit 0.7.1.

## Architecture
- `net/`: one thread, current-thread tokio + LocalSet. The manager owns all sessions
  (one per connected bookmark), routes UI/CLI commands, publishes `Snapshot`s.
  `session.rs` turns `AdminAction`s into TS commands and reports results to the
  activity feed.
- `audio/`: capture and playback streams stay open; a gate decides when mic audio
  is sent (PTT or voice activation). Playback mixes every server, per-user volume.
- `config.rs`: settings + bookmarks in ~/.config/wisp/config.json.
- `ctl.rs`: control socket $XDG_RUNTIME_DIR/wisp-ctl.sock (line in, JSON out) and
  the CLI. PTT also on wisp.sock (datagram).
- `tray.rs`: StatusNotifierItem via ksni; closing the window keeps the app in the tray.
- `hotkeys.rs`: global hotkeys as generated Hyprland binds (~/.config/hypr/wisp.lua).
- `ui/`: main window (servers, channel tree, user panel / activity), channel
  editor overlay, settings window.

Protocol coverage: tsclientlib already has every client command we need
(`c2s_messages.rs` in the ts-bookkeeping build output): permissions, groups,
bans, tokens, offline messages, complaints, file transfer (`download_file` /
`upload_file`) and whisper packets (`C2SWhisper` / `C2SWhisperNew`). Most work
below is UI plus wiring, not protocol.

Conventions for new features:
- One `AdminAction` (or `Cmd`) per user action, answer via `send_with_result`,
  outcome line in the activity feed (`failed()` maps errors to plain words).
- Data the UI needs goes into `ServerSnap`; data fetched on demand (lists that
  can be long: bans, files, permissions) goes through a request/response `Cmd`
  with a oneshot, like `Cmd::Status`, so it doesn't bloat every snapshot.
- Every admin feature also gets a CLI form when it's useful from scripts.

## Done

- [x] Connect / disconnect, multiple servers at once, reconnect after short drops
- [x] Bookmarks (nickname, server password, default channel, auto-connect), persistent identity
- [x] Ordered channel tree: sub-channels, spacers, password / default markers, user counts
- [x] User badges (server + channel group icons via file transfer, cached)
- [x] Talking / muted / away / commander / priority / recording indicators
- [x] Details panel: ping, loss, connected / idle, version, platform, country, roles
- [x] Per-user volume and mute for me (by unique id)
- [x] Poke, message, kick (channel / server), ban (presets), move, server groups,
      channel group, talk power
- [x] Channel create / edit dialog (all common properties), quick rename / topic /
      password, delete
- [x] Drag and drop: reorder and nest channels, move users, drag yourself to join
- [x] Channel chat; server, channel and private messages in the activity feed
- [x] Push-to-talk (global, release delay), voice activation with meter
- [x] Mic / speaker selection matching PipeWire, volumes, self mute / sound off
- [x] Tray with state colours, close to tray, CLI (`status`, `connect`, `join`,
      `say`, `mute`, `deafen`, `ptt`, `channel ...`), hotkeys tab

## Milestone 1: everyday use

Things that come up in a normal evening on a server.

### Passwords and joining
- [x] Prompt for the channel password when joining a locked channel
  - `client_move` with `cpw`; on `ChannelInvalidPassword` show the prompt and retry
  - remember passwords per bookmark + channel id (opt-in "remember")
- [x] Prompt for the server password instead of failing the connect
  - catch the `ServerInvalidPassword` connect error, prompt, reconnect, offer to save
- [x] Join a channel from a bookmark's default channel path (`Lobby/Sub`) reliably

### Tree
- [x] Collapse / expand channels, remembered per server (config: set of collapsed ids)
- [x] Spacer layouts: `[cspacer]` centred, `[rspacer]` right aligned, `[*spacer]` repeated
      character filling the row, plain `[spacer]` left
- [x] Channel info in the right panel when a channel is clicked: topic, rendered
      description (request via `channelgetdescription`), codec + quality, limits,
      needed talk power, who's in it
- [ ] Remember channel passwords only after a successful join (now saved on entry,
      dropped when the server says it's wrong)
- [x] Channel and client icons (`channel_icon_id`, `client_icon_id`): same download
      cache as group icons
- [x] Filter box above the tree (matches channel and user names)

### Chat
- [x] Chat tabs in the right panel: Activity, Channel, Server, one tab per private
      conversation (opened by incoming message or "Send message")
- [x] BBCode to styled text: b, i, u, s, color, url (clickable), size; strip the rest;
      images only as links
- [x] Unread badges on tabs and on servers in the sidebar; mention of own name highlighted
- [x] History kept per server in ~/.local/share/wisp/chat/<server>/ (JSONL), last 500 loaded
- [x] `clientchatclosed` when a private tab is closed (sent as a raw command)

### Notifications and sounds
- [x] Desktop notifications (notify-send / org.freedesktop.Notifications over D-Bus):
      poke, private message, kicked, banned, moved by someone, disconnected
- [x] Notification sounds with a small built-in pack: user joined / left your channel,
      poke, private message, mic muted / unmuted, talk power granted
- [x] Settings → Notifications tab: per event toggles for sound and popup, volume
- [x] Don't notify while Wisp is focused on that chat

### Self
- [x] Away status with message (`client_away`, `client_away_message`), from the self
      footer and the tray; auto-away after idle (optional)
- [x] Own description (`clientedit client_description` needs permission, else hide)
- [x] Request talk power with a message (`client_talk_request`, `client_talk_request_msg`);
      show pending requests to admins with grant / deny
- [x] Toggle channel commander for yourself (`client_is_channel_commander`)

## Milestone 2: voice

- [x] Whisper lists
  - config: named lists of targets (channels, server groups, channel groups, clients by uid)
  - send with `C2SWhisperNew` (group targets) or `C2SWhisper` (explicit channel / client ids)
  - own hotkey per list (hotkeys tab), "whispering" state in the talk bar and tray
  - show incoming whispers: highlight the speaker, optional sound
- [x] Noise suppression: RNNoise (nnnoiseless crate) on the capture path, toggle in Audio;
      skipped when the source already is an RNNoise filter
- [x] Echo cancellation: PipeWire's echo-cancel module (WebRTC), loaded / unloaded by a toggle
- [x] Automatic gain for incoming voices (per talker RMS normaliser, off by default)
- [x] Ducking: lower other apps while someone talks (PipeWire volume of other sink inputs)
- [x] Per-server voice mode override (bookmark setting)
- [x] Record channel audio to a file (mix of playback per server, Opus/OGG); sets
      `client_is_recording` so others see it
- [x] Text-to-speech for events (espeak-ng), optional

## Milestone 3: administration

All of these are server-side permission checked; hide or grey out what the server
refuses (`permoverview` / permission hints from `channel.permission_hints`).

### Permissions
- [x] Permission names: bundled table from a 3.13 server's ServerQuery `permissionlist`
      (the client protocol only sends group boundaries)
- [x] Generic permission editor component: searchable list, value / skip / negate,
      grant values, grouped by category like the official client
- [x] Channel permissions (`channelpermlist`, `channeladdperm`, `channeldelperm`)
- [x] Channel client permissions (`channelclientpermlist` / add / del)
- [x] Client permissions (`clientpermlist` / add / del)
- [x] Permission overview for a client ("why can / can't they"): `permoverview`

### Groups
- [x] Server group editor: create, copy, rename, delete, icon, sort id, permissions,
      member list (`servergroupclientlist`) with add / remove by uid
- [x] Channel group editor: same set for channel groups
      (`channelgroupadd/copy/rename/del/permlist/clientlist`)
- [x] Group icon picker: server icon list (`ftgetfilelist` on `/icons`), upload new

### Bans, complaints, tokens
- [x] Ban list window (`banlist`): search, remove, add by IP / uid / name regex with duration
- [x] Complaints (`complainlist`, `complainadd`, `complaindel`) on the user menu and a list view
- [x] Privilege keys: use a key (`privilegekeyuse` / `tokenuse`), list / create / delete
      (`privilegekeylist`, `privilegekeyadd`), copy to clipboard
- [x] Temporary server passwords (`servertemppasswordadd/list/del`)

### Server
- [x] Virtual server edit: name, phonetic name, welcome message, host message + mode,
      host banner / button, max clients, reserved slots, default groups, codec
      encryption mode, needed security level, temp channel delete delay
- [x] Server connection info (on the Server tab): uptime, bandwidth, loss
- [x] Server log viewer (`logview`), filter by level
- [x] Host message on connect (modal / log / modal-and-quit)
- [x] Host banner and host button (downloaded with reqwest, shown from the cache)

### Channels (rest)
- [x] Channel icon picker in the editor
- [x] Codec latency factor and "unencrypted voice" in the editor
- [ ] Talk power on create in one command (now a follow-up edit after creation)
- [x] Confirm before dragging a channel that has people in it (or an undo toast)
- [x] Subscribe / unsubscribe single channels (`channelsubscribe`), subscription mode setting

## Milestone 4: files and identity

- [x] Channel file browser (`ftgetfilelist`, `ftinitdownload`, `ftinitupload`,
      `ftdeletefile`, `ftcreatedir`, `ftrenamefile`): list, download to ~/Downloads,
      upload with progress, delete, folders; channel password aware
- [x] Avatars: download (`/avatar_<base64 uid>`), show in tree and details; upload own
      (resize to 300x300, `client_flag_avatar` = md5)
- [x] Identities: several named identities, pick per bookmark
- [x] Import a TS3 identity (`.ini` export / `settings.db`), export ours
- [x] Raise identity security level in the background (show progress)
- [x] Phonetic nickname per bookmark (set right after connecting)
- [x] Offline messages (`messagelist`, `messageget`, `messageadd`, `messagedel`) as an inbox

## Milestone 5: desktop and polish

- [x] ts3server:// links: .desktop file with MimeType `x-scheme-handler/ts3server`,
      `wisp connect ts3server://host?port=&nickname=&password=&channel=`
- [x] .desktop entry and app icon (hicolor), autostart with `--hidden`
- [ ] Omarchy bar widget using `wisp status --bar` (skipped for now: the tray icon shows the state)
- [x] Remember the window size (position is up to the compositor on Wayland)
- [x] Keyboard: Esc closes dialogs and panels, Ctrl+K quick switch (servers, channels, users)
- [x] Arrow keys in the tree: Enter joins / opens details, Left / Right collapse
- [ ] Import TS3 bookmarks (3.6 keeps them as protobuf in settings.db's ProtobufItems; needs a decoder)
- [x] Per-server settings: voice mode, nickname, phonetic name, identity, notifications on / off
- [x] Plain-word errors for every TS error code we show (extend `failed()` table)
- [x] Contacts: mark friend / blocked by uid, auto-mute blocked, friend joins notify
- [x] Hide the bookmark's "Connect" while connecting, retry with backoff on failure

## Testing notes

- Admin features are tested on a real server with temporary channels named
  `wisp-test`; they vanish once empty, so nothing is left behind.
- `wisp status --json` shows everything the UI sees, including the activity feed,
  which is the easiest way to check what a command did.

## Milestone 6: streamline

Make the code easier to change before it has to run on three systems.

- [ ] Split the big files (each is 1.5k+ lines): `net/session.rs` (event loop, snapshot,
      commands, chat, transfers), `ui/main_view.rs` (sidebar, tree, panels), `ui/settings.rs`
      (one file per tab), `ui/admin.rs` (one file per tab)
- [ ] One widget set: icon buttons, rows, error boxes and section layouts are repeated in
      several files; move them into `ui/widgets.rs`
- [ ] Group `Settings` into parts (audio, notifications, window, contacts...) with
      versioned migration, instead of one flat struct
- [ ] Play notification sounds through our own output stream instead of `pw-play`
      (fewer moving parts, works everywhere, follows the output device and volume)
- [ ] Desktop notifications with `notify-rust` instead of calling `notify-send`
- [ ] Context menus: move the moderation items into an "Admin" submenu so the
      everyday items stay short
- [ ] First start: no servers yet -> straight into "Add server"
- [ ] CI: fmt, clippy, tests on every push

## Milestone 7: macOS and Windows

GPUI, tsclientlib, cpal, Opus and RNNoise already run on all three. What needs work:

| Area | Linux now | macOS | Windows |
|---|---|---|---|
| Audio devices | PipeWire names via `pactl`, streams moved with `pactl` | cpal devices (CoreAudio) | cpal devices (WASAPI) |
| Echo cancel, RNNoise source check, ducking | PipeWire modules / `pactl` | own echo canceller (webrtc-audio-processing) or hide | same; ducking via Windows' own communications ducking |
| Push-to-talk / hotkeys | Hyprland binds (`wisp.lua`) | CGEventTap (needs Input Monitoring permission), mouse buttons too | low-level keyboard + mouse hooks (`WH_KEYBOARD_LL`, `WH_MOUSE_LL`) |
| Tray | ksni (StatusNotifierItem) | `tray-icon` (menu bar) | `tray-icon` |
| CLI and PTT socket | Unix sockets | Unix sockets | named pipes (`interprocess` crate) |
| Notifications | `notify-send` | `notify-rust` | `notify-rust` |
| Read aloud | espeak-ng | AVSpeechSynthesizer (`tts` crate) | SAPI (`tts` crate) |
| ts3server:// links | `.desktop` + `xdg-mime` | `CFBundleURLTypes` in the app's Info.plist | registry (`HKCU\Software\Classes\ts3server`) |
| Start at login | XDG autostart | login item (LaunchAgent) | `HKCU\...\Run` |
| Paths | XDG | `~/Library/Application Support/Wisp` (dirs crate) | `%APPDATA%\Wisp` |
| Packaging | binary / AUR | `.app` in a `.dmg`; signing + notarization need an Apple developer account | installer (`.msi`); unsigned shows a SmartScreen warning |

Order:
- [x] Per-OS code behind `cfg` (audio/pipewire.rs vs audio/devices.rs, tray.rs vs
      tray_native.rs, hotkeys/native.rs, named pipe on Windows); Linux keeps what it has
- [x] Windows compiles (`cargo xwin check`, see below)
- [x] Bundled UI font (Adwaita Sans), app icons (.ico in the exe, .icns in the .app)
- [x] Packages through cargo-packager (like Telescope): AppImage + .deb, NSIS installer
      (per user), .app in a .dmg per architecture. Pushing to `release` builds a draft
      release; publishing it makes it live
- [x] Self-update (src/update.rs, from Telescope): signed latest.json, one package per
      platform, swaps the exe / .app / AppImage and restarts. Key: 1Password "Wisp update
      signing key", GitHub secret WISP_UPDATE_SIGNING_KEY
- [ ] Run it on Windows: tray, hotkeys (press and release), sounds, popups, links, autostart
- [ ] Run it on macOS: same, plus the microphone permission prompt and links from the browser
- [ ] Device changes without a restart on Windows/macOS (reopen the cpal streams)
- [ ] Echo cancellation off Linux (webrtc-audio-processing) and ducking on Windows
- [ ] Read aloud off Linux (`tts` crate)
- [ ] Mouse buttons as hotkeys off Linux (needs low-level hooks / an event tap)
- [ ] Signing: Windows code signing, Apple notarization (needs a developer account)

Windows check from Linux (Opus is skipped, nothing is linked):

```sh
OPUS_LIB_DIR=/tmp/empty OPUS_STATIC=1 XWIN_ACCEPT_LICENSE=1 cargo xwin check --target x86_64-pc-windows-msvc
```
