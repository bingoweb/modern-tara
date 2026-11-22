#!/usr/bin/env bash
# Installs the external tools modern-tara runs, at the exact versions in
# tools.toml, for the current user only (Linux). Every download is checked
# against its SHA-256 in tools.toml. Safe to run again: a tool that is already
# at its pinned version is left alone.
#
#   ruff, ast-grep    GitHub release archive for this platform, the executable
#                     copied to $MODERN_TARA_BIN (default ~/.local/bin)
#   PSScriptAnalyzer  only when pwsh exists: install-tools.ps1 -Only PSScriptAnalyzer
#   npm set           npm ci --ignore-scripts in js/ (from js/package-lock.json)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
toml="$root/tools.toml"
bin_dir="${MODERN_TARA_BIN:-$HOME/.local/bin}"

fail() {
  echo "install-tools: $*" >&2
  exit 1
}

# pairs <section>: "key value" for every `key = "value"` line of a tools.toml section.
pairs() {
  awk -v section="$1" '
    /^[[:space:]]*(#|$)/ { next }
    /^[[:space:]]*\[/ { s = $0; gsub(/[][[:space:]]/, "", s); current = s; next }
    current == section {
      k = $0; sub(/[[:space:]]*=.*/, "", k); gsub(/^[[:space:]]+|"/, "", k)
      v = $0; sub(/^[^=]*=[[:space:]]*"/, "", v); sub(/".*/, "", v)
      print k " " v
    }' "$toml"
}

# value <section> <key>
value() {
  pairs "$1" | awk -v key="$2" '$1 == key { print $2; exit }'
}

# The x.y.z in the first line a tool prints, or nothing when it cannot run.
version_of() {
  [ -x "$1" ] || return 0
  "$1" --version 2>/dev/null | head -n 1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n 1 || true
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# install_release <tool> <version> <url> <asset>: a release archive, SHA-256
# checked, its executable copied to $bin_dir.
install_release() {
  local tool="$1" version="$2" url="$3" asset="$4"
  if [ "$(version_of "$bin_dir/$tool")" = "$version" ]; then
    echo "$tool $version: already installed"
    return
  fi
  local want got out exe
  want="$(value sha256 "$asset")"
  [ -n "$want" ] || fail "tools.toml: [sha256] has no entry for $asset"
  curl -fsSL --retry 3 -o "$tmp/$asset" "$url"
  got="$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)"
  [ "$got" = "$want" ] || fail "$asset: SHA-256 is $got, tools.toml expects $want"
  out="$tmp/$tool"
  mkdir -p "$out"
  case "$asset" in
    *.zip)
      command -v unzip >/dev/null 2>&1 || fail "unzip not found"
      unzip -q -o "$tmp/$asset" -d "$out"
      ;;
    *.tar.gz) tar -xzf "$tmp/$asset" -C "$out" ;;
  esac
  exe="$(find "$out" -type f -name "$tool" | head -n 1)"
  [ -n "$exe" ] || fail "$asset: no $tool inside"
  mkdir -p "$bin_dir"
  install -m 0755 "$exe" "$bin_dir/$tool"
  echo "$tool $version: installed to $bin_dir/$tool"
}

[ -f "$toml" ] || fail "$toml not found"
ruff_v="$(value tools ruff)"
sg_v="$(value tools ast-grep)"
[ -n "$ruff_v" ] && [ -n "$sg_v" ] && [ -n "$(value tools PSScriptAnalyzer)" ] ||
  fail "tools.toml: [tools] needs ruff, ast-grep and PSScriptAnalyzer"
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "no release for $(uname -m)" ;;
esac

ruff_asset="ruff-$arch-unknown-linux-gnu.tar.gz"
install_release ruff "$ruff_v" "https://github.com/astral-sh/ruff/releases/download/$ruff_v/$ruff_asset" "$ruff_asset"
sg_asset="app-$arch-unknown-linux-gnu.zip"
install_release ast-grep "$sg_v" "https://github.com/ast-grep/ast-grep/releases/download/$sg_v/$sg_asset" "$sg_asset"

# PSScriptAnalyzer, only where PowerShell 7 exists.
if command -v pwsh >/dev/null 2>&1; then
  pwsh -NoProfile -NonInteractive -File "$root/scripts/install-tools.ps1" -Only PSScriptAnalyzer -BinDir "$bin_dir"
else
  echo "PSScriptAnalyzer: skipped (pwsh not found; the PowerShell stack will be skipped)"
fi

# The npm set: npm ci only when a pinned package is missing or differs; no install scripts run.
js="$root/js"
stale=0
while read -r name version; do
  [ -n "$name" ] || continue
  pkg="$js/node_modules/$name/package.json"
  have=""
  if [ -f "$pkg" ] && command -v node >/dev/null 2>&1; then
    have="$(node -e 'process.stdout.write(String(require(process.argv[1]).version))' "$pkg")"
  fi
  [ "$have" = "$version" ] || stale=1
done < <(pairs npm)
if [ "$stale" = 0 ]; then
  echo "npm set: already installed"
else
  command -v npm >/dev/null 2>&1 || fail "npm not found: install Node.js 26 or newer first."
  (cd "$js" && npm ci --ignore-scripts --no-audit --no-fund)
fi
