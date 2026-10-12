#!/usr/bin/env bash
# Download mattwparas/vim.hx at a pinned commit into an isolated STEEL_HOME.
#
# vim.hx has no license file, so its source is NOT vendored in this repo.
# It is fetched at runtime into a gitignored local cache instead.
#
# Safety:
#   * Downloads into a private mktemp staging dir next to the destination,
#     verifies the commit SHA and required files, then moves it into place.
#     A failed download never touches an existing checkout.
#   * An existing checkout that is already at the pinned SHA and clean is a no-op.
#   * An existing checkout at another revision, with local modifications, or
#     that isn't a git checkout is left alone and the script fails, unless
#     --force is given. --force *renames* it to <dest>.bak-<timestamp>.
#     Nothing is ever deleted except this script's own staging dir.
#
# Usage: contrib/steel-evil/fetch-vim-hx.sh [--force]
# Env:
#   HELIX_STEEL_EVIL_HOME  state dir (default: contrib/steel-evil/.local)
#   VIM_HX_REPO            git URL or local path (default: https://github.com/mattwparas/vim.hx.git)
#   VIM_HX_REV             full commit SHA (default: pinned below)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="${HELIX_STEEL_EVIL_HOME:-$SCRIPT_DIR/.local}"
REPO="${VIM_HX_REPO:-https://github.com/mattwparas/vim.hx.git}"
REV="${VIM_HX_REV:-43f9c7fd26216c15fdf2455ac19ff0441a272876}"
# Same layout `forge pkg install` produces: $STEEL_HOME/cogs/<package-name>
COGS="$STATE/steel/cogs"
DEST="$COGS/vim-hx"
REQUIRED_FILES=(cog.scm init.scm)

die() { echo "fetch-vim-hx: error: $*" >&2; exit 1; }
log() { echo "fetch-vim-hx: $*"; }

force=0
case "${1:-}" in
  --force) force=1 ;;
  "") ;;
  -h|--help) sed -n '2,21p' "$0"; exit 0 ;;
  *) echo "fetch-vim-hx: unknown argument: $1" >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || { echo "fetch-vim-hx: too many arguments" >&2; exit 2; }

command -v git >/dev/null || die "git is required"
[[ "$REV" =~ ^[0-9a-f]{40}$ ]] || die "VIM_HX_REV must be a full 40-char commit SHA, got '$REV'"

# Classify the existing destination: absent | ok | wrong-rev | dirty | foreign
existing_state() {
  if [[ ! -e "$DEST" && ! -L "$DEST" ]]; then echo absent; return; fi
  if [[ -L "$DEST" || ! -d "$DEST/.git" ]]; then echo foreign; return; fi
  local head
  head="$(git -C "$DEST" rev-parse HEAD 2>/dev/null)" || { echo foreign; return; }
  if [[ -n "$(git -C "$DEST" status --porcelain --untracked-files=all 2>/dev/null)" ]]; then
    echo dirty; return
  fi
  [[ "$head" == "$REV" ]] && echo ok || echo wrong-rev
}

state="$(existing_state)"
case "$state" in
  ok)
    log "already at $REV (clean) in $DEST"
    exit 0 ;;
  absent) ;;
  wrong-rev|dirty|foreign)
    if [[ $force -ne 1 ]]; then
      die "$DEST exists ($state); refusing to replace it. Re-run with --force to move it aside to a timestamped backup."
    fi ;;
esac

mkdir -p "$COGS"
STAGE="$(mktemp -d "$COGS/.vim-hx.staging.XXXXXX")"
# Only ever remove our own staging dir.
cleanup() {
  if [[ -n "${STAGE:-}" && -d "$STAGE" ]]; then rm -rf -- "$STAGE"; fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

log "fetching $REPO @ $REV"
git -C "$STAGE" init -q
git -C "$STAGE" remote add origin "$REPO"
git -C "$STAGE" fetch -q --depth 1 origin "$REV" || die "fetch of $REV from $REPO failed (existing cache untouched)"
git -C "$STAGE" -c advice.detachedHead=false checkout -q --detach FETCH_HEAD || die "checkout failed"

actual="$(git -C "$STAGE" rev-parse HEAD)"
[[ "$actual" == "$REV" ]] || die "fetched $actual, expected $REV (existing cache untouched)"
for f in "${REQUIRED_FILES[@]}"; do
  [[ -f "$STAGE/$f" ]] || die "fetched tree is missing $f (existing cache untouched)"
done

if [[ "$state" != absent ]]; then
  backup="$DEST.bak-$(date +%Y%m%d-%H%M%S)-$$"
  mv -- "$DEST" "$backup"
  log "moved existing $state checkout to $backup"
fi
mv -- "$STAGE" "$DEST"
STAGE=""
log "vim.hx $REV installed to $DEST"
