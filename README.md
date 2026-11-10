# Echo

Echo is a chat client for Twitch.

It is a native desktop application for Windows and Linux, written in Rust on
top of the [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
UI framework.

Echo is an independent project and is not affiliated with or endorsed by Twitch.

## Features

**Chat and layout**

- Many channels at once in tabs and resizable splits, with drag-to-rearrange,
  collapsible split headers, popout channel windows and reopening of closed
  tabs
- Quick channel switcher (Ctrl+K) with a "Live now" list of followed channels
- A Watching split (Windows) that follows the channel open in your browser,
  through a browser extension
- Replies and reply threads, whispers with a dedicated conversation split, and
  an Activity view of mentions and whispers across channels
- Recent-message history when joining a channel, with a local per-channel
  history cache and optional plain-text chat logs
- Timestamps in custom formats, new-day dividers, and a choice of how deleted
  messages, timeouts and bans are shown
- Event messages for subscriptions, raids, announcements, cheers, channel point
  redemptions, watch streaks and Hype Trains

**Emotes and cosmetics**

- Twitch, 7TV, BetterTTV and FrankerFaceZ emotes, including animated, wide and
  zero-width emotes and high-DPI image variants
- An emote picker with search and provider grouping, and tab completion for
  emotes and usernames
- Live 7TV emote set updates
- 7TV name paints, badges, personal emotes and animated avatars
- Show a chosen emote as text or as a still image, everywhere or in one channel

**Highlights, filters and search**

- Mention and rule-based highlights with sounds, ignore rules, and an
  explanation of why a message was highlighted, with a rule tester
- Expression-based filters for messages, users, channel state and moderation
  events
- Search with `from:`, `has:`, `is:` and `badge:` predicates, negation and regular
  expressions

**Moderation**

- Moderation commands and controls for timeouts, bans, deletions, chat clears,
  shield mode and chat modes
- User cards with recent messages, roles, badges, follow and subscription
  age, pronouns and private notes
- Configurable moderator buttons and a per-split moderation mode
- Chatter list, AutoMod queue and suspicious-user workflows

**Links, uploads and commands**

- Link handling with open, copy and incognito options, and link previews for
  Twitch clips, VODs, channels and 7TV emotes
- An image uploader with provider presets, custom request settings, paste and
  drag-and-drop uploads, and a local upload history
- Custom commands with placeholders, import and export

**Notifications and appearance**

- Desktop notifications and sounds per event, including live notifications for
  channels
- Dark, light and custom themes from JSON theme files, window transparency and
  a reduced-motion option
- A hotkey editor, streamer mode to hide private details while streaming, and
  settings search
- Settings import from an existing Chatterino installation (Settings > Advanced)

**Platform**

- Twitch account login with multiple accounts, plus anonymous read-only mode
- Screen reader support and right-to-left text
- Windows: signed in-app updates and an optional notification-area icon
- Linux: Wayland and X11, with a desktop launcher installed by `make install`

## Building

Echo builds against a small fork of GPUI that is reconstructed from a pinned Zed
revision plus the patches in [`patches/`](patches/). The setup script clones Zed
into `../zed-gpui-fork`, next to your Echo checkout, applies the patch series and
verifies the result. Nothing compiles until it has run. See
[docs/GPUI_FORK.md](docs/GPUI_FORK.md) for details.

The Rust toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it
automatically on the first `cargo` command. You also need Git.

Builds print a warning that the Echo API proxy is disabled. That is expected:
the proxy is an optional, private build-time setting (see
[docs/ENVIRONMENT.md](docs/ENVIRONMENT.md)).

### Windows

Requirements: the MSVC build tools and Windows SDK (Visual Studio Build Tools
with the "Desktop development with C++" workload) and PowerShell 7.

```powershell
pwsh -NoProfile -File scripts/setup-gpui-fork.ps1
cargo run --release
```

For an optimized distribution build:

```powershell
cargo build --profile dist --locked --bin echo
```

The binary is written to `target\dist\echo.exe`.

### Linux

Install the native build dependencies; [docs/LINUX_DEVELOPMENT.md](docs/LINUX_DEVELOPMENT.md)
lists the packages for Arch and Debian/Ubuntu, and `make linux-deps` checks
that they are present. Then:

```sh
./scripts/setup-gpui-fork.sh
cargo run --release
```

To build the distribution profile and install Echo for the current user (binary
under `~/.local/share/echo`, an `echo-gpui` command in `~/.local/bin`, and a
desktop launcher):

```sh
make install
```

`make uninstall` removes it again.

## Contributing

Read [AGENTS.md](AGENTS.md) for the contribution workflow (small slices,
proportional validation, commit-message convention) and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for an overview of the code. Linux
is the authoritative test platform: `./scripts/test-linux.sh quick` runs the
main test suite. On Windows, use `cargo check -p echo_gpui --bins` and
`cargo clippy --workspace --bins`, and don't run `cargo test` for the root
crate.

## License

Echo is licensed under the [MIT License](LICENSE).
