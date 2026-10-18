#!/bin/sh
# Install opencode mesh on macOS/Linux (SPEC §10):
#   gh api repos/Hona/mesh/contents/install.sh -H "Accept: application/vnd.github.raw" | sh
#   ... | sh -s -- --yes            (arguments go to `mesh up`)
#   ... | MESH_VERSION=0.1.0 sh     (a release; default: the latest)
# Downloads with gh, checks SHA256SUMS, then `mesh up` installs to ~/.local/bin (adding it to
# PATH in the shell profile if needed) and sets this device up.
set -eu
case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) echo "mesh: unsupported OS $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) echo "mesh: unsupported CPU $(uname -m)" >&2; exit 1 ;;
esac
asset="mesh-$os-$arch"
dir=$(mktemp -d)
tag=""
if [ -n "${MESH_VERSION:-}" ]; then tag="v${MESH_VERSION#v}"; fi
# shellcheck disable=SC2086 # empty $tag means "latest"
gh release download $tag -R Hona/mesh -D "$dir" --clobber -p "$asset" -p SHA256SUMS
cd "$dir"
want=$(awk -v a="$asset" '$2 == a { print $1 }' SHA256SUMS)
if command -v sha256sum >/dev/null 2>&1; then
  got=$(sha256sum "$asset" | cut -d' ' -f1)
else
  got=$(shasum -a 256 "$asset" | cut -d' ' -f1)
fi
if [ -z "$want" ] || [ "$want" != "$got" ]; then
  echo "mesh: $asset: SHA256 mismatch" >&2
  exit 1
fi
chmod 755 "$asset"
mv "$asset" mesh
# stdin is this script when piped into sh: give `mesh up` the terminal for its questions
if [ -r /dev/tty ] && [ -t 1 ]; then
  ./mesh up "$@" </dev/tty
else
  ./mesh up "$@"
fi
cd /
rm -rf "$dir"
