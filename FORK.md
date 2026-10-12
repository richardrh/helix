# Steel + Evil + MCP fork

This is a personal integration fork of Helix, maintained on `steel-evil`. It is
not an upstream Helix release. The original project README follows unchanged.

## Build and run

```sh
git clone --branch steel-evil https://github.com/richardrh/helix.git
cd helix
cargo build --locked -p helix-term --release
./contrib/steel-evil/hx-evil
```

Steel is enabled by default in this fork. The launcher downloads the pinned
`mattwparas/vim.hx` plugin on first use and creates an isolated profile under
`contrib/steel-evil/.local/`. It does not install over your current `hx` or edit
your existing Helix configuration. Set `HELIX_STEEL_EVIL_HOME` to use a different
profile location. See [the profile guide](contrib/steel-evil/README.md).

For a faster development build, omit `--release`. The launcher prefers a release
binary when present, so set `HX_BIN="$PWD/target/debug/hx"` when testing a debug
build alongside an older release build. A direct `target/debug/hx` invocation
uses your ordinary Helix configuration rather than the isolated Evil profile.

The Vim plugin provides operator-first bindings, not complete Emacs Evil or Vim
compatibility. Its source is downloaded from its author, not redistributed in
this repository, because it has no explicit license at the pinned revision.
Review its limitations in the profile guide.

## MCP quick access

- **F11** or `:mcp`: connection manager.
- **F12** or `:mcp-connect mirage`: connect Mirage using the selected mode.
- `:mcp-config`: add/edit servers in the active user profile.
- `:mcp-status`, `:mcp-tools`, `:mcp-disconnect`: inspect and manage connections.
- `:mcp-call <server> <tool> <json>`: explicitly invoke a tool.

Mirage gets one background connection attempt at startup. An unavailable server
must not block editing. Production requires HTTPS and an access key obtained
from the environment. No tools are invoked automatically and no buffer contents
are sent on connection. See [MCP configuration](MCP.md) for dev/prod setup,
authentication, other servers and the limits of this client.

## Included changes

| Source | Pinned revision / decision |
| --- | --- |
| Upstream Helix | `ba40e5474` at fork creation |
| Steel plugin PR #8675 | `c16fac096a9dd162d46f53bf2411f36251d755f3`, merged with current upstream |
| Steel runtime | `24cd21598c091fb88bc10a6375a1ded25e677c37` |
| Vim Steel plugin | `43f9c7fd26216c15fdf2455ac19ff0441a272876`, downloaded by the launcher |
| PR #16290 | Adjacent numeric separators no longer freeze increment operations |
| PR #16274 | Stale snippet tabstop sessions end instead of panicking |
| Fork additions | Isolated Evil profile, native MCP client and connection manager |

The Steel integration needed a Cargo.lock resolution and adaptation to upstream's
`OnceLock` migration. This fork also fixes the PR's interrupt watcher consuming
ordinary queued keystrokes, verified with burst-input terminal tests. Other
architectural PRs were deliberately deferred. The
review shortlist and reasons are in [FORK_PR_REVIEW.md](FORK_PR_REVIEW.md).

## Validation commands

```sh
cargo test --locked -p helix-core --lib
cargo test --locked -p helix-term --lib
cargo test --locked -p helix-term --features integration --profile integration --test integration
cargo build --locked -p helix-term
python3 contrib/test-steel-evil.py
```

The Rust editor integration suite tests built-in editing with personal Steel
scripts disabled, so it cannot load a developer's local plugins. The Python
test separately drives the actual Steel-enabled editor and Vim plugin in a
pseudo-terminal, checking saved file contents for operator-first deletion,
change and insertion, including burst typing. It uses only
an isolated temporary profile. It requires a POSIX host, Python 3 and network
access to fetch the pinned Vim plugin on first use. MCP transport tests run
against local test servers rather than requiring production credentials.

## Verified results (2026-10-12)

Validated locally on macOS ARM64:

- 170 core unit tests, 38 editor unit tests and 177 editor integration tests passed.
- 34 offline plugin-installer checks passed, including preservation of existing files.
- Six real-editor Steel/Vim terminal scenarios passed, including burst typing.
- Four real Mirage docs-MCP terminal scenarios passed: automatic connection,
  tool listing/calling and connection keys, missing production-key refusal,
  disabled autoconnect, and the Evil profile's connection keys.
- Default Steel-enabled build and `--no-default-features` check passed.
- Fresh-profile startup creates generated Steel directories without preconfiguration.

The live MCP tests used a temporary loopback server and a synthetic token, not
production credentials. Production HTTPS/access-key enforcement is tested, but
an actual production Mirage endpoint must still be supplied by its operator.
The dedicated GitHub workflow reproduces the non-Mirage checks; these local
results do not imply that hosted CI has already run. Optional-feature builds
still emit upstream unused-code warnings.

## Updating

Keep upstream `master` separate from the integration branch. Merge new upstream
changes into `steel-evil`, update each dependency pin intentionally and run the
validation above before publishing. Do not blindly update Steel master or
replace the plugin checkout: its APIs are still evolving. Preserve original
commit authors when importing upstream fixes.
