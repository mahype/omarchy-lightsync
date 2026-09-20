# Contributing to LightSync

Thank you for improving LightSync. Keep changes focused, explain user-visible
behavior, and include tests for state or protocol changes.

## Development setup

Install Rust 1.85 or newer and the native development packages listed in the
README. Build and check the workspace with:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Integration-only changes should also run:

```sh
node --test integrations/omarchy/tests
bash -n install.sh packaging/arch/PKGBUILD
omarchy plugin validate .
```

Run the last command when the installed Omarchy version provides the plugin
validator. Test QML changes in a disposable Omarchy plugin checkout when
possible.

## Design requirements

- Keep capture opt-in. A daemon, desktop file, or plugin loading must not start
  screen capture or synchronization.
- Do not add privileged helpers or implicit package installation.
- Keep machine-readable output backward compatible or version its contract.
- Never log Hue credentials, Secret Service values, captured pixels, or raw
  frame data.
- Prefer the streaming status interface over repeated polling.
- Keep UI copy in English by default. German localization may be added where
  the integration can select it safely.
- Update dependency disclosures and packaging metadata when native or runtime
  requirements change.

## Changes and reviews

Use a descriptive branch and commit history. A pull request should state what
changed, how it was tested, and whether it affects capture consent, network
access, credentials, IPC, packaging, or the Omarchy plugin. Do not include
generated build output, private Bridge data, or secrets.

By contributing, you agree that your contribution is licensed under the MIT
license in this repository.
