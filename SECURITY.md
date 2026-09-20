# Security Policy

## Supported versions

Until the first stable release, only the latest release on the default branch
receives security fixes.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Use GitHub's private
security advisory flow for `mahype/omarchy-lightsync`:

<https://github.com/mahype/omarchy-lightsync/security/advisories/new>

Include the affected version or commit, impact, reproduction steps, and any
suggested mitigation. Remove Hue credentials, local addresses, captured image
data, and other personal information from reports. You should receive an
acknowledgement within seven days. Disclosure timing will be coordinated after
the issue is understood and a fix is available.

## Security boundaries

LightSync handles screen capture, local network traffic, and Hue credentials.
Capture must remain an explicit user action. Credentials belong in the Linux
Secret Service and must not be written to logs or ordinary configuration.

The daemon's Unix socket is intended for the current user only. The Omarchy
plugin is unsandboxed code loaded into Omarchy Shell, but it does not receive
Hue secrets or captured frames. It reads the documented status stream and
issues only explicit CLI controls. Installing the plugin does not install or
execute the LightSync application.

General bugs and feature requests that do not have security impact may be filed
at <https://github.com/mahype/omarchy-lightsync/issues>.
