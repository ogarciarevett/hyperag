#!/bin/sh
# Installs the latest hyperag release for this machine and checks it against SHA256SUMS.
#   curl -fsSL https://raw.githubusercontent.com/ogarciarevett/hyperag/main/scripts/install.sh | sh
# HYPERAG_VERSION pins a tag; HYPERAG_INSTALL_DIR picks the directory (default ~/.local/bin).
# While the repository is private, set GH_TOKEN to a token that can read it.
set -eu
repo="ogarciarevett/hyperag"
dir="${HYPERAG_INSTALL_DIR:-$HOME/.local/bin}"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  *) echo "hyperag: no release for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
auth=""
[ -n "${GH_TOKEN:-}" ] && auth="Authorization: Bearer $GH_TOKEN"
api() { curl -fsSL ${auth:+-H "$auth"} -H "Accept: application/vnd.github+json" "$1"; }
tag="${HYPERAG_VERSION:-$(api "https://api.github.com/repos/$repo/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)}"
case "$tag" in
  ''|*[!a-zA-Z0-9._-]*) echo "hyperag: invalid or missing release tag" >&2; exit 1 ;;
esac
name="hyperag-$tag-$target"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
release_json="$(api "https://api.github.com/repos/$repo/releases/tags/$tag")"
fetch() {
  # Release assets through the API, so a token works for a private repository too. Each asset
  # object lists its API "url" before its "name": remember the last asset url, print it at the name.
  url=$(printf '%s' "$release_json" | grep -o '"url": *"[^"]*/releases/assets/[0-9]*"\|"name": *"[^"]*"' \
    | awk -v want="\"$1\"" '/"url"/ { sub(/^"url": *"/, ""); sub(/"$/, ""); last = $0; next } { sub(/^"name": */, ""); if ($0 == want) { print last; exit } }')
  [ -n "$url" ] || { echo "hyperag: $1 is not in release $tag" >&2; exit 1; }
  curl -fsSL ${auth:+-H "$auth"} -H "Accept: application/octet-stream" -o "$tmp/$1" "$url"
}
fetch "$name.tar.gz"
fetch SHA256SUMS
(cd "$tmp" && grep " $name.tar.gz\$" SHA256SUMS | { command -v sha256sum >/dev/null && sha256sum -c - || shasum -a 256 -c -; }) >/dev/null \
  || { echo "hyperag: checksum mismatch for $name.tar.gz" >&2; exit 1; }
tar xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/$name/hyperag" "$dir/hyperag"
echo "hyperag $tag installed to $dir/hyperag"
case ":$PATH:" in *":$dir:"*) ;; *) echo "add $dir to PATH" ;; esac
