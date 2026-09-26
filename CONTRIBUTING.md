# Contributing to Omarchy Light Sync for Hue

Thank you for improving the plugin. Keep changes focused, explain user-visible
behavior, and include tests for state or protocol changes.

## Development setup

The plugin needs nothing beyond Omarchy. Tests need Node.js:

```sh
node --test tests/
bash -n tools/hue-stream.sh
omarchy plugin validate .
```

Pure logic lives in the JavaScript files (`HueBridge.js`, `SyncEngine.js`,
`ConfigStore.js`, `Model.js`) and is tested with Node. `Service.qml` only wires
it to processes, files and the capture window.

## Design requirements

- No compiled helpers, daemons, installers or package dependencies. Use only
  tools Omarchy ships (`curl`, `openssl`, `bash`, `secret-tool`,
  `avahi-browse`) and Quickshell APIs.
- Keep capture opt-in. Loading the plugin must not start screen capture or
  synchronization, unless the user enabled auto start.
- Keep secrets out of command lines where the tool allows it, and out of logs
  and the settings file.
- Keep HTTPS verification against the bundled Hue root certificates.
- Keep UI copy in English and German.

## Changes and reviews

Use a descriptive branch and commit history. A pull request should state what
changed, how it was tested, and whether it affects capture consent, network
access or credentials. Do not include private Bridge data or secrets.

By contributing, you agree that your contribution is licensed under the MIT
license in this repository.
