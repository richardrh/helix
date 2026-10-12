//! Model Context Protocol (MCP) client support, built on the official `rmcp` SDK.
//!
//! Configuration lives in `~/.config/helix/mcp.toml` (see [`EXAMPLE_CONFIG`]).
//! Two Mirage servers are built in:
//! * `mirage`: local dev cluster (`http://127.0.0.1:7345/mcp`), token from
//!   `$MIRAGE_MCP_TOKEN` or a token file, auto-connects by default.
//! * `mirage-prod`: HTTPS only, requires `$MIRAGE_ACCESS_KEY`, URL from config or
//!   `$MIRAGE_PROD_MCP_URL`, auto-connects only when `mirage_mode = "prod"` or
//!   `$MIRAGE_MODE=prod`.
//!
//! Security rules:
//! * Tools are never called automatically, only via [`McpManager::call_tool`].
//! * Secrets are read from env vars / token files and never logged or included in errors.
//! * Authenticated remote endpoints must use HTTPS (plain HTTP only for loopback/private hosts).
//! * HTTP redirects are refused so bearer tokens cannot leak to another origin.
//! * Stdio servers spawn processes only from the user's own config file, and are only
//!   started at startup when `auto_connect = true` is set there explicitly.
//!
//! All async methods are `Send`; the caller drives them (e.g. via helix jobs).

use std::{
    collections::{BTreeMap, HashMap},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

use parking_lot::Mutex;
use rmcp::{
    model::{
        CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
    },
    service::{RoleClient, RunningService},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, ConfigureCommandExt,
        StreamableHttpClientTransport, TokioChildProcess,
    },
    ServiceExt,
};
use serde::Deserialize;

pub const MIRAGE: &str = "mirage";
pub const MIRAGE_PROD: &str = "mirage-prod";
pub const MIRAGE_DEV_URL: &str = "http://127.0.0.1:7345/mcp";
pub const MIRAGE_DEV_TOKEN_ENV: &str = "MIRAGE_MCP_TOKEN";
pub const MIRAGE_ACCESS_KEY_ENV: &str = "MIRAGE_ACCESS_KEY";
pub const MIRAGE_PROD_URL_ENV: &str = "MIRAGE_PROD_MCP_URL";
pub const MIRAGE_MODE_ENV: &str = "MIRAGE_MODE";

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RESULT_CHARS: usize = 256 * 1024;

pub const EXAMPLE_CONFIG: &str = r#"# ~/.config/helix/mcp.toml
# "dev" (default) auto-connects the local Mirage cluster, "prod" the production endpoint.
mirage_mode = "dev"

# Built-in Mirage dev server (token: $MIRAGE_MCP_TOKEN, or uncomment token_file).
[servers.mirage]
url = "http://127.0.0.1:7345/mcp"
# token_file = "~/path/to/docs-mcp.token"
auto_connect = true

# Production: HTTPS required, key read from $MIRAGE_ACCESS_KEY.
# URL from $MIRAGE_PROD_MCP_URL (takes precedence) or set it here.
# [servers.mirage-prod]
# url = "https://<your-mirage-host>/mcp"

# A local stdio server (only spawned on explicit connect unless auto_connect = true).
# [servers.files]
# command = "my-mcp-server"
# args = ["--stdio"]
# auto_connect = false
"#;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpFile {
    #[serde(default)]
    pub mirage_mode: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, ServerConfig>,
}

/// One configured server. Exactly one of `url` / `command` must be set.
#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub url: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Env var holding the bearer token.
    pub token_env: Option<String>,
    /// File holding the bearer token (trailing newline trimmed).
    pub token_file: Option<PathBuf>,
    /// Connect at startup. Unset uses the built-in default (Mirage by mode, else false).
    pub auto_connect: Option<bool>,
    /// Fail rather than connect without a token.
    #[serde(default)]
    pub require_auth: bool,
    /// Require https regardless of host.
    #[serde(default)]
    pub require_https: bool,
    pub timeout_secs: Option<u64>,
}

// Never print env values or tokens.
impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("url", &self.url.as_deref().map(redact_url))
            .field("command", &self.command)
            .field("token_env", &self.token_env)
            .field("auto_connect", &self.auto_connect)
            .finish_non_exhaustive()
    }
}

