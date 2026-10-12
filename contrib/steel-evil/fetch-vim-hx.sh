#!/usr/bin/env bash
# Download mattwparas/vim.hx at a pinned commit into an isolated STEEL_HOME.
#
# vim.hx has no license file, so its source is NOT vendored in this repo.
# It is fetched at runtime into a gitignored local cache instead.
#
# Usage: contrib/steel-evil/fetch-vim-hx.sh [--force]
# Env:
#   HELIX_STEEL_EVIL_HOME  state dir (default: contrib/steel-evil/.local)
#   VIM_HX_REPO            git URL (default: https://github.com/mattwparas/vim.hx.git)
#   VIM_HX_REV             commit SHA (default: pinned below)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="${HELIX_STEEL_EVIL_HOME:-$SCRIPT_DIR/.local}"
REPO="${VIM_HX_REPO:-https://github.com/mattwparas/vim.hx.git}"
REV="${VIM_HX_REV:-43f9c7fd26216c15fdf2455ac19ff0441a272876}"
# Same layout `forge pkg install` produces: $STEEL_HOME/cogs/<package-name>
DEST="$STATE/steel/cogs/vim-hx"

force=0
case "${1:-}" in
  --force) force=1 ;;
  "") ;;
  -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
  *) echo "unknown argument: $1" >&2; exit 2 ;;
esac

command -v git >/dev/null || { echo "error: git is required" >&2; exit 1; }

if [[ -d "$DEST/.git" && $force -eq 0 ]]; then
  current="$(git -C "$DEST" rev-parse HEAD 2>/dev/null || true)"
  if [[ "$current" == "$REV" ]]; then
    echo "vim.hx already at $REV in $DEST"
    exit 0
  fi
fi

rm -rf "$DEST"
mkdir -p "$DEST"
git -C "$DEST" init -q
git -C "$DEST" remote add origin "$REPO"
git -C "$DEST" fetch -q --depth 1 origin "$REV"
git -C "$DEST" checkout -q --detach FETCH_HEAD

actual="$(git -C "$DEST" rev-parse HEAD)"
if [[ "$actual" != "$REV" ]]; then
  echo "error: fetched $actual, expected $REV" >&2
  exit 1
fi
for f in cog.scm init.scm; do
  [[ -f "$DEST/$f" ]] || { echo "error: $DEST/$f missing" >&2; exit 1; }
done
echo "vim.hx $REV installed to $DEST"
