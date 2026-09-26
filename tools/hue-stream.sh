#!/usr/bin/env bash
# Hue Entertainment stream over DTLS 1.2 (PSK) using the system openssl.
#
# Usage: hue-stream.sh HOST APPLICATION_KEY
# stdin: first line is the hex client key, then one HueStream frame per line
#        written as "\xHH" escapes (see packetLine in HueBridge.js).
# stdout: "ready" once the handshake completed. Errors go to stderr.
# Closing stdin ends the stream.
set -u

host=${1:-}
identity=${2:-}
IFS= read -r psk || exit 64
[[ $psk =~ ^[0-9a-fA-F]{2,}$ && $identity =~ ^[A-Za-z0-9_-]+$ && -n $host ]] || exit 64
[[ $host == *:* ]] && host="[$host]"

while IFS= read -r frame; do
  # Only escaped bytes reach printf, so a frame can never become a format string.
  # shellcheck disable=SC2059
  [[ $frame =~ ^(\\x[0-9a-f]{2})+$ ]] && printf "$frame"
done | openssl s_client -dtls1_2 -connect "$host:2100" \
  -psk_identity "$identity" -psk "$psk" -cipher PSK-AES128-GCM-SHA256 \
  -brief -nocommands 2>&1 >/dev/null | while IFS= read -r message; do
    case $message in
      "CONNECTION ESTABLISHED"*) echo ready ;;
      Protocol*|Ciphersuite*|Peer*|Verification*|Hash*|Signature*|Supported*|Shared*|"Connecting to"*|"Can't use SSL_get_servername"*|"No peer certificate"*|DONE) ;;
      *) echo "$message" >&2 ;;
    esac
  done
