//! `hive.config.toml` loading — ported from `HiveConfig.swift`.
//!
//! Swift hand-rolled a ~1,100-line TOML parser (no TOML dependency was
//! available). Rust uses the `toml` crate, so this is a thin serde layer: a
//! `Raw*` mirror of the on-disk schema plus a conversion into clean domain
//! types (notably `RuntimeTarget`). Provider/location/scope strings are parsed
//! leniently to match the Swift loader's accepted spellings.

use serde::Deserialize;
use thiserror::Error;

use crate::policy::{PermissionPolicy, PermissionScope};
use crate::runtime::{ModelProviderKind, RuntimeCapabilities, RuntimeLocation, RuntimeTarget};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to parse hive.config.toml: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unknown provider: {0}")]
    InvalidProvider(String),
    #[error("unknown runtime location/kind: {0}")]
    InvalidLocation(String),
    #[error("unknown permission scope: {0}")]
    InvalidScope(String),
    #[error("unknown transport kind: {0}")]
    InvalidTransportKind(String),
}

// ---------------------------------------------------------------------------
// Domain config (clean types returned to callers)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct HiveConfig {
    pub app: AppConfig,
    pub transport: TransportConfig,
    pub sync: SyncConfig,
    pub permissions: PermissionsConfig,
    pub runtimes: Vec<RuntimeTarget>,
    pub chat_defaults: ChatDefaultsConfig,
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppConfig {
    pub name: String,
    pub local_mode: bool,
    pub sync_mode: String,
    pub default_runtime: String,
    pub default_model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Local,
    Relay,
    Lan,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransportConfig {
    pub kind: TransportKind,
    pub relay_endpoint: Option<String>,
    pub relay_account_token_env: Option<String>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            kind: TransportKind::Local,
            relay_endpoint: None,
            relay_account_token_env: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncConfig {
    pub enabled: bool,
    pub server: String,
    pub device_name: String,
    pub end_to_end_encryption: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionsConfig {
    pub default_policy: PermissionScope,
    pub allow_network: bool,
    pub presets: std::collections::BTreeMap<String, PermissionPolicy>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            default_policy: PermissionScope::AlwaysAsk,
            allow_network: true,
            presets: Default::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatDefaultsConfig {
    pub permission_preset: String,
    pub retrieval_mode: String,
    pub runtime_pool_id: Option<String>,
    pub show_context_panel: bool,
    pub show_activity_panel: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerTransportKind {
    Stdio,
    Http,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpServerConfig {
    pub id: String,
    pub transport: McpServerTransportKind,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// Raw on-disk schema (mirrors hive.config.toml keys directly)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    app: Option<RawApp>,
    transport: Option<RawTransport>,
    sync: Option<RawSync>,
    permissions: Option<RawPermissions>,
    #[serde(default)]
    runtimes: Vec<RawRuntime>,
    chat_defaults: Option<RawChatDefaults>,
    #[serde(default)]
    mcp_servers: Vec<RawMcpServer>,
}

#[derive(Debug, Default, Deserialize)]
struct RawApp {
    #[serde(default)]
    name: String,
    #[serde(default)]
    local_mode: bool,
    #[serde(default)]
    sync_mode: String,
    #[serde(default)]
    default_runtime: String,
    #[serde(default)]
    default_model: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawTransport {
    #[serde(default)]
    kind: Option<String>,
    relay: Option<RawRelay>,
}

#[derive(Debug, Default, Deserialize)]
struct RawRelay {
    endpoint: Option<String>,
    account_token_env: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSync {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    server: String,
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    end_to_end_encryption: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RawPermissions {
    default_policy: Option<String>,
    #[serde(default)]
    allow_network: bool,
    #[serde(default)]
    presets: std::collections::BTreeMap<String, RawPermissionPreset>,
}

#[derive(Debug, Default, Deserialize)]
struct RawPermissionPreset {
    #[serde(default)]
    read_files: bool,
    #[serde(default)]
    write_files: bool,
    #[serde(default)]
    run_commands: bool,
    #[serde(default)]
    access_vaults: bool,
    #[serde(default)]
    access_remote_runtime: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RawRuntime {
    id: String,
    #[serde(default)]
    name: String,
    provider: String,
    #[serde(default = "default_kind_remote")]
    kind: String,
    #[serde(default)]
    endpoint: String,
    metrics_endpoint: Option<String>,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    preferred_model: String,
    model_provider_id: Option<String>,
    model_base_url: Option<String>,
    keep_alive: Option<String>,
    #[serde(default)]
    supports_embeddings: bool,
    #[serde(default)]
    supports_tools: bool,
    context_window: Option<u32>,
    #[serde(default)]
    performance_score: f64,
    #[serde(default)]
    cost_per_1m_input_tokens_usd: f64,
}

fn default_kind_remote() -> String {
    "remote".to_string()
}

#[derive(Debug, Default, Deserialize)]
struct RawChatDefaults {
    #[serde(default)]
    permission_preset: String,
    #[serde(default)]
    retrieval_mode: String,
    runtime_pool: Option<String>,
    #[serde(default)]
    show_context_panel: bool,
    #[serde(default)]
    show_activity_panel: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RawMcpServer {
    id: String,
    #[serde(default = "default_transport_stdio")]
    transport: String,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    url: Option<String>,
    /// MCP servers are inert until explicitly enabled — default `false`.
    #[serde(default)]
    enabled: bool,
}

fn default_transport_stdio() -> String {
    "stdio".to_string()
}

// ---------------------------------------------------------------------------
// Lenient enum parsing (matches the Swift loader's accepted spellings)
// ---------------------------------------------------------------------------

fn parse_provider(s: &str) -> Result<ModelProviderKind, ConfigError> {
    match s.to_lowercase().replace(['_', '-'], "").as_str() {
        "ollama" => Ok(ModelProviderKind::Ollama),
        "openai" => Ok(ModelProviderKind::OpenAI),
        "anthropic" => Ok(ModelProviderKind::Anthropic),
        "openrouter" => Ok(ModelProviderKind::OpenRouter),
        "custom" => Ok(ModelProviderKind::Custom),
        "hivedaemon" => Ok(ModelProviderKind::HiveDaemon),
        "aider" => Ok(ModelProviderKind::Aider),
        "pi" => Ok(ModelProviderKind::Pi),
        "claudecode" => Ok(ModelProviderKind::ClaudeCode),
        _ => Err(ConfigError::InvalidProvider(s.to_string())),
    }
}

fn parse_location(s: &str) -> Result<RuntimeLocation, ConfigError> {
    match s.to_lowercase().as_str() {
        "local" => Ok(RuntimeLocation::Local),
        "remote" => Ok(RuntimeLocation::Remote),
        _ => Err(ConfigError::InvalidLocation(s.to_string())),
    }
}

fn parse_scope(s: &str) -> Result<PermissionScope, ConfigError> {
    match s.to_lowercase().replace(['_', '-'], "").as_str() {
        "alwaysask" => Ok(PermissionScope::AlwaysAsk),
        "oneaction" => Ok(PermissionScope::OneAction),
        "chat" => Ok(PermissionScope::Chat),
        "workspace" => Ok(PermissionScope::Workspace),
        _ => Err(ConfigError::InvalidScope(s.to_string())),
    }
}

fn parse_transport_kind(s: &str) -> Result<TransportKind, ConfigError> {
    match s.to_lowercase().as_str() {
        "local" => Ok(TransportKind::Local),
        "relay" => Ok(TransportKind::Relay),
        "lan" => Ok(TransportKind::Lan),
        _ => Err(ConfigError::InvalidTransportKind(s.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Parse a `hive.config.toml` from its text contents.
pub fn load_from_str(contents: &str) -> Result<HiveConfig, ConfigError> {
    let raw: RawConfig = toml::from_str(contents)?;
    raw.into_config()
}

impl RawConfig {
    fn into_config(self) -> Result<HiveConfig, ConfigError> {
        let app = self
            .app
            .map(|a| AppConfig {
                name: a.name,
                local_mode: a.local_mode,
                sync_mode: a.sync_mode,
                default_runtime: a.default_runtime,
                default_model: a.default_model,
            })
            .unwrap_or_default();

        let transport = match self.transport {
            Some(t) => {
                let kind = match t.kind {
                    Some(k) => parse_transport_kind(&k)?,
                    None => TransportKind::Local,
                };
                let (relay_endpoint, relay_account_token_env) = t
                    .relay
                    .map(|r| (r.endpoint, r.account_token_env))
                    .unwrap_or((None, None));
                TransportConfig {
                    kind,
                    relay_endpoint,
                    relay_account_token_env,
                }
            }
            None => TransportConfig::default(),
        };

        let sync = self
            .sync
            .map(|s| SyncConfig {
                enabled: s.enabled,
                server: s.server,
                device_name: s.device_name,
                end_to_end_encryption: s.end_to_end_encryption,
            })
            .unwrap_or_default();

        let permissions = match self.permissions {
            Some(p) => {
                let default_policy = match p.default_policy {
                    Some(s) => parse_scope(&s)?,
                    None => PermissionScope::AlwaysAsk,
                };
                let mut presets = std::collections::BTreeMap::new();
                for (name, preset) in p.presets {
                    presets.insert(
                        name,
                        PermissionPolicy {
                            read_files: preset.read_files,
                            write_files: preset.write_files,
                            run_commands: preset.run_commands,
                            access_vaults: preset.access_vaults,
                            access_network: p.allow_network,
                            access_remote_runtime: preset.access_remote_runtime,
                            scope: default_policy,
                        },
                    );
                }
                PermissionsConfig {
                    default_policy,
                    allow_network: p.allow_network,
                    presets,
                }
            }
            None => PermissionsConfig::default(),
        };

        let mut runtimes = Vec::with_capacity(self.runtimes.len());
        for r in self.runtimes {
            runtimes.push(r.into_target()?);
        }

        let chat_defaults = self
            .chat_defaults
            .map(|c| ChatDefaultsConfig {
                permission_preset: c.permission_preset,
                retrieval_mode: c.retrieval_mode,
                runtime_pool_id: c.runtime_pool,
                show_context_panel: c.show_context_panel,
                show_activity_panel: c.show_activity_panel,
            })
            .unwrap_or_default();

        let mcp_servers = self
            .mcp_servers
            .into_iter()
            .map(|m| McpServerConfig {
                id: m.id,
                transport: if m.transport.eq_ignore_ascii_case("http") {
                    McpServerTransportKind::Http
                } else {
                    McpServerTransportKind::Stdio
                },
                command: m.command,
                args: m.args,
                url: m.url,
                enabled: m.enabled,
            })
            .collect();

        Ok(HiveConfig {
            app,
            transport,
            sync,
            permissions,
            runtimes,
            chat_defaults,
            mcp_servers,
        })
    }
}

impl RawRuntime {
    fn into_target(self) -> Result<RuntimeTarget, ConfigError> {
        let provider_kind = parse_provider(&self.provider)?;
        let location = parse_location(&self.kind)?;
        let model_id = if !self.preferred_model.is_empty() {
            self.preferred_model.clone()
        } else {
            self.models.first().cloned().unwrap_or_default()
        };
        Ok(RuntimeTarget {
            id: self.id,
            name: self.name,
            provider_kind,
            location,
            model_id,
            available_models: self.models,
            endpoint: self.endpoint,
            metrics_endpoint: self.metrics_endpoint,
            model_provider_id: self.model_provider_id,
            model_base_url: self.model_base_url,
            request_keep_alive: self.keep_alive,
            estimated_performance_score: self.performance_score,
            estimated_cost_per_1m_input_tokens_usd: self.cost_per_1m_input_tokens_usd,
            capabilities: RuntimeCapabilities {
                supports_embeddings: self.supports_embeddings,
                supports_tools: self.supports_tools,
                supports_streaming: true,
                supports_agent_orchestration: false,
                context_window_tokens: self.context_window,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../../../hive.config.toml");

    #[test]
    fn parses_the_repo_sample_config() {
        let cfg = load_from_str(SAMPLE).expect("parse sample");
        assert_eq!(cfg.app.name, "Hive");
        assert_eq!(cfg.app.default_runtime, "primary-runtime");
        assert_eq!(cfg.transport.kind, TransportKind::Local);
        assert!(!cfg.sync.enabled);
        assert!(cfg.sync.end_to_end_encryption);
        assert_eq!(cfg.permissions.default_policy, PermissionScope::AlwaysAsk);
        let default = cfg.permissions.presets.get("default").expect("preset");
        assert!(default.read_files && default.write_files && default.run_commands);

        assert_eq!(cfg.runtimes.len(), 1);
        let rt = &cfg.runtimes[0];
        assert_eq!(rt.id, "primary-runtime");
        assert_eq!(rt.provider_kind, ModelProviderKind::Ollama);
        assert_eq!(rt.location, RuntimeLocation::Remote);
        // preferred_model wins over the models[] list
        assert_eq!(rt.model_id, "qwen3.5:latest");
        assert_eq!(rt.request_keep_alive.as_deref(), Some("-1"));
        assert!(rt.capabilities.supports_tools);
    }

    #[test]
    fn context_window_override_parses() {
        let toml = r#"
[[runtimes]]
id = "local-qwen"
provider = "ollama"
kind = "local"
endpoint = "http://localhost:11434"
preferred_model = "qwen2.5"
context_window = 32768
"#;
        let cfg = load_from_str(toml).unwrap();
        assert_eq!(
            cfg.runtimes[0].capabilities.context_window_tokens,
            Some(32768)
        );
    }

    #[test]
    fn mcp_servers_default_disabled() {
        let toml = r#"
[[mcp_servers]]
id = "fs"
transport = "stdio"
command = "mcp-fs"
args = ["--root", "/tmp"]
"#;
        let cfg = load_from_str(toml).unwrap();
        assert_eq!(cfg.mcp_servers.len(), 1);
        assert_eq!(cfg.mcp_servers[0].transport, McpServerTransportKind::Stdio);
        assert!(
            !cfg.mcp_servers[0].enabled,
            "MCP servers must be inert until explicitly enabled"
        );
    }
}