pub fn config_path() -> PathBuf {
    helix_loader::config_dir().join("mcp.toml")
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn builtin_servers() -> BTreeMap<String, ServerConfig> {
    let mut map = BTreeMap::new();
    map.insert(
        MIRAGE.to_string(),
        ServerConfig {
            url: Some(MIRAGE_DEV_URL.to_string()),
            token_env: Some(MIRAGE_DEV_TOKEN_ENV.to_string()),
            ..Default::default()
        },
    );
    map.insert(
        MIRAGE_PROD.to_string(),
        ServerConfig {
            url: env_nonempty(MIRAGE_PROD_URL_ENV),
            token_env: Some(MIRAGE_ACCESS_KEY_ENV.to_string()),
            require_auth: true,
            require_https: true,
            ..Default::default()
        },
    );
    map
}

/// Resolved configuration: built-ins merged with the user file.
#[derive(Debug, Clone)]
pub struct McpConfig {
    pub servers: BTreeMap<String, ServerConfig>,
    pub prod_mode: bool,
}

impl McpConfig {
    pub fn load() -> Result<Self, String> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read {}: {}", path.display(), e.kind())),
        };
        Self::from_toml(text.as_deref(), env_nonempty(MIRAGE_MODE_ENV))
    }

    pub fn from_toml(text: Option<&str>, env_mode: Option<String>) -> Result<Self, String> {
        let file: McpFile = match text {
            Some(t) => {
                toml::from_str(t).map_err(|e: toml::de::Error| {
                    // Never echo values (a key may be pasted in the wrong slot).
                    match e.span() {
                        Some(sp) => format!(
                            "invalid mcp.toml (line {})",
                            t[..sp.start.min(t.len())].matches('\n').count() + 1
                        ),
                        None => "invalid mcp.toml".to_string(),
                    }
                })?
            }
            None => McpFile::default(),
        };
        let mode = env_mode
            .or(file.mirage_mode.clone())
            .unwrap_or_else(|| "dev".into())
            .to_ascii_lowercase();
        let prod_mode = match mode.as_str() {
            "dev" | "local" => false,
            "prod" | "production" => true,
            _ => return Err("invalid mirage_mode, expected dev or prod".into()),
        };
        let mut servers = builtin_servers();
        for (name, user) in file.servers {
            match servers.get_mut(&name) {
                // Merge onto built-in defaults, keeping prod's security floor.
                Some(base) => {
                    let floor_https = base.require_https;
                    let floor_auth = base.require_auth;
                    let base_env = base.token_env.clone();
                    *base = ServerConfig {
                        // Env URL wins so a stale file URL can't receive the key.
                        url: if user.command.is_some() {
                            user.url
                        } else {
                            base.url
                                .take()
                                .filter(|_| name == MIRAGE_PROD)
                                .or(user.url)
                                .or(base.url.take())
                        },
                        token_env: user.token_env.or(base_env),
                        require_https: user.require_https || floor_https,
                        require_auth: user.require_auth || floor_auth,
                        auto_connect: user.auto_connect.or(base.auto_connect),
                        ..user
                    };
                }
                None => {
                    servers.insert(name, user);
                }
            }
        }
        // Mode selects which Mirage endpoint auto-connects, unless set explicitly.
        for (name, s) in servers.iter_mut() {
            s.auto_connect = Some(match name.as_str() {
                // Only the mode-selected Mirage entry may auto-connect.
                // Stdio commands never auto-spawn unless explicitly enabled.
                MIRAGE => !prod_mode && s.auto_connect.unwrap_or(s.command.is_none()),
                MIRAGE_PROD => prod_mode && s.auto_connect.unwrap_or(true),
                _ => s.auto_connect.unwrap_or(false),
            });
        }
        if servers
            .get(MIRAGE_PROD)
            .is_some_and(|p| p.command.is_some())
        {
            return Err(format!(
                "server '{MIRAGE_PROD}' must use an https url, not a command"
            ));
        }
        for (name, s) in &servers {
            if s.url.is_some() && s.command.is_some() {
                return Err(format!(
                    "server '{name}': set either url or command, not both"
                ));
            }
        }
        Ok(Self { servers, prod_mode })
    }
}

// ---------------------------------------------------------------------------
// Security validation
// ---------------------------------------------------------------------------

fn is_local_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback(),
        Ok(IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => h.eq_ignore_ascii_case("localhost"),
    }
}

/// Validate an HTTP endpoint. Returns the parsed URL on success.
pub fn validate_http_url(
    raw: &str,
    authenticated: bool,
    require_https: bool,
) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "invalid server URL".to_string())?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("credentials in URL are not allowed, use token_env".into());
    }
    if url.query().is_some_and(|q| !q.is_empty()) && authenticated {
        return Err("query strings are not allowed on authenticated endpoints".into());
    }
    let host = url.host_str().ok_or("server URL has no host")?.to_string();
    match url.scheme() {
        "https" => Ok(url),
        "http" if require_https => Err("this server requires https".into()),
        "http" if authenticated && !is_local_host(&host) => {
            Err("refusing to send a token over plain http to a non-local host".into())
        }
        "http" => Ok(url),
        _ => Err("unsupported URL scheme (use http or https)".into()),
    }
}

