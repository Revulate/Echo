# Echo on the Linux desktop

How to install Echo as a desktop application on Linux and how it behaves there,
with notes for KDE Plasma (the CachyOS default). Building, testing and the
verification harness are in [LINUX_DEVELOPMENT.md](LINUX_DEVELOPMENT.md).

Echo runs on Plasma 6 under Wayland and under X11.

## Install

1. Install the build dependencies (`./scripts/check-linux-build-deps.sh` prints
   the packages for Arch/CachyOS and Debian/Ubuntu), `rustup` (or another way
   to get the pinned toolchain), a Vulkan driver (`vulkan-radeon`,
   `vulkan-intel`, or `nvidia-utils`), `pipewire-alsa` (Echo plays sounds
   through ALSA), and fonts (`noto-fonts` for text, `ttf-dejavu` for wide symbol
   and braille coverage in ASCII art).
2. Clone Echo next to an empty `../zed-gpui-fork` path and run
   `./scripts/setup-gpui-fork.sh`. It fetches Zed at the pinned revision and
   applies `patches/`; Linux cannot reuse a Windows fork checkout.
3. Run `make install`. It builds the `dist` profile and installs
   `~/.local/share/echo/bin/echo`, a `~/.local/bin/echo-gpui` link, the
   `com.echo.chat.desktop` launcher entry, and its icon. Never put the binary on
   `PATH` as `echo`: that name shadows coreutils' `echo`. Rerun `make install`
   after `git pull` to update; Linux has no automatic updater. `make uninstall`
   removes the files again and leaves settings and caches in place.

## Desktop behavior

Every Echo window uses the app id `com.echo.chat` (the Wayland `app_id` and the
X11 `WM_CLASS`), so Plasma groups popouts under one task-manager entry and takes
the icon and name from the launcher entry.

- KWin draws the title bar (server-side decorations), so Echo hides its own
  minimize, maximize, and close buttons. Dragging or double-clicking Echo's top
  bar still moves or maximizes the window. A compositor that refuses
  server-side decorations, such as GNOME's, gets Echo's buttons instead, but
  Echo does not draw resize borders there.
- Always on top, and the task-manager attention flag for mentions, go through
  KWin's scripting D-Bus API (`org.kde.KWin /Scripting`), because neither
  Wayland nor GPUI can request them. Echo writes a short script to its runtime
  directory that matches the main window by process id, app id, and title.
  Outside KWin both are unavailable.
- Echo runs one instance per profile. The lock and the activation socket live
  in `$XDG_RUNTIME_DIR/echo/`. A second launch forwards its
  `XDG_ACTIVATION_TOKEN`, so KWin raises the running window instead of only
  flagging it. Launching again while Echo is still shutting down waits up to
  10 seconds for it to exit, then starts normally. X11 sessions forward no
  startup id, so KWin may only flag the window there.
- Closing the main window quits Echo even while popouts are open. SIGTERM,
  SIGINT, and SIGHUP (session logout, `kill`, a closed terminal) quit through
  the same settings, layout, and history shutdown path; a second signal exits at
  once.
- Desktop notifications appear only while no Echo window is active. They are
  filed under the `com.echo.chat` launcher entry, so System Settings >
  Notifications > Applications can configure them, and their text is escaped
  because Plasma renders notification bodies as markup.
- Text uses the General and Fixed width fonts from System Settings >
  Text & Fonts (read from `~/.config/kdeglobals`), falling back to Noto Sans and
  a common monospace family. GPUI's own default, IBM Plex Sans, is not bundled
  with Echo.
- Copy and paste go through GPUI's native Wayland (or X11) clipboard rather
  than XWayland, because KWin lets X11 clients read the clipboard only while an
  X11 window is focused. Clipboard file lists are not read on Linux; drag files
  onto the composer instead.
- Tokens stay in `~/.config/echo/tokens.json` with mode `0600`; KWallet and the
  Secret Service are not used yet.
- Not available on Linux: the tray icon (minimize or close to tray), the browser
  extension's Watching split, and a click-through stream overlay.

## Settings, data and logs

Settings live in `~/.config/echo/`, local data and logs in `~/.local/share/echo/`,
and caches in `~/.cache/echo/`. Windows keeps them under `%APPDATA%\echo` and
`%LOCALAPPDATA%\echo`. To carry settings over from a Windows install, copy
`settings.json` and `sounds.json` from the Windows profile into
`~/.config/echo/` while Echo is closed. Sign in again on Linux, because Windows
tokens are sealed to the Windows user. Custom sound files with Windows paths
need re-selecting.
