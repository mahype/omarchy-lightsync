# LightSync

LightSync is a Linux-native screen synchronization service for Philips Hue
Entertainment areas. Capture, color sampling, Hue streaming, and control stay
in the user's session. The repository also provides an independent Omarchy
Shell integration.

LightSync does not bypass DRM or HDCP. Protected video may be black in a normal
Wayland screen-capture stream and therefore cannot drive the lights.

## Current Features

- Video and Game screen-sampling modes. Game uses a smaller sample area, higher
  exposure, and faster smoothing than Video.
- Four intensity presets, brightness control, per-light spatial sampling, and
  30 FPS Hue Entertainment streaming.
- Wayland ScreenCast portal capture with explicit consent and a saved portal
  restore token, plus an optional `grim` compatibility backend.
- Hue bridge discovery and pairing, Entertainment area selection, and Secret
  Service credential storage.
- Named profiles containing mode, intensity, brightness, display, and area.
- Optional restoration of the previous light state after an orderly stop.
- English and German GUI text, plus system-language selection.
- CLI, GTK/libadwaita GUI, systemd user service, and Omarchy status widget.

Music mode, Scene mode, audio reactivity, coordinated multi-monitor capture,
HDR-aware color processing, and broader Hue Sync feature parity are roadmap
items. The daemon rejects Music, Scene, and audio-reactive configurations rather
than silently pretending to support them. The portal chooser can select one
screen for a session; it cannot target a configured display ID.

## Components

- `lightsyncd` owns configuration, capture, color processing, Hue credentials,
  and streaming. It exposes a user-only Unix socket.
- `lightsync` controls the daemon. `lightsync status --watch --json` is the
  newline-delimited status contract used by desktop integrations.
- `lightsync-gui` provides setup, synchronization controls, profiles, settings,
  and diagnostics.
- `integrations/omarchy` provides one shared status-stream service and a bar
  widget. It does not poll once per monitor.

Configuration is in `$XDG_CONFIG_HOME/omarchy-lightsync` (normally
`~/.config/omarchy-lightsync`), runtime IPC is below
`$XDG_RUNTIME_DIR/omarchy-lightsync`, and Hue secrets are stored through the
session's Secret Service. These paths are separate from earlier LightSync
prototypes.

## Dependencies

Runtime dependencies are GTK 4, libadwaita, PipeWire, OpenSSL,
`xdg-desktop-portal`, a desktop-compatible portal backend, and a Secret Service
provider such as GNOME Keyring or KeePassXC. The optional `grim` backend also
requires `grim`. A Philips Hue Bridge and a configured Hue Entertainment area
are required for light output.

Building requires Rust 1.85 or newer, Cargo, Clang, `pkg-config`, and development
headers for GTK 4, libadwaita, PipeWire, and OpenSSL.

## Install

### Per-user source install

Install the dependencies, review the script, and run:

```sh
./install.sh
systemctl --user enable --now lightsync.service
```

The script builds with the committed `Cargo.lock` and installs only to
`~/.local`. It refuses a different `PREFIX`, does not enable or start the
service, and does not alter LightSync configuration.

To update an existing source install:

```sh
git pull --ff-only
./install.sh
systemctl --user restart lightsync.service
```

### Arch release template

`packaging/arch/PKGBUILD` is intentionally not usable as a published package
recipe yet: public tag `v0.1.0` and its source archive do not exist. After the
release is published, verify the archive and supply its real checksum before
running `makepkg`:

```sh
export LIGHTSYNC_V0_1_0_SHA256='<verified GitHub archive SHA-256>'
cd packaging/arch
makepkg -si
systemctl --user enable --now lightsync.service
```

The template deliberately fails when the verified checksum is absent; it does
not use `SKIP` or a fabricated digest.

## Setup

Start the service and open the GUI for the guided flow:

```sh
systemctl --user start lightsync.service
lightsync-gui
```

The complete CLI pairing flow is:

