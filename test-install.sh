#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
sandbox=$(mktemp -d)
trap 'rm -rf -- "$sandbox"' EXIT
export HOME="$sandbox/home"
export CARGO_TARGET_DIR="$sandbox/target"
mkdir -p "$HOME" "$sandbox/bin"

cat > "$sandbox/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
mkdir -p "$CARGO_TARGET_DIR/release"
for binary in lightsync lightsyncd lightsync-gui; do
  printf '#!/usr/bin/env sh\nexit 0\n' > "$CARGO_TARGET_DIR/release/$binary"
  chmod +x "$CARGO_TARGET_DIR/release/$binary"
done
EOF
cat > "$sandbox/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ " $* " == *" show "* ]]; then
  printf '%s\n' "$HOME/.local/share/systemd/user/lightsync.service"
fi
printf '%s\n' "$*" >> "$HOME/systemctl.log"
EOF
chmod +x "$sandbox/bin/cargo" "$sandbox/bin/systemctl"
export PATH="$sandbox/bin:$PATH"

"$repo_dir/install.sh"
test -x "$HOME/.local/bin/lightsync"
test -f "$HOME/.local/share/lightsync/source-install-v1"
grep -Fx 'ExecStart=%h/.local/bin/lightsyncd' \
  "$HOME/.local/share/systemd/user/lightsync.service" >/dev/null
mkdir -p "$HOME/.config/omarchy-lightsync"
printf 'preserve me\n' > "$HOME/.config/omarchy-lightsync/config.toml"

"$repo_dir/install.sh" --uninstall
test ! -e "$HOME/.local/bin/lightsync"
test -f "$HOME/.config/omarchy-lightsync/config.toml"
grep -F 'disable --now lightsync.service' "$HOME/systemctl.log" >/dev/null

: > "$HOME/systemctl.log"
mkdir -p "$HOME/.local/share/systemd/user"
printf '[Service]\nExecStart=/usr/bin/foreign-daemon\n' > \
  "$HOME/.local/share/systemd/user/lightsync.service"
"$repo_dir/install.sh" --uninstall
test -f "$HOME/.local/share/systemd/user/lightsync.service"
if grep -F 'disable --now lightsync.service' "$HOME/systemctl.log" >/dev/null; then
  printf 'uninstall disabled a foreign service\n' >&2
  exit 1
fi