fn expand_tilde(p: &Path) -> PathBuf {
    helix_stdx::path::expand_tilde(p).into_owned()
}

fn validate_token(t: &str) -> Result<String, String> {
    let t = t.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) || !t.is_ascii() {
        return Err("token is empty or malformed".into());
    }
    Ok(t.to_string())
}

/// Resolve the bearer token. Errors name the source, never the value.
pub fn resolve_token(cfg: &ServerConfig) -> Result<Option<String>, String> {
    if let Some(var) = &cfg.token_env {
        if let Some(v) = env_nonempty(var) {
            return validate_token(&v)
                .map(Some)
                .map_err(|e| format!("${var}: {e}"));
        }
    }
    if let Some(file) = &cfg.token_file {
        let path = expand_tilde(file);
        let v = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read token file {}: {}", path.display(), e.kind()))?;
        return validate_token(&v)
            .map(Some)
            .map_err(|e| format!("token file: {e}"));
    }
    if cfg.require_auth {
        let src = cfg.token_env.as_deref().unwrap_or("token_env");
        return Err(format!("access key required: set ${src}"));
    }
    Ok(None)
}

/// Scheme, host, port and path only (no userinfo, query or fragment).
pub fn redact_url(raw: &str) -> String {
    match reqwest::Url::parse(raw) {
        Ok(u) => format!(
            "{}://{}{}{}",
            u.scheme(),
            u.host_str().unwrap_or(""),
            u.port().map(|p| format!(":{p}")).unwrap_or_default(),
            u.path()
        ),
        Err(_) => "<invalid url>".into(),
    }
}

fn truncate_chars(mut s: String, max: usize) -> String {
    if let Some((i, _)) = s.char_indices().nth(max) {
        s.truncate(i);
        s.push('…');
    }
    s
}

fn scrub(msg: String, secrets: &[&str]) -> String {
    let mut msg = msg;
    for s in secrets.iter().filter(|s| s.len() >= 4) {
        msg = msg.replace(s, "***");
    }
    truncate_chars(msg, 500)
}

