#!/usr/bin/env bash
# Build and install a per-user LightSync checkout. Nothing invokes this script
# automatically; inspect it and run it explicitly if this install style fits.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
prefix="$HOME/.local"
mode=install

case ${1:-} in
  "") ;;
  --uninstall) mode=uninstall ;;
  -h|--help)
    printf 'Usage: %s [--uninstall]\nInstalls only to $HOME/.local.\n' "$0"
    exit 0
    ;;
  *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
esac

[[ $HOME = /* ]] || { printf 'HOME must be an absolute path\n' >&2; exit 2; }
if [[ ${PREFIX+x} && $PREFIX != "$prefix" ]]; then
  printf 'Custom PREFIX values are not supported; LightSync installs only to %s\n' "$prefix" >&2
  exit 2
fi

files=(
  "$prefix/bin/lightsync"
  "$prefix/bin/lightsyncd"
  "$prefix/bin/lightsync-gui"
  "$prefix/share/applications/io.github.mahype.omarchylightsync.desktop"
  "$prefix/share/metainfo/io.github.mahype.omarchylightsync.metainfo.xml"
  "$prefix/share/icons/hicolor/scalable/apps/io.github.mahype.omarchylightsync.svg"
  "$prefix/share/systemd/user/lightsync.service"
  "$prefix/share/licenses/lightsync/LICENSE"
)
marker="$prefix/share/lightsync/source-install-v1"
unit_path=${files[6]}

if [[ $mode = uninstall ]]; then
  if [[ ! -f $marker ]]; then
    printf 'No LightSync source install is recorded in %s; nothing was removed.\n' "$prefix"
    exit 0
  fi
  fragment=
  if command -v systemctl >/dev/null 2>&1; then
    fragment=$(systemctl --user show --property=FragmentPath --value lightsync.service 2>/dev/null || true)
  fi
  if [[ $fragment == "$unit_path" ]]; then
    systemctl --user disable --now lightsync.service 2>/dev/null || true
  fi
  rm -f -- "${files[@]}"
  rm -f -- "$marker"
  if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload 2>/dev/null || true
  fi
  printf 'Removed LightSync files from %s. User configuration was preserved.\n' "$prefix"
  exit 0
fi

command -v cargo >/dev/null 2>&1 || { printf 'cargo is required to build LightSync\n' >&2; exit 1; }
if [[ ! -f $marker ]]; then
  for file in "${files[@]}"; do
    if [[ -e $file ]]; then
      printf 'Refusing to overwrite untracked file: %s\n' "$file" >&2
      exit 1
    fi
  done
fi
cargo build --manifest-path "$repo_dir/Cargo.toml" --release --workspace --locked
target_dir=${CARGO_TARGET_DIR:-"$repo_dir/target"}

install -Dm644 /dev/null "$marker"
install -Dm755 "$target_dir/release/lightsync" "${files[0]}"
install -Dm755 "$target_dir/release/lightsyncd" "${files[1]}"
install -Dm755 "$target_dir/release/lightsync-gui" "${files[2]}"
install -Dm644 "$repo_dir/data/io.github.mahype.omarchylightsync.desktop" "${files[3]}"
install -Dm644 "$repo_dir/data/io.github.mahype.omarchylightsync.metainfo.xml" "${files[4]}"
install -Dm644 "$repo_dir/data/icons/hicolor/scalable/apps/io.github.mahype.omarchylightsync.svg" "${files[5]}"
install -Dm644 "$repo_dir/LICENSE" "${files[7]}"

unit_tmp=$(mktemp)
trap 'rm -f -- "$unit_tmp"' EXIT
while IFS= read -r line || [[ -n $line ]]; do
  if [[ $line == ExecStart=* ]]; then
    printf '%s\n' 'ExecStart=%h/.local/bin/lightsyncd'
  else
    printf '%s\n' "$line"
  fi
done < "$repo_dir/data/systemd/user/lightsync.service" > "$unit_tmp"
install -Dm644 "$unit_tmp" "${files[6]}"
if command -v systemctl >/dev/null 2>&1; then
  systemctl --user daemon-reload 2>/dev/null || \
    printf 'Warning: systemd user manager is unavailable; reload it before starting LightSync.\n' >&2
fi

printf 'Installed LightSync into %s. Start it with:\n  systemctl --user enable --now lightsync.service\n' "$prefix"
