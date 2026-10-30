#!/bin/sh
# Release assembly replaces this marker with the matching tag.
set -eu

fail() { printf '%s\n' "shellboy installer: $*" >&2; exit 1; }
version=${SHELLBOY_VERSION:-__SHELLBOY_RELEASE_VERSION__}
case "$(uname -s):$(uname -m)" in
    Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
    Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-gnu ;;
    *) fail "Unsupported OS/CPU: $(uname -s)/$(uname -m). No release archive is available." ;;
esac
command -v curl >/dev/null 2>&1 || fail 'curl is required.'
case "$version" in v*) ;; *) version=v$version ;; esac
# Restrict the tag before using it in URLs and archive paths.
case "$version" in *[!v0-9.]*) fail 'Use a release installer asset or set SHELLBOY_VERSION to a stable version such as v0.3.0.' ;; esac
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || fail 'Use a release installer asset or set SHELLBOY_VERSION to a stable version such as v0.3.0.'
archive=shellboy-$version-$target.tar.gz
base=https://github.com/tya5/shellboy/releases/download/$version
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' 0
trap 'exit 1' HUP INT TERM
curl -fsSL "$base/$archive" -o "$tmp/$archive"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
expected=$(awk -v name="$archive" '$2 == name {print $1}' "$tmp/SHA256SUMS")
[ "${#expected}" -eq 64 ] || fail 'Missing or invalid SHA256SUMS entry.'
case "$expected" in *[!0-9a-fA-F]*) fail 'Invalid SHA256SUMS entry.' ;; esac
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$archive" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')
else
    fail 'sha256sum or shasum is required.'
fi
[ "$actual" = "$expected" ] || fail 'SHA-256 mismatch; nothing installed.'
tar -xzf "$tmp/$archive" -C "$tmp"
binary=$tmp/shellboy-$version-$target/shellboy
[ -f "$binary" ] || fail 'Archive does not contain shellboy.'
chmod +x "$binary"
installed_version=$("$binary" --version)
[ "$installed_version" = "shellboy ${version#v}" ] || fail 'Archive binary version does not match the requested version.'
dir=${SHELLBOY_INSTALL_DIR:-$HOME/.local/bin}
mkdir -p "$dir"
# Stage on the destination filesystem, then rename so readers never see a partial copy.
staged=$(mktemp "$dir/.shellboy-install.XXXXXX")
trap 'rm -rf "$tmp"; rm -f "$staged"' 0
cp "$binary" "$staged"
chmod 755 "$staged"
if [ -e "$dir/shellboy" ] || [ -L "$dir/shellboy" ]; then
    backup=$dir/shellboy.backup-$(date -u +%Y%m%dT%H%M%SZ)-$$
    [ ! -e "$backup" ] || fail "Backup already exists: $backup"
    cp -p "$dir/shellboy" "$backup"
    printf 'Previous binary saved to %s\n' "$backup"
fi
mv -f "$staged" "$dir/shellboy"
printf 'Installed %s to %s\n' "$installed_version" "$dir/shellboy"
case ":${PATH:-}:" in
    *:"$dir":*) ;;
    *) printf 'Add the install directory to PATH (no shell files were changed):\n  export PATH="%s:$PATH"\n' "$dir" ;;
esac
printf '%s\n' \
    'If an owner is running, restart it to use the new binary.' \
    'Before first startup: stop the owner and back up the data directory.' \
    'First startup may upgrade databases; older versions cannot open them.' \
    "Read upgrade details: https://github.com/tya5/shellboy/releases/tag/$version"

printf '%s\n' \
    'First setup: shellboy config init -> edit the reported config path and set api_key_env -> shellboy config check -> shellboy' \
    'Keep API key values in the environment, not the config file. Use the same --config path for every step.' \
    'Guide: shellboy docs get --id shellboy/configuration' \
    'For older releases without config init/check, follow the bundled configuration guide manually.'