/// Map SDK/transport errors to a short generic message; raw errors may echo
/// server-controlled text or request details.
fn generic_error(op: &str, raw: &str) -> String {
    let r = raw.to_ascii_lowercase();
    let why = if r.contains("auth required") || r.contains("401") || r.contains("unauthorized") {
        "authentication rejected (check token)"
    } else if r.contains("403") || r.contains("forbidden") || r.contains("insufficient scope") {
        "access forbidden"
    } else if r.contains("redirect") || r.contains("30") && r.contains("unexpected server response")
    {
        "server redirected (redirects are refused)"
    } else if r.contains("connect") || r.contains("io error") || r.contains("closed") {
        "server unreachable or connection closed"
    } else if r.contains("session expired") {
        "session expired"
    } else if r.contains("json-rpc error") || r.contains("mcp error") {
        "server returned an error"
    } else {
        "protocol error"
    };
    format!("{op} failed: {why}")
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

type Client = RunningService<RoleClient, ClientConfig>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStatus {
    pub name: String,
    pub connected: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

struct Conn {
    client: Arc<Client>,
    timeout: Duration,
    secrets: Vec<String>,
}

impl Conn {
    fn alive(&self) -> bool {
        !self.client.is_closed() && !self.client.peer().is_transport_closed()
    }
}

#[derive(Default)]
struct State {
    conns: HashMap<String, Conn>,
    detail: HashMap<String, String>,
    /// Per-server lock serialising connect/disconnect.
    locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

#[derive(Default)]
pub struct McpManager {
    state: Mutex<State>,
    config_override: Mutex<Option<McpConfig>>,
}

static GLOBAL: OnceLock<McpManager> = OnceLock::new();

pub fn global() -> &'static McpManager {
    GLOBAL.get_or_init(McpManager::default)
}

fn client_info() -> ClientConfig {
    let mut info = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("helix", env!("CARGO_PKG_VERSION")),
    );
    // Mirage pins 2025-11-25 (also negotiates older versions).
    info.protocol_version = ProtocolVersion::V_2025_11_25;
    info
}

impl McpManager {
    /// Manager with a fixed config (tests / embedding).
    pub fn with_config(cfg: McpConfig) -> Self {
        Self {
            state: Mutex::default(),
            config_override: Mutex::new(Some(cfg)),
        }
    }

    pub fn config(&self) -> Result<McpConfig, String> {
        if let Some(c) = self.config_override.lock().clone() {
            return Ok(c);
        }
        McpConfig::load()
    }

    /// The Mirage server for the active mode (`mirage` or `mirage-prod`).
    pub fn default_server_name(&self) -> String {
        match self.config() {
            Ok(c) if c.prod_mode => MIRAGE_PROD.into(),
            _ => MIRAGE.into(),
        }
    }

    /// `"mirage"` is an alias for the active-mode Mirage server.
    pub fn resolve_name(&self, name: &str) -> String {
        if name == MIRAGE {
            self.default_server_name()
        } else {
            name.to_string()
        }
    }

    /// Names of configured servers marked auto_connect.
    pub fn autoconnect_servers(&self) -> Vec<String> {
        match self.config() {
            Ok(c) => {
                let mut v: Vec<String> = c
                    .servers
                    .iter()
                    .filter(|(_, s)| s.auto_connect == Some(true))
                    .map(|(n, _)| self.resolve_name(n))
                    .collect();
                v.sort();
                v.dedup();
                v
            }
            Err(e) => {
                log::warn!("mcp: {e}");
                Vec::new()
            }
        }
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        let names: Vec<String> = self
            .config()
            .map(|c| c.servers.into_keys().collect())
            .unwrap_or_default();
        let mut st = self.state.lock();
        // Drop connections whose service loop or transport has ended.
        let dead: Vec<String> = st
            .conns
            .iter()
            .filter(|(_, c)| !c.alive())
            .map(|(n, _)| n.clone())
            .collect();
        for n in dead {
            st.conns.remove(&n);
            st.detail.insert(n, "connection closed".into());
        }
        let mut all = names;
        all.extend(st.conns.keys().cloned());
        all.sort();
        all.dedup();
        all.into_iter()
            .map(|name| {
                let connected = st.conns.contains_key(&name);
                let detail = st.detail.get(&name).cloned().unwrap_or_else(|| {
                    if connected {
                        "connected".into()
                    } else {
                        "disconnected".into()
                    }
                });
                ServerStatus {
                    name,
                    connected,
                    detail,
                }
            })
            .collect()
    }

    pub fn is_connected(&self, name: &str) -> bool {
        let name = self.resolve_name(name);
        self.state.lock().conns.get(&name).is_some_and(Conn::alive)
    }

    fn set_detail(&self, name: &str, d: impl Into<String>) {
        self.state.lock().detail.insert(name.to_string(), d.into());
    }

    fn lock_for(&self, name: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.state
            .lock()
            .locks
            .entry(name.to_string())
            .or_default()
            .clone()
    }

    pub async fn connect(&self, name: &str) -> Result<String, String> {
        let name = self.resolve_name(name);
        let lock = self.lock_for(&name);
        let _guard = lock.lock().await;
        if let Some(old) = self.take_conn(&name) {
            shutdown(old).await;
        }
        self.set_detail(&name, "connecting");
        let res = self.connect_inner(&name).await;
        match &res {
            Ok(s) => self.set_detail(&name, s.clone()),
            Err(e) => {
                log::warn!("mcp: connect {name} failed: {e}");
                self.set_detail(&name, format!("error: {e}"));
            }
        }
        res
    }

    async fn connect_inner(&self, name: &str) -> Result<String, String> {
        let cfg = self.config()?;
        let server = cfg
            .servers
            .get(name)
            .ok_or_else(|| format!("unknown MCP server '{name}'"))?
            .clone();
        let timeout = server
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT);
        let (client, secrets) = if let Some(url) = &server.url {
            let token = resolve_token(&server)?;
            if server.require_auth && token.is_none() {
                return Err("access key required".into());
            }
            let url = validate_http_url(url, token.is_some(), server.require_https)?;
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
                .build()
                .map_err(|_| "failed to build HTTP client".to_string())?;
            let mut tc = StreamableHttpClientTransportConfig::with_uri(url.as_str().to_string())
                .reinit_on_expired_session(true);
            if let Some(t) = &token {
                tc = tc.auth_header(t.clone());
            }
            let transport = StreamableHttpClientTransport::with_client(http, tc);
            let client =
                tokio::time::timeout(DEFAULT_CONNECT_TIMEOUT, client_info().serve(transport))
                    .await
                    .map_err(|_| "connection timed out".to_string())?
                    .map_err(|e| generic_error("initialize", &e.to_string()))?;
            (client, token.into_iter().collect::<Vec<_>>())
        } else if let Some(cmd) = &server.command {
            let command =
                tokio::process::Command::new(expand_tilde(Path::new(cmd))).configure(|c| {
                    c.args(&server.args).envs(&server.env).kill_on_drop(true);
                });
            let (transport, _stderr) = TokioChildProcess::builder(command)
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| format!("failed to spawn '{cmd}': {}", e.kind()))?;
            let client =
                tokio::time::timeout(DEFAULT_CONNECT_TIMEOUT, client_info().serve(transport))
                    .await
                    .map_err(|_| "server start timed out".to_string())?
                    .map_err(|e| generic_error("initialize", &e.to_string()))?;
            (client, server.env.values().cloned().collect())
        } else {
            return Err(format!("server '{name}' is not configured"));
        };
        let summary = match client.peer_info() {
            Some(info) if info.server_info.is_some() => {
                let si = info.server_info.as_ref().unwrap();
                let s = format!("connected to {} {}", si.name, si.version);
                let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
                scrub(truncate_chars(s, 120), &refs)
            }
            _ => "connected".to_string(),
        };
        self.state.lock().conns.insert(
            name.to_string(),
            Conn {
                client: Arc::new(client),
                timeout,
                secrets,
            },
        );
        Ok(summary)
    }

    fn take_conn(&self, name: &str) -> Option<Conn> {
        self.state.lock().conns.remove(name)
    }

    pub async fn disconnect(&self, name: &str) -> Result<(), String> {
        let name = self.resolve_name(name);
        let lock = self.lock_for(&name);
        let _guard = lock.lock().await;
        let conn = self.take_conn(&name);
        self.set_detail(&name, "disconnected");
        let Some(conn) = conn else {
            return Err(format!("'{name}' is not connected"));
        };
        shutdown(conn).await;
        Ok(())
    }

    fn get(&self, name: &str) -> Result<(Arc<Client>, Duration, Vec<String>), String> {
        let st = self.state.lock();
        let c = st
            .conns
            .get(name)
            .filter(|c| c.alive())
            .ok_or_else(|| format!("'{name}' is not connected"))?;
        Ok((c.client.clone(), c.timeout, c.secrets.clone()))
    }

    pub async fn list_tools(&self, name: &str) -> Result<Vec<ToolInfo>, String> {
        let name = self.resolve_name(name);
        let (client, timeout, _) = self.get(&name)?;
        let tools = tokio::time::timeout(timeout, client.list_all_tools())
            .await
            .map_err(|_| "tools/list timed out".to_string())?
            .map_err(|e| generic_error("tools/list", &e.to_string()))?;
        Ok(tools
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: serde_json::Value::Object((*t.input_schema).clone()),
            })
            .collect())
    }

    /// Explicit, user-initiated tool call. `args` must be a JSON object (or null).
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<String, String> {
        let args = match args {
            serde_json::Value::Object(m) => Some(m),
            serde_json::Value::Null => None,
            _ => return Err("tool arguments must be a JSON object".into()),
        };
        let server = self.resolve_name(server);
        let (client, timeout, secrets) = self.get(&server)?;
        let mut params = CallToolRequestParams::new(tool.to_string());
        if let Some(a) = args {
            params = params.with_arguments(a);
        }
        let res = tokio::time::timeout(timeout, client.call_tool(params))
            .await
            .map_err(|_| "tool call timed out".to_string())?
            .map_err(|e| generic_error("tools/call", &e.to_string()))?;
        let mut out = res
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        if out.is_empty() {
            if let Some(v) = &res.structured_content {
                out = serde_json::to_string_pretty(v).unwrap_or_default();
            }
        }
        // Tool output is what the user asked for, but never echo our own secrets.
        let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
        for s in refs.iter().filter(|s| s.len() >= 4) {
            out = out.replace(s, "***");
        }
        let out = truncate_chars(out, MAX_RESULT_CHARS);
        if res.is_error == Some(true) {
            return Err(format!("tool error: {}", truncate_chars(out, 2000)));
        }
        Ok(out)
    }
}

