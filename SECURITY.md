# Security Policy

## Supported versions

Until the first stable release, only the latest release on the default branch
receives security fixes.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Use GitHub's private
security advisory flow for `mahype/omarchy-light-sync-hue`:

<https://github.com/mahype/omarchy-light-sync-hue/security/advisories/new>

Include the affected version or commit, impact, reproduction steps, and any
suggested mitigation. Remove Hue credentials, local addresses, captured image
data, and other personal information from reports. You should receive an
acknowledgement within seven days. Disclosure timing will be coordinated after
the issue is understood and a fix is available.

## Security boundaries

The plugin is unsandboxed QML inside the Omarchy shell. It handles screen
capture, local network traffic and Hue credentials:

- Screen capture runs only while the user has started synchronization.
  Captured frames are downsampled to about 64 pixels in width, stay in memory
  and are never written to disk or logged.
- Hue credentials are stored in the Secret Service, never in the settings file.
- Bridge requests are TLS-verified against the bundled Signify root
  certificates with the bridge ID as server name. The application key reaches
  curl on stdin, not on the command line.
- The Entertainment client key is passed to `openssl s_client` as a command-line
  argument while streaming, because openssl has no other way to receive a PSK.
  Other local users can see it in the process list during a sync. It only
  authorizes Entertainment streaming to the bridge on the local network.

General bugs and feature requests that do not have security impact may be filed
at <https://github.com/mahype/omarchy-light-sync-hue/issues>.
