# steel-evil: Vim keybindings for Steel-enabled Helix

This directory runs this Helix fork (the `steel-evil` branch, with the Steel
plugin system merged in) with the
[mattwparas/vim.hx](https://github.com/mattwparas/vim.hx) Steel plugin in a
fully **isolated profile**. Your real `~/.config/helix`, `~/.cache/helix`,
`~/.steel` and `~/.local/share/steel` are never read or written.

| File | Purpose |
| --- | --- |
| `hx-evil` | Launcher. Sets up isolated XDG/Steel dirs, fetches the plugin if needed, then execs `hx`. |
| `fetch-vim-hx.sh` | Downloads vim.hx at a pinned commit into the local cache. |
| `profile/config.toml` | Isolated Helix config, including the MCP keys (F11/F12). |
| `profile/init.scm` | Loads vim.hx and calls `(set-vim-keybindings!)`. |
| `profile/helix.scm` | Steel module for typed commands (empty). |
| `.gitignore` | Ignores `.local/`, the state and plugin cache. |

## Licensing: why vim.hx is not vendored

vim.hx has **no license file**, so by default all rights are reserved. Its
source is therefore not copied into this repository. `fetch-vim-hx.sh` clones it
at runtime into the gitignored `.local/` cache, pinned to:

```
repo: https://github.com/mattwparas/vim.hx.git
rev:  43f9c7fd26216c15fdf2455ac19ff0441a272876   (master, 2026-03-08)
```

Don't commit anything from `.local/`. If you want to redistribute or vendor the
plugin, ask the author to add a license first.

## Build

The launcher does not build anything. Build a Steel-enabled `hx` explicitly
from the repo root:

```sh
cargo build -p helix-term --features steel             # target/debug/hx
cargo build -p helix-term --features steel --release   # target/release/hx (preferred if present)
```

(`cargo xtask steel` also works, but it *installs* `hx`, `steel` and `forge`
into `~/.cargo/bin`, which isn't needed here.)

## Run

```sh
contrib/steel-evil/hx-evil [file ...]       # launch
contrib/steel-evil/hx-evil --print-env      # show resolved paths, don't launch
contrib/steel-evil/fetch-vim-hx.sh --force  # re-fetch the plugin
```

Environment knobs:

| Variable | Default | Meaning |
| --- | --- | --- |
| `HX_BIN` | `target/release/hx`, then `target/debug/hx` | Steel-enabled `hx` binary to run |
| `HELIX_STEEL_EVIL_HOME` | `contrib/steel-evil/.local` | Root of all isolated state |
| `HX_EVIL_RESET_PROFILE=1` | unset | Re-copy `profile/*` over the isolated config |
| `VIM_HX_REV` / `VIM_HX_REPO` | pinned SHA / upstream | Override the plugin source |

`profile/*` is copied into the state dir only on first launch, so local edits
to the isolated copy persist. Use `HX_EVIL_RESET_PROFILE=1` to pick up changes
from the repo.

### How isolation and `require` resolution work

The launcher exports:

```
XDG_CONFIG_HOME / XDG_CACHE_HOME / XDG_DATA_HOME / XDG_STATE_HOME = $STATE/{config,cache,data,state}
HELIX_STEEL_CONFIG = $STATE/config/helix        # where init.scm / helix.scm are read
STEEL_HOME         = $STATE/steel               # Steel module root
HELIX_RUNTIME      = <repo>/runtime
```

It also passes `--config $STATE/config/helix/config.toml`. On macOS, Helix uses
the XDG strategy, so `XDG_CONFIG_HOME` redirects it too.

`fetch-vim-hx.sh` places the plugin at `$STEEL_HOME/cogs/vim-hx/`, the same
layout `forge pkg install` produces. In `init.scm`, `(require "vim-hx/init.scm")`
is not found relative to the config dir, so Steel falls back to
`$STEEL_HOME/cogs/vim-hx/init.scm`. The plugin's own relative requires
(`"change-motions.scm"` etc.) then resolve next to that file. A `-vv` log
confirms this:

```
steel::compiler::modules [INFO] Searching STEEL_HOME for ".../steel/cogs/vim-hx/init.scm"
steel::compiler::modules [INFO] Opening: ".../steel/cogs/vim-hx/change-motions.scm"
...
helix_term::commands::engine::steel [INFO] Finished loading init.scm!
```

### Debugging init failures

Steel errors in `init.scm` (or a missing plugin) are shown inside the editor
as an error popup or status message, and they're logged. To capture a log:

```sh
contrib/steel-evil/hx-evil --log /tmp/hx-evil.log -vv file.txt
grep -E 'init.scm|STEEL_HOME|Opening|Error' /tmp/hx-evil.log
```

The launcher itself exits non-zero, before starting `hx`, if the binary is
missing or the plugin fetch fails (`set -euo pipefail`; the fetch also verifies
the checked-out SHA).

## MCP controls

The isolated profile binds these keys in normal, select and insert modes:

| Key | Command |
| --- | --- |
| `F11` | `:mcp` |
| `F12` | `:mcp-connect mirage` |

They're defined in `profile/config.toml`. `profile/init.scm` re-asserts them
after `(set-vim-keybindings!)`. `add-global-keybinding` merges into the existing
keymap rather than replacing it, so the TOML keys normally survive. The re-assert
is a guard in case a future vim.hx binds F11/F12. The commands themselves and
the `mcp.toml` server configuration are documented in the root `MCP.md`.

## What vim.hx provides (pinned rev)

Normal mode: `h j k l`, arrows, `w e` (vim word semantics), `f F t T`, `a`,
`u`, `G`, `V` (visual line), `%` (match bracket), `p P` (clipboard), `dd`,
`d{w,W,b,B,e,E,$,^}`, `c{...}` and `y{...}` equivalents. Text objects via
`{d,c,y}{a,i}{w,p,f,c,e,x,t,T,{,[,(,<,",'}`. Plus visual/select-mode
counterparts and insert-mode `Esc` / `C-d` / `C-t`.

## Known limitations (be honest)

* **Burst input loss (Steel branch bug, not vim.hx).** While any Steel command
  runs, the Steel Ctrl-C watchdog thread in
  `helix-term/src/commands/engine/steel/mod.rs` reads and *discards* every
  pending terminal event (it polls and reads with an accept-all filter). Keys
  that arrive in the same burst as a Steel-bound key are lost. For example,
  pasting or typing `ciwchanged<Esc>` very fast, or `wwbb`, drops keys. Normal
  human typing speed works. Automated tests must pace keystrokes. The fix is
  to filter only Ctrl-C in that thread's `poll`/`read`, which is tracked
  separately because it's a Rust change.
* **No license** upstream (see above), so this is runtime download only.
* **Upstream is stale.** 2 commits (2026-03-08). Fixes such as `A I o O r s J ~ * #`,
  `H M L`, `D C`, `{ }`, `zz/zt/zb`, `; ,` and linewise `yy`/`p` exist only in
  unmerged PRs #2/#3 (the RoastBeefer00 fork, also unlicensed). To try them:
  `VIM_HX_REPO=https://github.com/RoastBeefer00/vim.hx.git VIM_HX_REV=1202a46cc9a25df160ff3d312f6c067903fdfbaa contrib/steel-evil/fetch-vim-hx.sh --force`.
* **Partial Vim coverage.** No counts (`3dw`), no `.` repeat, no Vim
  registers/macros semantics, no `:s` / ex-command parity, no marks. Several
  of the keys above (`o`, `O`, `x`, `r`, ...) fall through to Helix's
  behaviour, which differs from Vim.
* **Helix model leaks through.** Selections, multi-cursor, `Space`/`g`/`m`
  menus and Helix-native commands remain. Some operators work by selecting
  then acting, so the cursor and selection can differ from Vim afterwards.
* **Experimental API.** The Steel plugin API (`add-global-keybinding`,
  `on-key-callback`, `trigger-on-key-callback`) is unmerged upstream (Helix
  PR #8675) and may change, which can break the pinned plugin.
* **Startup cost.** Compiling the plugin adds about 2.8s of Steel init on a
  debug build. Release builds are much faster.