async fn shutdown(conn: Conn) {
    match Arc::try_unwrap(conn.client) {
        Ok(client) => {
            let _ = tokio::time::timeout(Duration::from_secs(5), client.cancel()).await;
        }
        // In-flight calls still hold it: cancel the service loop.
        Err(shared) => shared.cancellation_token().cancel(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_dev_mode() {
        let c = McpConfig::from_toml(None, None).unwrap();
        assert!(!c.prod_mode);
        let m = &c.servers[MIRAGE];
        assert_eq!(m.url.as_deref(), Some(MIRAGE_DEV_URL));
        assert_eq!(m.auto_connect, Some(true));
        let p = &c.servers[MIRAGE_PROD];
        assert!(p.require_auth && p.require_https && p.auto_connect == Some(false));
    }

    #[test]
    fn prod_mode_switches_autoconnect() {
        let c = McpConfig::from_toml(Some("mirage_mode = \"prod\""), None).unwrap();
        assert!(
            c.prod_mode
                && c.servers[MIRAGE].auto_connect == Some(false)
                && c.servers[MIRAGE_PROD].auto_connect == Some(true)
        );
        let c = McpConfig::from_toml(
            Some("mirage_mode = \"prod\"\n[servers.mirage-prod]\nauto_connect = false"),
            None,
        )
        .unwrap();
        assert_eq!(c.servers[MIRAGE_PROD].auto_connect, Some(false));
        let c = McpConfig::from_toml(Some("[servers.mirage]\nauto_connect = false"), None).unwrap();
        assert_eq!(c.servers[MIRAGE].auto_connect, Some(false));
        assert!(
            McpConfig::from_toml(Some("[servers.mirage-prod]\ncommand = \"x\""), None).is_err()
        );
        let m = McpManager::with_config(McpConfig::from_toml(None, Some("prod".into())).unwrap());
        assert_eq!(m.resolve_name(MIRAGE), MIRAGE_PROD);
        assert_eq!(m.autoconnect_servers(), vec![MIRAGE_PROD.to_string()]);
        let c = McpConfig::from_toml(None, Some("prod".into())).unwrap();
        assert!(c.prod_mode);
        assert!(McpConfig::from_toml(Some("mirage_mode = \"x\""), None).is_err());
    }

    #[test]
    fn user_override_keeps_prod_floor() {
        let t = "[servers.mirage-prod]\nurl = \"http://evil.example/mcp\"\nrequire_https = false\n";
        let c = McpConfig::from_toml(Some(t), None).unwrap();
        let p = &c.servers[MIRAGE_PROD];
        assert!(p.require_https && p.require_auth);
        assert!(validate_http_url(p.url.as_ref().unwrap(), true, p.require_https).is_err());
    }

    #[test]
    fn example_dev_to_prod_no_double_connect() {
        let prod = EXAMPLE_CONFIG.replace("mirage_mode = \"dev\"", "mirage_mode = \"prod\"");
        let m = McpManager::with_config(McpConfig::from_toml(Some(&prod), None).unwrap());
        assert_eq!(m.autoconnect_servers(), vec![MIRAGE_PROD.to_string()]);
        let off = format!("{prod}\n[servers.mirage-prod]\nauto_connect = false\n");
        let m = McpManager::with_config(McpConfig::from_toml(Some(&off), None).unwrap());
        assert!(
            m.autoconnect_servers().is_empty(),
            "{:?}",
            m.autoconnect_servers()
        );
        let m = McpManager::with_config(McpConfig::from_toml(Some(EXAMPLE_CONFIG), None).unwrap());
        assert_eq!(m.autoconnect_servers(), vec![MIRAGE.to_string()]);
    }

    #[test]
    fn config_errors_hide_values_and_stdio_not_autospawned() {
        let e = McpConfig::from_toml(Some("[servers.x]\nauto_connect = \"sk-SECRET\""), None)
            .unwrap_err();
        assert!(!e.contains("SECRET") && e.contains("line 2"), "{e}");
        let e = McpConfig::from_toml(None, Some("sk-SECRET".into())).unwrap_err();
        assert!(!e.contains("SECRET"));
        let c = McpConfig::from_toml(Some("[servers.mirage]\nurl = \"\"\ncommand = \"x\""), None);
        assert!(c.is_err()); // url + command
        let c = McpConfig::from_toml(Some("[servers.files]\ncommand = \"x\""), None).unwrap();
        assert_eq!(c.servers["files"].auto_connect, Some(false));
        let ex = McpConfig::from_toml(Some(EXAMPLE_CONFIG), None).unwrap();
        assert!(ex.servers[MIRAGE].token_file.is_none());
        let c = McpConfig::from_toml(Some("[servers.mirage]\ncommand = \"x\""), None).unwrap();
        assert_eq!(c.servers[MIRAGE].auto_connect, Some(false));
    }

    #[test]
    fn example_config_parses() {
        let c = McpConfig::from_toml(Some(EXAMPLE_CONFIG), None).unwrap();
        assert!(c.servers.contains_key(MIRAGE_PROD));
    }

    #[test]
    fn rejects_bad_config() {
        assert!(McpConfig::from_toml(Some("[servers.x]\nbogus = 1"), None).is_err());
        assert!(
            McpConfig::from_toml(Some("[servers.x]\nurl=\"http://a\"\ncommand=\"b\""), None)
                .is_err()
        );
    }

    #[test]
    fn url_security() {
        assert!(validate_http_url("https://host/mcp", true, true).is_ok());
        assert!(validate_http_url("http://127.0.0.1:7345/mcp", true, false).is_ok());
        assert!(validate_http_url("http://localhost/mcp", true, false).is_ok());
        assert!(validate_http_url("http://10.0.0.2/mcp", true, false).is_err());
        assert!(validate_http_url("http://example.com/mcp", true, false).is_err());
        assert!(validate_http_url("http://example.com/mcp", false, false).is_ok());
        assert!(validate_http_url("http://127.0.0.1/mcp", false, true).is_err());
        assert!(validate_http_url("https://u:p@host/mcp", false, false).is_err());
        assert!(validate_http_url("https://host/mcp?key=1", true, false).is_err());
        assert!(validate_http_url("ftp://host/mcp", false, false).is_err());
    }

    #[test]
    fn token_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("tok");
        std::fs::write(&f, "abc123\n").unwrap();
        let cfg = ServerConfig {
            token_file: Some(f.clone()),
            ..Default::default()
        };
        assert_eq!(resolve_token(&cfg).unwrap().as_deref(), Some("abc123"));
        std::fs::write(&f, "a b").unwrap();
        let e = resolve_token(&cfg).unwrap_err();
        assert!(!e.contains("a b"));
        let cfg = ServerConfig {
            token_env: Some("HELIX_MCP_TEST_UNSET_VAR".into()),
            require_auth: true,
            ..Default::default()
        };
        assert!(resolve_token(&cfg)
            .unwrap_err()
            .contains("HELIX_MCP_TEST_UNSET_VAR"));
        let cfg = ServerConfig::default();
        assert_eq!(resolve_token(&cfg).unwrap(), None);
    }

    #[test]
    fn debug_and_scrub_hide_secrets() {
        let cfg = ServerConfig {
            env: HashMap::from([("K".into(), "s3cret".into())]),
            ..Default::default()
        };
        assert!(!format!("{cfg:?}").contains("s3cret"));
        assert_eq!(scrub("bad s3cret here".into(), &["s3cret"]), "bad *** here");
        let long = "é".repeat(600);
        assert_eq!(scrub(long, &[]).chars().count(), 501);
        let cfg = ServerConfig {
            url: Some("https://u:pw@h/mcp?k=v".into()),
            ..Default::default()
        };
        let d = format!("{cfg:?}");
        assert!(!d.contains("pw") && !d.contains("k=v"), "{d}");
        assert!(!generic_error("x", "secretbody 401").contains("secretbody"));
    }

    fn test_manager(url: String, token_file: Option<PathBuf>) -> McpManager {
        let mut c = McpConfig::from_toml(None, None).unwrap();
        c.servers.insert(
            "t".into(),
            ServerConfig {
                url: Some(url),
                token_file,
                timeout_secs: Some(3),
                ..Default::default()
            },
        );
        McpManager::with_config(c)
    }

    #[tokio::test]
    async fn prod_requires_key() {
        // Deterministic: fake unroutable host, token env guaranteed unset, no real creds.
        let mut c = McpConfig::from_toml(None, None).unwrap();
        let p = c.servers.get_mut(MIRAGE_PROD).unwrap();
        p.url = Some("https://mirage.invalid/mcp".into());
        p.token_env = Some("HELIX_MCP_TEST_UNSET_KEY_7f3a".into());
        let m = McpManager::with_config(c);
        assert!(m.connect(MIRAGE_PROD).await.is_err());
        assert!(!m.status().iter().any(|s| s.connected));
    }

    #[tokio::test]
    async fn redirect_is_refused_and_token_not_leaked() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("tok");
        std::fs::write(&f, "supersecrettoken").unwrap();
        let m = test_manager(format!("http://{addr}/mcp"), Some(f));
        let err = m.connect("t").await.unwrap_err();
        assert!(!err.contains("supersecrettoken"), "{err}");
        assert!(hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        let st = m.status().into_iter().find(|s| s.name == "t").unwrap();
        assert!(!st.connected && st.detail.starts_with("error"));
        assert!(m
            .call_tool("t", "x", serde_json::Value::Null)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn unreachable_server_errors() {
        let m = test_manager("http://127.0.0.1:1/mcp".into(), None);
        assert!(m.connect("t").await.is_err());
        assert!(m.disconnect("t").await.is_err());
        assert!(m.call_tool("t", "x", serde_json::json!([1])).await.is_err());
    }

    /// Minimal Streamable HTTP MCP server (JSON responses) for lifecycle tests.
    async fn mock_server(token: &'static str) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let log = log2.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    loop {
                        // read one request (headers + content-length body)
                        let mut tmp = [0u8; 8192];
                        let (head_end, clen) = loop {
                            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                                let clen = head
                                    .lines()
                                    .find_map(|l| {
                                        l.strip_prefix("content-length:")
                                            .map(|v| v.trim().parse::<usize>().unwrap())
                                    })
                                    .unwrap_or(0);
                                break (i + 4, clen);
                            }
                            match s.read(&mut tmp).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                            }
                        };
                        while buf.len() < head_end + clen {
                            match s.read(&mut tmp).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                            }
                        }
                        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                        let body =
                            String::from_utf8_lossy(&buf[head_end..head_end + clen]).to_string();
                        buf.drain(..head_end + clen);
                        let method_line = head.lines().next().unwrap_or("").to_string();
                        let authed = head.lines().any(|l| {
                            l.eq_ignore_ascii_case(&format!("authorization: Bearer {token}"))
                        });
                        let resp: (u16, String) = if !authed {
                            (401, String::new())
                        } else if method_line.starts_with("GET") {
                            (405, String::new())
                        } else if method_line.starts_with("DELETE") {
                            log.lock().push("DELETE".into());
                            (200, String::new())
                        } else {
                            let v: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            let m = v["method"].as_str().unwrap_or("").to_string();
                            log.lock().push(m.clone());
                            let id = v["id"].clone();
                            let result = match m.as_str() {
                                "initialize" => Some(serde_json::json!({
                                    "protocolVersion": v["params"]["protocolVersion"],
                                    "capabilities": {"tools": {}},
                                    "serverInfo": {"name": "mock", "version": "1.0"}
                                })),
                                "tools/list" => Some(serde_json::json!({"tools": [
                                    {"name": "echo", "description": "Echo", "inputSchema": {"type": "object"}}
                                ]})),
                                "tools/call" => Some(serde_json::json!({
                                    "content": [{"type": "text", "text": format!("hi {}", v["params"]["arguments"]["who"].as_str().unwrap_or(""))}],
                                    "isError": false
                                })),
                                _ => None,
                            };
                            match result {
                                Some(r) => (
                                    200,
                                    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": r})
                                        .to_string(),
                                ),
                                None => (202, String::new()),
                            }
                        };
                        let ctype = if resp.1.is_empty() {
                            ""
                        } else {
                            "Content-Type: application/json\r\n"
                        };
                        let out = format!(
                            "HTTP/1.1 {} X\r\nMcp-Session-Id: sess1\r\n{}Content-Length: {}\r\n\r\n{}",
                            resp.0, ctype, resp.1.len(), resp.1
                        );
                        if s.write_all(out.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (addr, log)
    }

    #[tokio::test]
    async fn http_lifecycle_connect_list_call_disconnect() {
        let (addr, log) = mock_server("goodtoken123").await;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("tok");
        std::fs::write(&f, "goodtoken123\n").unwrap();
        let m = test_manager(format!("http://{addr}/mcp"), Some(f));
        let summary = m.connect("t").await.unwrap();
        assert!(summary.contains("mock"), "{summary}");
        assert!(m.is_connected("t"));
        let tools = m.list_tools("t").await.unwrap();
        assert_eq!(tools[0].name, "echo");
        let out = m
            .call_tool("t", "echo", serde_json::json!({"who": "helix"}))
            .await
            .unwrap();
        assert_eq!(out, "hi helix");
        // Reconnect replaces the session rather than leaking it.
        m.connect("t").await.unwrap();
        m.disconnect("t").await.unwrap();
        assert!(!m.is_connected("t"));
        assert!(m.list_tools("t").await.is_err());
        let log = log.lock().clone();
        assert!(
            log.iter().any(|x| x == "initialize") && log.iter().any(|x| x == "tools/call"),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn http_wrong_token_rejected() {
        let (addr, _) = mock_server("goodtoken123").await;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("tok");
        std::fs::write(&f, "badtoken9999").unwrap();
        let m = test_manager(format!("http://{addr}/mcp"), Some(f));
        let e = m.connect("t").await.unwrap_err();
        assert!(!e.contains("badtoken9999"), "{e}");
        assert!(!m.is_connected("t"));
    }
}
