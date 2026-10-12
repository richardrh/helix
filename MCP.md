# MCP connections and Mirage

This fork includes an MCP client using the official Rust SDK (`rmcp`). It supports
Streamable HTTP and local stdio servers. It connects, lists tools and runs tools
on explicit request. It is not an AI agent: there is no model integration,
automatic tool execution or automatic transmission of buffer contents.

## Connect in one action

- **F12**: connect Mirage in the selected dev/prod mode.
- **F11** or `:mcp`: open the MCP manager and choose **Connect Mirage**.
- `:mcp-status`: show connection state and the most recent connection error.
- `:mcp-config`: open the active profile's `mcp.toml`, creating an example only
  if the file does not exist.
- `:mcp-connect SERVER`, `:mcp-disconnect SERVER`, `:mcp-tools SERVER`: manage a
  named server. The default server is Mirage.
- `:mcp-call SERVER TOOL {"argument":"value"}`: explicitly invoke a tool. Tool
  results open in a new scratch buffer, leaving existing buffers intact.

On macOS you may need **Fn-F11/F12**, or change system shortcuts that capture
those keys. Typed commands always remain available.

Mirage is built in and gets one non-blocking startup connection attempt by
default. Failure is reported in the editor and does not prevent editing.
Reconnect with F12 after starting the server. No repeated connection loop runs
in the background. HTTP initialization/session management is handled by the SDK.

## Where configuration lives

A direct `hx` launch uses `mcp.toml` in the normal Helix user configuration
directory. The isolated `contrib/steel-evil/hx-evil` launcher instead uses:

```text
contrib/steel-evil/.local/config/helix/mcp.toml
```

Or `$HELIX_STEEL_EVIL_HOME/config/helix/mcp.toml` when overridden. Use
`:mcp-config` to avoid editing the wrong profile. Save changes and reconnect;
connection configuration is loaded again on each connection. Repository-local
`.helix` configuration is not used for MCP process execution or credentials.

## Dev mode: local Mirage endpoint

```toml
mirage_mode = "dev"

[servers.mirage]
url = "http://127.0.0.1:7345/mcp"
token_env = "MIRAGE_MCP_TOKEN"
# Optional fallback when the environment variable is not set:
# token_file = "~/.config/mirage/docs-mcp.token"
auto_connect = true
```

`MIRAGE_MODE=dev` also selects this mode. The endpoint is a default, not an
assumption that a cluster is already running. Set `url` to your local cluster's
MCP service or its loopback port-forward. The ordinary Mirage API on port 8080
has no `/mcp` route and is not an MCP endpoint.

Mirage currently includes a standalone **read-only docs MCP server** at
`apps/docs-mcp`. It exposes `docs_outline`, `docs_search` and `docs_get` and can
run without any cluster. For example, from the Mirage repository:

```sh
mkdir -p "$HOME/.config/mirage" "$HOME/.local/bin"
# Create a new token only if you do not already have one. Do not overwrite
# an existing token used by another running server.
(umask 077; set -C; openssl rand -hex 32 > "$HOME/.config/mirage/docs-mcp.token")
go -C apps/docs-mcp build -o "$HOME/.local/bin/mirage-docs-mcp" ./cmd/docs-mcp
"$HOME/.local/bin/mirage-docs-mcp" serve -transport http \
  -addr 127.0.0.1:7345 -token-file "$HOME/.config/mirage/docs-mcp.token"
```

In the editor configuration, set `token_file` to that file. Keep the server
running in its own terminal. This fork does not start or modify a Kubernetes
cluster, expose a service publicly, or generate production credentials.

## Prod mode: HTTPS plus required access key

```toml
mirage_mode = "prod"

[servers.mirage-prod]
url = "https://your-mirage-host.example/mcp"
token_env = "MIRAGE_ACCESS_KEY"
require_auth = true
require_https = true
auto_connect = true
```

Or configure the mode and endpoint through the environment:

```sh
export MIRAGE_MODE=prod
export MIRAGE_PROD_MCP_URL=https://your-mirage-host.example/mcp
# Supply MIRAGE_ACCESS_KEY through your secret manager or shell environment.
# Do not paste access keys into mcp.toml, editor commands or git.
./contrib/steel-evil/hx-evil
```

The key is sent as `Authorization: Bearer <key>`. Missing credentials or an
invalid/non-HTTPS production URL prevent connection. Production has no invented
public endpoint: the inspected Mirage project does not yet define a deployed
production MCP URL or a separate access-key issuance service. The client is
ready for a bearer-key-protected HTTPS deployment once one is provided.

F12 and the manager's Mirage action follow the selected mode. Production does
not silently fall back to development. `MIRAGE_MODE` takes precedence over the
file's mode.

## Add another MCP server

Streamable HTTP:

```toml
[servers.team-docs]
url = "https://docs.example/mcp"
token_env = "TEAM_DOCS_MCP_TOKEN"
require_auth = true
auto_connect = false
```

Local stdio:

```toml
[servers.local-tools]
command = "/absolute/path/to/my-mcp-server"
args = ["--stdio"]
auto_connect = false
```

Then run `:mcp-connect team-docs` or `:mcp-connect local-tools`. Stdio executes
that configured program with your permissions, so add only servers you trust.
It is not sandboxed. The default for additional servers is no auto-connect.
To disable Mirage startup attempts, set `auto_connect = false` on the selected
Mirage server.

## Security and limitations

- Access keys are read from the environment or an explicitly configured token
  file. They are not committed or displayed in connection errors.
- HTTP redirects are refused to prevent credential forwarding to another host.
- Authenticated non-local connections require HTTPS. Production always requires
  HTTPS, including when testing against loopback.
- Startup performs the protocol handshake only. Tool invocation requires an
  explicit `:mcp-call` command. Treat tools as programs that may have side effects.
- This first integration exposes MCP **tools** only. Resources, prompts, OAuth
  login/discovery, sampling, subscriptions and legacy HTTP+SSE transport are not
  presented as editor features.
- Network requests have timeouts. Long text results are truncated for display.
- Connecting does not grant MCP servers access to editor buffers, files or
  secrets beyond the configured credentials and explicitly supplied tool args.
