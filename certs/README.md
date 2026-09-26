# Hue bridge trust anchors

`hue-root-bridge.pem` is the legacy Philips Hue `root-bridge` certificate
published in Signify's Hue application design guidance. `hue-root-ca-01.pem` is
the replacement `Hue Root CA 01` trust anchor published by Signify for bridges
using certificates issued from February 2025.

`hue-ca-bundle.pem` is both files concatenated, because curl takes a single
`--cacert` file. CI checks that the bundle matches its sources.

Sources (retrieved 2026-09-20):

- https://developers.meethue.com/develop/application-design-guidance/using-https/
- https://developers.meethue.com/develop/application-design-guidance/hue-bridge-certificates/

These are CA certificates, not client credentials. Bridge application and DTLS
client keys are stored only in the desktop Secret Service.
