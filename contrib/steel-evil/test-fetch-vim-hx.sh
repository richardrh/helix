#!/usr/bin/env bash
# Offline safety tests for fetch-vim-hx.sh using a local git fixture.
# Usage: contrib/steel-evil/test-fetch-vim-hx.sh
# Creates everything under a fresh mktemp dir and removes only that dir.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FETCH="$SCRIPT_DIR/fetch-vim-hx.sh"
T="$(mktemp -d "${TMPDIR:-/tmp}/fetch-vim-hx-test.XXXXXX")"
cleanup() {
  if [[ -n "$T" && -d "$T" && "$T" == */fetch-vim-hx-test.* ]]; then rm -rf -- "$T"; fi
}
trap cleanup EXIT

pass=0; fail=0
ok()   { echo "ok   - $*"; pass=$((pass+1)); }
bad()  { echo "FAIL - $*"; fail=$((fail+1)); }
check() { local d="$1"; shift; if "$@"; then ok "$d"; else bad "$d"; fi; }

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# Fixture repo with two commits; allow fetching arbitrary SHAs.
SRC="$T/src"
git init -q "$SRC"
git -C "$SRC" config uploadpack.allowAnySHA1InWant true
printf "(define package-name 'vim-hx)\n" > "$SRC/cog.scm"
printf ';; v1\n' > "$SRC/init.scm"
git -C "$SRC" add . && git -C "$SRC" commit -q -m v1
REV1="$(git -C "$SRC" rev-parse HEAD)"
printf ';; v2\n' > "$SRC/init.scm"
git -C "$SRC" commit -q -am v2
REV2="$(git -C "$SRC" rev-parse HEAD)"
git -C "$SRC" rm -q cog.scm && git -C "$SRC" commit -q -m broken
REV_BROKEN="$(git -C "$SRC" rev-parse HEAD)"

export HELIX_STEEL_EVIL_HOME="$T/state" VIM_HX_REPO="file://$SRC" VIM_HX_REV="$REV1"
DEST="$T/state/steel/cogs/vim-hx"
COGS="$T/state/steel/cogs"
run() { "$FETCH" "$@" >"$T/out" 2>&1; }
head_of() { git -C "$DEST" rev-parse HEAD; }
no_staging() { ! compgen -G "$COGS/.vim-hx.staging.*" >/dev/null; }
backups() { compgen -G "$DEST.bak-*" | wc -l | tr -d ' '; }

# 1. fresh install
check "fresh install succeeds" env VIM_HX_REV="$REV1" "$FETCH"
check "fresh install at REV1" test "$(head_of)" = "$REV1"
check "no staging left after install" no_staging

# 2. clean + same rev = no-op
ino_before="$(ls -di "$DEST" | awk '{print $1}')"
check "same rev exits 0" run
check "same rev reports no-op" grep -q "already at" "$T/out"
check "same rev keeps the same directory" test "$(ls -di "$DEST" | awk '{print $1}')" = "$ino_before"
check "same rev makes no backup" test "$(backups)" = 0

# 3. wrong rev without --force refuses, keeps checkout
if VIM_HX_REV="$REV2" run; then bad "wrong rev without --force refused"; else ok "wrong rev without --force refused"; fi
check "wrong rev: checkout kept at REV1" test "$(head_of)" = "$REV1"
check "wrong rev: no backup made" test "$(backups)" = 0

# 4. dirty checkout refuses even at the same rev
echo ';; my edit' >> "$DEST/init.scm"
if VIM_HX_REV="$REV1" run; then bad "dirty checkout refused"; else ok "dirty checkout refused"; fi
check "dirty: user edit preserved" grep -q "my edit" "$DEST/init.scm"
if VIM_HX_REV="$REV2" run; then bad "dirty + wrong rev refused"; else ok "dirty + wrong rev refused"; fi
check "dirty: user edit still preserved" grep -q "my edit" "$DEST/init.scm"

# 5. download failure (even with --force) leaves cache intact
if VIM_HX_REV=0123456789abcdef0123456789abcdef01234567 run --force; then bad "bad SHA fails"; else ok "bad SHA fails"; fi
check "bad SHA: user edit preserved" grep -q "my edit" "$DEST/init.scm"
check "bad SHA: no backup made" test "$(backups)" = 0
check "bad SHA: staging cleaned" no_staging
if VIM_HX_REPO="file://$T/nonexistent" VIM_HX_REV="$REV2" run --force; then bad "bad repo fails"; else ok "bad repo fails"; fi
check "bad repo: user edit preserved" grep -q "my edit" "$DEST/init.scm"
check "bad repo: staging cleaned" no_staging

# 6. verification failure (missing required file) leaves cache intact
if VIM_HX_REV="$REV_BROKEN" run --force; then bad "missing cog.scm fails"; else ok "missing cog.scm fails"; fi
check "missing file: user edit preserved" grep -q "my edit" "$DEST/init.scm"
check "missing file: staging cleaned" no_staging

# 7. --force with dirty checkout backs up, never deletes
check "--force dirty succeeds" env VIM_HX_REV="$REV2" "$FETCH" --force
check "--force: now at REV2" test "$(head_of)" = "$REV2"
check "--force: exactly one backup" test "$(backups)" = 1
check "--force: backup keeps user edit" grep -q "my edit" $(compgen -G "$DEST.bak-*")/init.scm
check "--force: staging cleaned" no_staging

# 8. foreign (non-git) dir refused without --force
mv "$DEST" "$T/moved"
mkdir -p "$DEST" && echo keep > "$DEST/file"
if VIM_HX_REV="$REV1" run; then bad "foreign dir refused"; else ok "foreign dir refused"; fi
check "foreign dir untouched" grep -q keep "$DEST/file"

# 9. argument validation
if run --bogus; then bad "unknown arg rejected"; else ok "unknown arg rejected"; fi
if VIM_HX_REV=abc run; then bad "short SHA rejected"; else ok "short SHA rejected"; fi
check "foreign dir still untouched" grep -q keep "$DEST/file"

echo "# $pass passed, $fail failed"
[[ $fail -eq 0 ]]
