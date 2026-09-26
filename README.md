# Omarchy Light Sync for Hue

Synchronizes a Philips Hue Entertainment area with your screen, like an
ambilight, straight from the Omarchy bar. The plugin runs entirely inside the
Omarchy shell: no daemon, no build step, no extra packages and no cloud.

LightSync does not bypass DRM or HDCP. Protected video may be black in a
screen capture and therefore cannot drive the lights.

## Features

- Video and Game modes. Game uses a smaller sample area, higher exposure and
  faster smoothing than Video.
- Four intensity presets and a brightness control, per-light spatial sampling
  based on the light positions of the Entertainment area, 30 FPS streaming.
- Bridge discovery (mDNS, falling back to Signify's discovery service) and
  pairing from the panel, Entertainment area selection.
- Display selection on multi-monitor setups.
- Optional restoration of the previous light state after stopping.
- Optional start of the synchronization when the shell loads.

## Requirements

- Omarchy with the Quickshell-based `omarchy-shell`
- A Philips Hue bridge (v2, square) with an Entertainment area, created in the
  Hue app
- A Secret Service provider for the bridge key (GNOME Keyring is the Omarchy
  default)

Everything else ships with Omarchy: `curl`, `openssl`, `bash`, `secret-tool`
and `avahi-browse`.

## Install

```sh
omarchy plugin add https://github.com/mahype/omarchy-lightsync-hue.git --enable
```

Then click the light bulb in the bar:

1. **Find Hue bridge**, then **Pair** and press the round button on the bridge
   within 30 seconds.
2. Pick an **Entertainment area**.
3. **Start synchronization**, or right-click the bar icon.

## Keyboard and scripting

The widget registers the IPC target `io.github.mahype.omarchy-lightsync-hue`:

```sh
omarchy-shell io.github.mahype.omarchy-lightsync-hue toggleSync
omarchy-shell io.github.mahype.omarchy-lightsync-hue start
omarchy-shell io.github.mahype.omarchy-lightsync-hue stop
omarchy-shell io.github.mahype.omarchy-lightsync-hue status   # JSON
```

`open`, `close`, `toggle` and `refresh` control the panel.

## How it works

| Part | Implementation |
|---|---|
| Screen capture | Quickshell `ScreencopyView` in an invisible 1 px layer window, downsampled on the GPU to about 64 px width and read back through a QML `Canvas`. No ScreenCast portal. |
| Color processing | `SyncEngine.js`: sRGB to linear light, per-channel area sampling, exposure, black cutoff and attack/release smoothing. |
| Bridge API | `curl`, verified against Signify's Hue root certificates in `certs/` with the bridge ID as TLS name. The request, including the application key, is passed on stdin (`curl -K -`), never on the command line. |
| Streaming | `tools/hue-stream.sh`: HueStream v2 frames over DTLS 1.2 with PSK (`PSK-AES128-GCM-SHA256`) via `openssl s_client`. |
| Credentials | Secret Service through `secret-tool` (`service=io.github.mahype.omarchy-lightsync-hue`, `bridge=<bridge id>`). |
| Settings | `~/.config/omarchy-lightsync-hue/config.json` (no secrets). |

Screen capture only runs while synchronization is active; loading the plugin
never starts capture or streaming. If the shell exits during a sync, the
bridge ends the stream after its timeout; the next start takes over the
leftover session of this plugin, but never a session of another app.

## What it stores and where it connects

| What | Where |
|---|---|
| Bridge ID, address, area, settings | `~/.config/omarchy-lightsync-hue/config.json` |
| Hue application key and Entertainment client key | Secret Service |
| Hue bridge (HTTPS 443, DTLS 2100) | your local network |
| `discovery.meethue.com` | only when mDNS finds no bridge |

## Remove

```sh
secret-tool clear service io.github.mahype.omarchy-lightsync-hue
rm -r ~/.config/omarchy-lightsync-hue
omarchy plugin remove io.github.mahype.omarchy-lightsync-hue
```

The bridge keeps a registration entry named `omarchy-lightsync-hue#desktop`;
remove it in the Hue app if you like.

## Development

```sh
node --test tests/
bash -n tools/hue-stream.sh
omarchy plugin validate .
```

For live testing, symlink the checkout to
`~/.config/omarchy/plugins/io.github.mahype.omarchy-lightsync-hue`. Changes to
`Service.qml` need `omarchy restart shell` because the service is kept loaded.

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