```sh
# Note the ID and host from the output.
lightsync bridge discover

# Select that bridge; --name is optional.
lightsync bridge use BRIDGE_ID BRIDGE_IP --name 'Living room'

# Press the physical link button, then use the ID printed by `bridge use`.
lightsync bridge pair BRIDGE_ID

# Note an Entertainment area ID, then select it.
lightsync area list
lightsync area select AREA_ID
```

`bridge use` makes an initial pairing attempt. If the link button was already
pressed, pairing may complete immediately; otherwise the exact continuation
command printed by `bridge use` retries the saved candidate. Create the
Entertainment area first in the Philips Hue app.

Hue credentials need an unlocked Secret Service provider in the user session.
They are never written to LightSync's configuration file.

## Use

```sh
lightsync status
lightsync status --watch --json
lightsync capabilities
lightsync sync start
lightsync sync stop
```

The first interactive portal start opens the desktop's screen chooser and
permission dialog. Select one screen and approve capture. Later daemon-start
auto-sync can reuse a portal restore token when the compositor permits it; if
fresh consent is required, start synchronization interactively again. The
`grim` backend does not use portal consent.

Brightness and intensity changes are applied while streaming. Video and Game
are implemented. Light restoration is attempted on stop and on handled capture
or stream failure when enabled. Auto-start sync is attempted when the daemon
starts and portal capture is selected. Enabling the systemd user service is the
supported way to launch the daemon at login.

### Profiles

Profiles snapshot the current sync settings, selected display, and selected
area. Activating one restores that snapshot.

```sh
lightsync profile create 'Movies'
lightsync profile list
lightsync profile activate PROFILE_UUID
lightsync profile delete PROFILE_UUID
lightsync profile activate none
```

### Language

The GUI follows the system language by default and includes complete English
and German resources. Override it with the GUI setting or:

```sh
lightsync config set language en
lightsync config set language de
lightsync config set language system
```

## Safety Limits

Stopping normally gives capture and Hue resources a bounded cleanup period and
attempts light restoration when configured. A process crash, `SIGKILL`, power
loss, compositor failure, or bridge/network loss can prevent that cleanup; in
that case the Hue Entertainment session or last streamed colors may remain
until the bridge times out or another controller changes them.

Protected content is not supported. LightSync neither bypasses protected video
paths nor guarantees useful pixels when a compositor returns black frames.

## Omarchy Plugin

Install LightSync separately, ensure `~/.local/bin` is on `PATH`, then review and
enable the plugin repository:

```sh
omarchy plugin add https://github.com/mahype/omarchy-lightsync.git --enable
```

The plugin ID remains `io.github.mahype.omarchy-lightsync`, the Omarchy
repository identity. This is intentionally distinct from the lowercase GUI
desktop/AppStream ID `io.github.mahype.omarchylightsync`.

Omarchy plugins are unsandboxed QML in the shell process. This plugin only runs
the installed CLI status stream, starts or stops sync after a right click, and
launches/focuses the GUI after a left click. It does not build or install
LightSync, start the daemon, begin capture on load, request elevated privileges,
or use repository hooks. The tooltip distinguishes a missing package, required
setup, daemon errors, idle state, and active sync. `showWhenIdle` only hides the
idle widget; errors and active sync stay visible.

Remove only the plugin with:

```sh
omarchy plugin remove io.github.mahype.omarchy-lightsync
```

## Remove

For a per-user source install, run from a checkout:

```sh
./install.sh --uninstall
```

The script removes only an installation carrying its ownership marker. It
disables `lightsync.service` only when systemd reports that the active unit is
the script-owned `~/.local` unit, so a distribution-provided unit is left alone.

For an Arch package:

```sh
systemctl --user disable --now lightsync.service
sudo pacman -Rns lightsync
```

Both removal paths preserve `~/.config/omarchy-lightsync`. Delete it and the
LightSync entries in your keyring manually only when saved setup and credentials
are no longer wanted.

## Development

```sh
cargo fetch --locked
cargo fmt --all -- --check
cargo test --frozen --workspace --all-features
cargo clippy --frozen --workspace --all-targets --all-features -- -D warnings
node --test integrations/omarchy/tests/model.test.js
./test-install.sh
omarchy plugin validate .
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
