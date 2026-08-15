use std::collections::{BTreeMap, HashSet};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Process-lifetime set of already-emitted config deprecation warning strings.
/// Prevents duplicate warnings when `ConfigLoader::load()` is called multiple
/// times within a single CLI invocation. (ROADMAP #698)
static EMITTED_CONFIG_WARNINGS: std::sync::OnceLock<Mutex<HashSet<String>>> =
    std::sync::OnceLock::new();

fn emit_config_warning_once(warning: &str) {
    let set = EMITTED_CONFIG_WARNINGS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = set.lock().unwrap_or_else(|e| e.into_inner());
    if guard.insert(warning.to_string()) {
        eprintln!("warning: {warning}");
    }
}

use crate::json::JsonValue;
use crate::sandbox::{FilesystemIsolationMode, SandboxConfig};

/// Schema name advertised by generated settings files.
pub const CLAW_SETTINGS_SCHEMA_NAME: &str = "SettingsSchema";

/// Origin of a loaded settings file in the configuration precedence chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigSource {
    User,
    Project,
    Local,
}

/// Effective permission mode after decoding config values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedPermissionMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

/// A discovered config file and the scope it contributes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    pub source: ConfigSource,
    pub path: PathBuf,
}

/// Fully merged runtime configuration plus parsed feature-specific views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    merged: BTreeMap<String, JsonValue>,
    loaded_entries: Vec<ConfigEntry>,
    feature_config: RuntimeFeatureConfig,
}

/// Parsed plugin-related settings extracted from runtime config.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimePluginConfig {
    enabled_plugins: BTreeMap<String, bool>,
    external_directories: Vec<String>,
    install_root: Option<String>,
    registry_path: Option<String>,
    bundled_root: Option<String>,
    max_output_tokens: Option<u32>,
}

/// Structured feature configuration consumed by runtime subsystems.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeFeatureConfig {
    hooks: RuntimeHookConfig,
    plugins: RuntimePluginConfig,
    mcp: McpConfigCollection,
    oauth: Option<OAuthConfig>,
    model: Option<String>,
    aliases: BTreeMap<String, String>,
    permission_mode: Option<ResolvedPermissionMode>,
    permission_rules: RuntimePermissionRuleConfig,
    sandbox: SandboxConfig,
    provider_fallbacks: ProviderFallbackConfig,
    trusted_roots: Vec<String>,
    disable_workflows: bool,
    fallback_model: Option<String>,
    worktree_base_ref: Option<String>,
    allow_all_claude_ai_mcps: bool,
    lean_system_prompt_default: bool,
    plugin_suggestion_marketplaces: Vec<String>,
    disallowed_tools: Vec<String>,
    enforce_available_models: bool,
    available_models: Vec<String>,
    language: Option<String>,
    disable_bundled_skills: bool,
    /// v2.1.186: `!` bash commands trigger Claude to respond to the output.
    respond_to_bash_commands: bool,
    /// v2.1.183: omit the session URL from commits/PRs when false.
    attribution_session_url: bool,
    /// v2.1.193: route all Bash/PowerShell through the auto-mode classifier.
    auto_mode_classify_all_shell: bool,
    /// v2.1.202: advisory guideline for dynamic workflow agent counts.
    /// "small"/"medium"/"large" map to suggested chunk sizes.
    workflow_size: Option<String>,
    /// v2.1.212: session-wide cap on WebSearch tool calls (stops runaway
    /// search loops). 0 = unlimited.
    max_web_searches_per_session: u32,
    /// v2.1.212: per-session cap on subagent spawns (stops runaway delegation).
    /// 0 = unlimited.
    max_subagents_per_session: u32,
    /// v2.1.233: opt-in memory cgroup limit for Bash tool commands, in MiB
    /// (Linux only). 0 = disabled.
    tool_memory_limit_mib: u32,
    /// v2.1.233: WebFetch session URL cache TTL in milliseconds
    /// (`CLAUDE_CODE_WEBFETCH_CACHE_TTL_MS`). Default 15 minutes.
    webfetch_cache_ttl_ms: u32,
}

/// Ordered chain of fallback model identifiers used when the primary
/// provider returns a retryable failure (429/500/503/etc.). The chain is
/// strict: each entry is tried in order until one succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderFallbackConfig {
    primary: Option<String>,
    fallbacks: Vec<String>,
}

/// Hook command lists grouped by lifecycle stage.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeHookConfig {
    pre_tool_use: Vec<String>,
    post_tool_use: Vec<String>,
    post_tool_use_failure: Vec<String>,
    notification: Vec<String>,
    stop: Vec<String>,
    teammate_idle: Vec<String>,
    task_created: Vec<String>,
    task_completed: Vec<String>,
    message_display: Vec<String>,
    session_start: Vec<String>,
}

/// Raw permission rule lists grouped by allow, deny, and ask behavior.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimePermissionRuleConfig {
    allow: Vec<String>,
    deny: Vec<String>,
    ask: Vec<String>,
    /// #159: simple tool-name denials parsed from the `deniedTools` config field.
    /// Unlike the `deny` rules (pattern-based), `denied_tools` is a flat list of
    /// tool names that are unconditionally denied regardless of permission mode.
    denied_tools: Vec<String>,
}

/// Collection of configured MCP servers after scope-aware merging.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpConfigCollection {
    servers: BTreeMap<String, ScopedMcpServerConfig>,
}

/// MCP server config paired with the scope that defined it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedMcpServerConfig {
    pub required: bool,
    pub scope: ConfigSource,
    pub config: McpServerConfig,
}

/// Transport families supported by configured MCP servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransport {
    Stdio,
    Sse,
    Http,
    Ws,
    Sdk,
    ManagedProxy,
}

/// Scope-normalized MCP server configuration variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerConfig {
    Stdio(McpStdioServerConfig),
    Sse(McpRemoteServerConfig),
    Http(McpRemoteServerConfig),
    Ws(McpWebSocketServerConfig),
    Sdk(McpSdkServerConfig),
    ManagedProxy(McpManagedProxyServerConfig),
}

/// Configuration for an MCP server launched as a local stdio process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStdioServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub tool_call_timeout_ms: Option<u64>,
}

/// Configuration for an MCP server reached over HTTP or SSE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRemoteServerConfig {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub headers_helper: Option<String>,
    pub oauth: Option<McpOAuthConfig>,
}

/// Configuration for an MCP server reached over WebSocket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpWebSocketServerConfig {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub headers_helper: Option<String>,
}

/// Configuration for an MCP server addressed through an SDK name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSdkServerConfig {
    pub name: String,
}

/// Configuration for an MCP managed-proxy endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpManagedProxyServerConfig {
    pub url: String,
    pub id: String,
}

/// OAuth overrides associated with a remote MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpOAuthConfig {
    pub client_id: Option<String>,
    pub callback_port: Option<u16>,
    pub auth_server_metadata_url: Option<String>,
    pub xaa: Option<bool>,
}

/// OAuth client configuration used by the main Claw runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    pub client_id: String,
    pub authorize_url: String,
    pub token_url: String,
    pub callback_port: Option<u16>,
    pub manual_redirect_url: Option<String>,
    pub scopes: Vec<String>,
}

/// Errors raised while reading or parsing runtime configuration files.
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(String),
}

impl Display for ConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Parse(error) => write!(
                f,
                "{error}\nFix: open the file shown above and correct the JSON syntax, then retry."
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// Discovers config files and merges them into a [`RuntimeConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLoader {
    cwd: PathBuf,
    config_home: PathBuf,
}

impl ConfigLoader {
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>, config_home: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            config_home: config_home.into(),
        }
    }

    #[must_use]
    pub fn default_for(cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        let config_home = default_config_home();
        Self { cwd, config_home }
    }

    #[must_use]
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    #[must_use]
    pub fn discover(&self) -> Vec<ConfigEntry> {
        let user_legacy_path = self.config_home.parent().map_or_else(
            || PathBuf::from(".claw.json"),
            |parent| parent.join(".claw.json"),
        );
        vec![
            ConfigEntry {
                source: ConfigSource::User,
                path: user_legacy_path,
            },
            ConfigEntry {
                source: ConfigSource::User,
                path: self.config_home.join("settings.json"),
            },
            ConfigEntry {
                source: ConfigSource::Project,
                path: self.cwd.join(".claw.json"),
            },
            ConfigEntry {
                source: ConfigSource::Project,
                path: self.cwd.join(".claw").join("settings.json"),
            },
            ConfigEntry {
                source: ConfigSource::Local,
                path: self.cwd.join(".claw").join("settings.local.json"),
            },
        ]
    }

    pub fn load(&self) -> Result<RuntimeConfig, ConfigError> {
        let mut merged = BTreeMap::new();
        let mut loaded_entries = Vec::new();
        let mut mcp_servers = BTreeMap::new();
        let mut all_warnings = Vec::new();

        for entry in self.discover() {
            crate::config_validate::check_unsupported_format(&entry.path)?;
            let Some(parsed) = read_optional_json_object(&entry.path)? else {
                continue;
            };
            let validation = crate::config_validate::validate_config_file(
                &parsed.object,
                &parsed.source,
                &entry.path,
            );
            if !validation.is_ok() {
                let first_error = &validation.errors[0];
                return Err(ConfigError::Parse(first_error.to_string()));
            }
            all_warnings.extend(validation.warnings);
            validate_optional_hooks_config(&parsed.object, &entry.path)?;
            merge_mcp_servers(&mut mcp_servers, entry.source, &parsed.object, &entry.path)?;
            deep_merge_objects(&mut merged, &parsed.object);
            loaded_entries.push(entry);
        }

        for warning in &all_warnings {
            emit_config_warning_once(&warning.to_string());
        }

        let merged_value = JsonValue::Object(merged.clone());

        let feature_config = RuntimeFeatureConfig {
            hooks: parse_optional_hooks_config(&merged_value)?,
            plugins: parse_optional_plugin_config(&merged_value)?,
            mcp: McpConfigCollection {
                servers: mcp_servers,
            },
            oauth: parse_optional_oauth_config(&merged_value, "merged settings.oauth")?,
            model: parse_optional_model(&merged_value),
            aliases: parse_optional_aliases(&merged_value)?,
            permission_mode: parse_optional_permission_mode(&merged_value)?,
            permission_rules: parse_optional_permission_rules(&merged_value)?,
            sandbox: parse_optional_sandbox_config(&merged_value)?,
            provider_fallbacks: parse_optional_provider_fallbacks(&merged_value)?,
            trusted_roots: parse_optional_trusted_roots(&merged_value)?,
            disable_workflows: parse_optional_bool(&merged_value, "disableWorkflows")
                .unwrap_or(false),
            fallback_model: parse_optional_string_field(&merged_value, "fallbackModel"),
            worktree_base_ref: parse_optional_worktree_base_ref(&merged_value),
            allow_all_claude_ai_mcps: parse_optional_bool(&merged_value, "allowAllClaudeAiMcps")
                .unwrap_or(false),
            lean_system_prompt_default: parse_optional_bool(
                &merged_value,
                "leanSystemPromptDefault",
            )
            .unwrap_or(true),
            plugin_suggestion_marketplaces: parse_optional_string_vec(
                &merged_value,
                "pluginSuggestionMarketplaces",
            )
            .unwrap_or_default(),
            disallowed_tools: parse_optional_disallowed_tools(&merged_value),
            enforce_available_models: parse_optional_bool(&merged_value, "enforceAvailableModels")
                .unwrap_or(false),
            available_models: parse_optional_string_vec(&merged_value, "availableModels")
                .unwrap_or_default(),
            language: parse_optional_string_field(&merged_value, "language"),
            disable_bundled_skills: parse_optional_bool(&merged_value, "disableBundledSkills")
                .unwrap_or(false)
                || std::env::var("CLAW_DISABLE_BUNDLED_SKILLS").as_deref() == Ok("1")
                || std::env::var("CLAUDE_CODE_DISABLE_BUNDLED_SKILLS").as_deref() == Ok("1"),
            respond_to_bash_commands: parse_optional_bool(&merged_value, "respondToBashCommands")
                .unwrap_or(true),
            attribution_session_url: nested_bool(&merged_value, &["attribution", "sessionUrl"])
                .unwrap_or(true),
            auto_mode_classify_all_shell: nested_bool(
                &merged_value,
                &["autoMode", "classifyAllShell"],
            )
            .unwrap_or(false),
            workflow_size: nested_string(&merged_value, &["workflow", "size"]).map(str::to_string),
            max_web_searches_per_session: session_cap_from_env(
                "CLAUDE_CODE_MAX_WEB_SEARCHES_PER_SESSION",
                200,
            ),
            max_subagents_per_session: session_cap_from_env(
                "CLAUDE_CODE_MAX_SUBAGENTS_PER_SESSION",
                200,
            ),
            // v2.1.233: opt-in Bash memory cgroup limit (MiB, Linux); 0 = off.
            tool_memory_limit_mib: session_cap_from_env("CLAUDE_CODE_TOOL_MEMORY_LIMIT", 0),
            // v2.1.233: WebFetch session URL cache TTL (ms); default 15 min.
            webfetch_cache_ttl_ms: session_cap_from_env(
                "CLAUDE_CODE_WEBFETCH_CACHE_TTL_MS",
                15 * 60 * 1000,
            ),
        };

        Ok(RuntimeConfig {
            merged,
            loaded_entries,
            feature_config,
        })
    }

    /// Like [`load`] but also returns the list of validation warnings collected during
    /// loading, without emitting them to stderr. Callers that want to surface warnings
    /// through a structured channel (e.g. the JSON config envelope) should use this.
    /// #773: enables JSON-mode callers to include `warnings` in their output envelope
    /// instead of receiving unstructured text on stderr.
    pub fn load_collecting_warnings(&self) -> Result<(RuntimeConfig, Vec<String>), ConfigError> {
        let mut merged = BTreeMap::new();
        let mut loaded_entries = Vec::new();
        let mut mcp_servers = BTreeMap::new();
        let mut all_warnings: Vec<String> = Vec::new();

        for entry in self.discover() {
            crate::config_validate::check_unsupported_format(&entry.path)?;
            let Some(parsed) = read_optional_json_object(&entry.path)? else {
                continue;
            };
            let validation = crate::config_validate::validate_config_file(
                &parsed.object,
                &parsed.source,
                &entry.path,
            );
            if !validation.is_ok() {
                let first_error = &validation.errors[0];
                return Err(ConfigError::Parse(first_error.to_string()));
            }
            all_warnings.extend(validation.warnings.iter().map(|w| w.to_string()));
            validate_optional_hooks_config(&parsed.object, &entry.path)?;
            merge_mcp_servers(&mut mcp_servers, entry.source, &parsed.object, &entry.path)?;
            deep_merge_objects(&mut merged, &parsed.object);
            loaded_entries.push(entry);
        }

        let merged_value = JsonValue::Object(merged.clone());

        let feature_config = RuntimeFeatureConfig {
            hooks: parse_optional_hooks_config(&merged_value)?,
            plugins: parse_optional_plugin_config(&merged_value)?,
            mcp: McpConfigCollection {
                servers: mcp_servers,
            },
            oauth: parse_optional_oauth_config(&merged_value, "merged settings.oauth")?,
            model: parse_optional_model(&merged_value),
            aliases: parse_optional_aliases(&merged_value)?,
            permission_mode: parse_optional_permission_mode(&merged_value)?,
            permission_rules: parse_optional_permission_rules(&merged_value)?,
            sandbox: parse_optional_sandbox_config(&merged_value)?,
            provider_fallbacks: parse_optional_provider_fallbacks(&merged_value)?,
            trusted_roots: parse_optional_trusted_roots(&merged_value)?,
            disable_workflows: parse_optional_bool(&merged_value, "disableWorkflows")
                .unwrap_or(false),
            fallback_model: parse_optional_string_field(&merged_value, "fallbackModel"),
            worktree_base_ref: parse_optional_worktree_base_ref(&merged_value),
            allow_all_claude_ai_mcps: parse_optional_bool(&merged_value, "allowAllClaudeAiMcps")
                .unwrap_or(false),
            lean_system_prompt_default: parse_optional_bool(
                &merged_value,
                "leanSystemPromptDefault",
            )
            .unwrap_or(true),
            plugin_suggestion_marketplaces: parse_optional_string_vec(
                &merged_value,
                "pluginSuggestionMarketplaces",
            )
            .unwrap_or_default(),
            disallowed_tools: parse_optional_disallowed_tools(&merged_value),
            enforce_available_models: parse_optional_bool(&merged_value, "enforceAvailableModels")
                .unwrap_or(false),
            available_models: parse_optional_string_vec(&merged_value, "availableModels")
                .unwrap_or_default(),
            language: parse_optional_string_field(&merged_value, "language"),
            disable_bundled_skills: parse_optional_bool(&merged_value, "disableBundledSkills")
                .unwrap_or(false)
                || std::env::var("CLAW_DISABLE_BUNDLED_SKILLS").as_deref() == Ok("1")
                || std::env::var("CLAUDE_CODE_DISABLE_BUNDLED_SKILLS").as_deref() == Ok("1"),
            respond_to_bash_commands: parse_optional_bool(&merged_value, "respondToBashCommands")
                .unwrap_or(true),
            attribution_session_url: nested_bool(&merged_value, &["attribution", "sessionUrl"])
                .unwrap_or(true),
            auto_mode_classify_all_shell: nested_bool(
                &merged_value,
                &["autoMode", "classifyAllShell"],
            )
            .unwrap_or(false),
            workflow_size: nested_string(&merged_value, &["workflow", "size"]).map(str::to_string),
            max_web_searches_per_session: session_cap_from_env(
                "CLAUDE_CODE_MAX_WEB_SEARCHES_PER_SESSION",
                200,
            ),
            max_subagents_per_session: session_cap_from_env(
                "CLAUDE_CODE_MAX_SUBAGENTS_PER_SESSION",
                200,
            ),
            // v2.1.233: opt-in Bash memory cgroup limit (MiB, Linux); 0 = off.
            tool_memory_limit_mib: session_cap_from_env("CLAUDE_CODE_TOOL_MEMORY_LIMIT", 0),
            // v2.1.233: WebFetch session URL cache TTL (ms); default 15 min.
            webfetch_cache_ttl_ms: session_cap_from_env(
                "CLAUDE_CODE_WEBFETCH_CACHE_TTL_MS",
                15 * 60 * 1000,
            ),
        };

        let config = RuntimeConfig {
            merged,
            loaded_entries,
            feature_config,
        };
        Ok((config, all_warnings))
    }
}

impl RuntimeConfig {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            merged: BTreeMap::new(),
            loaded_entries: Vec::new(),
            feature_config: RuntimeFeatureConfig::default(),
        }
    }

    #[must_use]
    pub fn merged(&self) -> &BTreeMap<String, JsonValue> {
        &self.merged
    }

    #[must_use]
    pub fn loaded_entries(&self) -> &[ConfigEntry] {
        &self.loaded_entries
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.merged.get(key)
    }

    #[must_use]
    pub fn as_json(&self) -> JsonValue {
        JsonValue::Object(self.merged.clone())
    }

    #[must_use]
    pub fn feature_config(&self) -> &RuntimeFeatureConfig {
        &self.feature_config
    }

    #[must_use]
    pub fn mcp(&self) -> &McpConfigCollection {
        &self.feature_config.mcp
    }

    #[must_use]
    pub fn hooks(&self) -> &RuntimeHookConfig {
        &self.feature_config.hooks
    }

    #[must_use]
    pub fn plugins(&self) -> &RuntimePluginConfig {
        &self.feature_config.plugins
    }

    #[must_use]
    pub fn oauth(&self) -> Option<&OAuthConfig> {
        self.feature_config.oauth.as_ref()
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.feature_config.model.as_deref()
    }

    #[must_use]
    pub fn aliases(&self) -> &BTreeMap<String, String> {
        &self.feature_config.aliases
    }

    #[must_use]
    pub fn permission_mode(&self) -> Option<ResolvedPermissionMode> {
        self.feature_config.permission_mode
    }

    #[must_use]
    pub fn permission_rules(&self) -> &RuntimePermissionRuleConfig {
        &self.feature_config.permission_rules
    }

    #[must_use]
    pub fn sandbox(&self) -> &SandboxConfig {
        &self.feature_config.sandbox
    }

    #[must_use]
    pub fn provider_fallbacks(&self) -> &ProviderFallbackConfig {
        &self.feature_config.provider_fallbacks
    }

    #[must_use]
    pub fn trusted_roots(&self) -> &[String] {
        &self.feature_config.trusted_roots
    }

    #[must_use]
    pub fn fallback_model(&self) -> Option<&str> {
        self.feature_config.fallback_model()
    }

    #[must_use]
    pub fn worktree_base_ref(&self) -> Option<&str> {
        self.feature_config.worktree_base_ref()
    }

    #[must_use]
    pub fn allow_all_claude_ai_mcps(&self) -> bool {
        self.feature_config.allow_all_claude_ai_mcps()
    }

    #[must_use]
    pub fn lean_system_prompt_default(&self) -> bool {
        self.feature_config.lean_system_prompt_default()
    }

    #[must_use]
    pub fn plugin_suggestion_marketplaces(&self) -> &[String] {
        self.feature_config.plugin_suggestion_marketplaces()
    }

    #[must_use]
    pub fn disallowed_tools(&self) -> &[String] {
        self.feature_config.disallowed_tools()
    }

    /// v2.1.175
    #[must_use]
    pub fn enforce_available_models(&self) -> bool {
        self.feature_config.enforce_available_models()
    }

    /// v2.1.175/176
    #[must_use]
    pub fn available_models(&self) -> &[String] {
        self.feature_config.available_models()
    }

    /// v2.1.176
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.feature_config.language()
    }

    /// v2.1.169
    #[must_use]
    pub fn disable_bundled_skills(&self) -> bool {
        self.feature_config.disable_bundled_skills()
    }

    /// v2.1.186
    #[must_use]
    pub fn respond_to_bash_commands(&self) -> bool {
        self.feature_config.respond_to_bash_commands()
    }

    /// v2.1.183
    #[must_use]
    pub fn attribution_session_url(&self) -> bool {
        self.feature_config.attribution_session_url()
    }

    /// v2.1.193
    #[must_use]
    pub fn auto_mode_classify_all_shell(&self) -> bool {
        self.feature_config.auto_mode_classify_all_shell()
    }

    /// v2.1.202: advisory workflow size ("small"/"medium"/"large").
    #[must_use]
    pub fn workflow_size(&self) -> Option<&str> {
        self.feature_config.workflow_size()
    }

    /// v2.1.212: session WebSearch cap (0 = unlimited).
    #[must_use]
    pub fn max_web_searches_per_session(&self) -> u32 {
        self.feature_config.max_web_searches_per_session()
    }

    /// v2.1.212: per-session subagent spawn cap (0 = unlimited).
    #[must_use]
    pub fn max_subagents_per_session(&self) -> u32 {
        self.feature_config.max_subagents_per_session()
    }

    /// v2.1.233: Bash tool memory cgroup limit in MiB (0 = disabled).
    #[must_use]
    pub fn tool_memory_limit_mib(&self) -> u32 {
        self.feature_config.tool_memory_limit_mib()
    }

    /// v2.1.233: WebFetch session URL cache TTL in milliseconds.
    #[must_use]
    pub fn webfetch_cache_ttl_ms(&self) -> u32 {
        self.feature_config.webfetch_cache_ttl_ms()
    }

    /// Merge config-level default trusted roots with per-call roots.
    ///
    /// Config roots are defaults and are kept first; per-call roots extend the
    /// allowlist for a specific worker/session creation request. Duplicates are
    /// removed without reordering the first occurrence so evidence remains
    /// deterministic while avoiding repeated trust checks.
    #[must_use]
    pub fn trusted_roots_with_overrides(&self, per_call_roots: &[String]) -> Vec<String> {
        merge_trusted_roots(self.trusted_roots(), per_call_roots)
    }
}

impl RuntimeFeatureConfig {
    #[must_use]
    pub fn with_hooks(mut self, hooks: RuntimeHookConfig) -> Self {
        self.hooks = hooks;
        self
    }

    #[must_use]
    pub fn with_plugins(mut self, plugins: RuntimePluginConfig) -> Self {
        self.plugins = plugins;
        self
    }

    #[must_use]
    pub fn hooks(&self) -> &RuntimeHookConfig {
        &self.hooks
    }

    #[must_use]
    pub fn plugins(&self) -> &RuntimePluginConfig {
        &self.plugins
    }

    #[must_use]
    pub fn mcp(&self) -> &McpConfigCollection {
        &self.mcp
    }

    #[must_use]
    pub fn oauth(&self) -> Option<&OAuthConfig> {
        self.oauth.as_ref()
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    #[must_use]
    pub fn aliases(&self) -> &BTreeMap<String, String> {
        &self.aliases
    }

    #[must_use]
    pub fn permission_mode(&self) -> Option<ResolvedPermissionMode> {
        self.permission_mode
    }

    #[must_use]
    pub fn permission_rules(&self) -> &RuntimePermissionRuleConfig {
        &self.permission_rules
    }

    #[must_use]
    pub fn sandbox(&self) -> &SandboxConfig {
        &self.sandbox
    }

    #[must_use]
    pub fn provider_fallbacks(&self) -> &ProviderFallbackConfig {
        &self.provider_fallbacks
    }

    #[must_use]
    pub fn trusted_roots(&self) -> &[String] {
        &self.trusted_roots
    }

    #[must_use]
    pub fn disable_workflows(&self) -> bool {
        self.disable_workflows
    }

    #[must_use]
    pub fn fallback_model(&self) -> Option<&str> {
        self.fallback_model.as_deref()
    }

    #[must_use]
    pub fn worktree_base_ref(&self) -> Option<&str> {
        self.worktree_base_ref.as_deref()
    }

    #[must_use]
    pub fn allow_all_claude_ai_mcps(&self) -> bool {
        self.allow_all_claude_ai_mcps
    }

    #[must_use]
    pub fn lean_system_prompt_default(&self) -> bool {
        self.lean_system_prompt_default
    }

    #[must_use]
    pub fn plugin_suggestion_marketplaces(&self) -> &[String] {
        &self.plugin_suggestion_marketplaces
    }

    #[must_use]
    pub fn disallowed_tools(&self) -> &[String] {
        &self.disallowed_tools
    }

    /// v2.1.175: when enabled, `available_models` also constrains the Default model.
    #[must_use]
    pub fn enforce_available_models(&self) -> bool {
        self.enforce_available_models
    }

    /// v2.1.175/176: allowlist of models a session may use.
    #[must_use]
    pub fn available_models(&self) -> &[String] {
        &self.available_models
    }

    /// v2.1.176: language for session titles / UI localization.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// v2.1.169: hide bundled skills/workflows/built-in slash commands from the model.
    #[must_use]
    pub fn disable_bundled_skills(&self) -> bool {
        self.disable_bundled_skills
    }

    /// v2.1.186
    #[must_use]
    pub fn respond_to_bash_commands(&self) -> bool {
        self.respond_to_bash_commands
    }

    /// v2.1.183
    #[must_use]
    pub fn attribution_session_url(&self) -> bool {
        self.attribution_session_url
    }

    /// v2.1.193
    #[must_use]
    pub fn auto_mode_classify_all_shell(&self) -> bool {
        self.auto_mode_classify_all_shell
    }

    /// v2.1.202: advisory workflow size ("small"/"medium"/"large").
    #[must_use]
    pub fn workflow_size(&self) -> Option<&str> {
        self.workflow_size.as_deref()
    }

    /// v2.1.212: session WebSearch cap (0 = unlimited).
    #[must_use]
    pub fn max_web_searches_per_session(&self) -> u32 {
        self.max_web_searches_per_session
    }

    /// v2.1.212: per-session subagent spawn cap (0 = unlimited).
    #[must_use]
    pub fn max_subagents_per_session(&self) -> u32 {
        self.max_subagents_per_session
    }

    /// v2.1.233: Bash tool memory cgroup limit in MiB (0 = disabled).
    #[must_use]
    pub fn tool_memory_limit_mib(&self) -> u32 {
        self.tool_memory_limit_mib
    }

    /// v2.1.233: WebFetch session URL cache TTL in milliseconds.
    #[must_use]
    pub fn webfetch_cache_ttl_ms(&self) -> u32 {
        self.webfetch_cache_ttl_ms
    }

    /// Merge this config's default trusted roots with per-call roots.
    #[must_use]
    pub fn trusted_roots_with_overrides(&self, per_call_roots: &[String]) -> Vec<String> {
        merge_trusted_roots(self.trusted_roots(), per_call_roots)
    }
}

fn merge_trusted_roots(config_roots: &[String], per_call_roots: &[String]) -> Vec<String> {
    let mut merged = Vec::with_capacity(config_roots.len() + per_call_roots.len());
    for root in config_roots.iter().chain(per_call_roots.iter()) {
        if !merged.contains(root) {
            merged.push(root.clone());
        }
    }
    merged
}

impl ProviderFallbackConfig {
    #[must_use]
    pub fn new(primary: Option<String>, fallbacks: Vec<String>) -> Self {
        Self { primary, fallbacks }
    }

    #[must_use]
    pub fn primary(&self) -> Option<&str> {
        self.primary.as_deref()
    }

    #[must_use]
    pub fn fallbacks(&self) -> &[String] {
        &self.fallbacks
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fallbacks.is_empty()
    }
}

impl RuntimePluginConfig {
    #[must_use]
    pub fn enabled_plugins(&self) -> &BTreeMap<String, bool> {
        &self.enabled_plugins
    }

    #[must_use]
    pub fn external_directories(&self) -> &[String] {
        &self.external_directories
    }

    #[must_use]
    pub fn install_root(&self) -> Option<&str> {
        self.install_root.as_deref()
    }

    #[must_use]
    pub fn registry_path(&self) -> Option<&str> {
        self.registry_path.as_deref()
    }

    #[must_use]
    pub fn bundled_root(&self) -> Option<&str> {
        self.bundled_root.as_deref()
    }

    #[must_use]
    pub fn max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
    }

    pub fn set_max_output_tokens(&mut self, max_output_tokens: Option<u32>) {
        self.max_output_tokens = max_output_tokens;
    }

    pub fn set_plugin_state(&mut self, plugin_id: String, enabled: bool) {
        self.enabled_plugins.insert(plugin_id, enabled);
    }

    #[must_use]
    pub fn state_for(&self, plugin_id: &str, default_enabled: bool) -> bool {
        self.enabled_plugins
            .get(plugin_id)
            .copied()
            .unwrap_or(default_enabled)
    }
}

#[must_use]
/// Returns the default per-user config directory used by the runtime.
pub fn default_config_home() -> PathBuf {
    std::env::var_os("CLAW_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claw")))
        .unwrap_or_else(|| PathBuf::from(".claw"))
}

/// Save provider settings to the user-level `~/.claw/settings.json`.
/// Creates the file and directory if they don't exist. Sets file permissions
/// to `0o600` (owner read/write only) to protect stored API keys.
pub fn save_user_provider_settings(
    kind: &str,
    api_key: &str,
    base_url: Option<&str>,
    model: Option<&str>,
) -> Result<(), ConfigError> {
    let config_home = default_config_home();
    fs::create_dir_all(&config_home).map_err(ConfigError::Io)?;
    let settings_path = config_home.join("settings.json");

    let mut root = read_settings_root(&settings_path);

    let mut provider = serde_json::Map::new();
    provider.insert(
        "kind".to_string(),
        serde_json::Value::String(kind.to_string()),
    );
    provider.insert(
        "apiKey".to_string(),
        serde_json::Value::String(api_key.to_string()),
    );
    if let Some(base_url) = base_url {
        provider.insert(
            "baseUrl".to_string(),
            serde_json::Value::String(base_url.to_string()),
        );
    } else {
        provider.remove("baseUrl");
    }
    root.insert("provider".to_string(), serde_json::Value::Object(provider));
    if let Some(model) = model {
        root.insert(
            "model".to_string(),
            serde_json::Value::String(model.to_string()),
        );
    } else {
        root.remove("model");
    }

    write_settings_root(&settings_path, &root)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        fs::set_permissions(&settings_path, perms).map_err(ConfigError::Io)?;
    }

    Ok(())
}

/// Remove the `provider` section from the user-level `~/.claw/settings.json`.
pub fn clear_user_provider_settings() -> Result<(), ConfigError> {
    let config_home = default_config_home();
    let settings_path = config_home.join("settings.json");

    if !settings_path.exists() {
        return Ok(());
    }

    let mut root = read_settings_root(&settings_path);
    if root.remove("provider").is_none() {
        return Ok(());
    }
    root.remove("model");

    write_settings_root(&settings_path, &root)?;

    Ok(())
}

fn read_settings_root(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    match fs::read_to_string(path) {
        Ok(contents) if !contents.trim().is_empty() => {
            serde_json::from_str::<serde_json::Value>(&contents)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default()
        }
        _ => serde_json::Map::new(),
    }
}

fn write_settings_root(
    path: &Path,
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ConfigError::Io)?;
    }
    let rendered = serde_json::to_string_pretty(&serde_json::Value::Object(root.clone()))
        .map_err(|e| ConfigError::Parse(e.to_string()))?;
    fs::write(path, format!("{rendered}\n")).map_err(ConfigError::Io)
}

impl RuntimeHookConfig {
    #[must_use]
    pub fn new(
        pre_tool_use: Vec<String>,
        post_tool_use: Vec<String>,
        post_tool_use_failure: Vec<String>,
    ) -> Self {
        Self {
            pre_tool_use,
            post_tool_use,
            post_tool_use_failure,
            notification: Vec::new(),
            stop: Vec::new(),
            teammate_idle: Vec::new(),
            task_created: Vec::new(),
            task_completed: Vec::new(),
            message_display: Vec::new(),
            session_start: Vec::new(),
        }
    }

    #[must_use]
    pub fn pre_tool_use(&self) -> &[String] {
        &self.pre_tool_use
    }

    #[must_use]
    pub fn post_tool_use(&self) -> &[String] {
        &self.post_tool_use
    }

    #[must_use]
    pub fn merged(&self, other: &Self) -> Self {
        let mut merged = self.clone();
        merged.extend(other);
        merged
    }

    pub fn extend(&mut self, other: &Self) {
        extend_unique(&mut self.pre_tool_use, other.pre_tool_use());
        extend_unique(&mut self.post_tool_use, other.post_tool_use());
        extend_unique(
            &mut self.post_tool_use_failure,
            other.post_tool_use_failure(),
        );
        extend_unique(&mut self.notification, other.notification());
        extend_unique(&mut self.stop, other.stop());
        extend_unique(&mut self.message_display, other.message_display());
        extend_unique(&mut self.session_start, other.session_start());
    }

    #[must_use]
    pub fn post_tool_use_failure(&self) -> &[String] {
        &self.post_tool_use_failure
    }

    #[must_use]
    pub fn notification(&self) -> &[String] {
        &self.notification
    }

    #[must_use]
    pub fn stop(&self) -> &[String] {
        &self.stop
    }

    #[must_use]
    pub fn with_notification(mut self, commands: Vec<String>) -> Self {
        self.notification = commands;
        self
    }

    #[must_use]
    pub fn with_stop(mut self, commands: Vec<String>) -> Self {
        self.stop = commands;
        self
    }

    #[must_use]
    pub fn teammate_idle(&self) -> &[String] {
        &self.teammate_idle
    }

    #[must_use]
    pub fn task_created(&self) -> &[String] {
        &self.task_created
    }

    #[must_use]
    pub fn task_completed(&self) -> &[String] {
        &self.task_completed
    }

    #[must_use]
    pub fn with_teammate_idle(mut self, commands: Vec<String>) -> Self {
        self.teammate_idle = commands;
        self
    }

    #[must_use]
    pub fn with_task_created(mut self, commands: Vec<String>) -> Self {
        self.task_created = commands;
        self
    }

    #[must_use]
    pub fn with_task_completed(mut self, commands: Vec<String>) -> Self {
        self.task_completed = commands;
        self
    }

    #[must_use]
    pub fn message_display(&self) -> &[String] {
        &self.message_display
    }

    #[must_use]
    pub fn session_start(&self) -> &[String] {
        &self.session_start
    }

    #[must_use]
    pub fn with_message_display(mut self, commands: Vec<String>) -> Self {
        self.message_display = commands;
        self
    }

    #[must_use]
    pub fn with_session_start(mut self, commands: Vec<String>) -> Self {
        self.session_start = commands;
        self
    }
}

impl RuntimePermissionRuleConfig {
    #[must_use]
    pub fn new(
        allow: Vec<String>,
        deny: Vec<String>,
        ask: Vec<String>,
        denied_tools: Vec<String>,
    ) -> Self {
        Self {
            allow,
            deny,
            ask,
            denied_tools,
        }
    }

    #[must_use]
    pub fn allow(&self) -> &[String] {
        &self.allow
    }

    #[must_use]
    pub fn deny(&self) -> &[String] {
        &self.deny
    }

    #[must_use]
    pub fn ask(&self) -> &[String] {
        &self.ask
    }

    #[must_use]
    pub fn denied_tools(&self) -> &[String] {
        &self.denied_tools
    }
}

impl McpConfigCollection {
    #[must_use]
    pub fn servers(&self) -> &BTreeMap<String, ScopedMcpServerConfig> {
        &self.servers
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ScopedMcpServerConfig> {
        self.servers.get(name)
    }
}

impl ScopedMcpServerConfig {
    #[must_use]
    pub fn transport(&self) -> McpTransport {
        self.config.transport()
    }
}

impl McpServerConfig {
    #[must_use]
    pub fn transport(&self) -> McpTransport {
        match self {
            Self::Stdio(_) => McpTransport::Stdio,
            Self::Sse(_) => McpTransport::Sse,
            Self::Http(_) => McpTransport::Http,
            Self::Ws(_) => McpTransport::Ws,
            Self::Sdk(_) => McpTransport::Sdk,
            Self::ManagedProxy(_) => McpTransport::ManagedProxy,
        }
    }
}

/// Parsed JSON object paired with its raw source text for validation.
struct ParsedConfigFile {
    object: BTreeMap<String, JsonValue>,
    source: String,
}

fn read_optional_json_object(path: &Path) -> Result<Option<ParsedConfigFile>, ConfigError> {
    let is_legacy_config = path.file_name().and_then(|name| name.to_str()) == Some(".claw.json");
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ConfigError::Io(error)),
    };

    if contents.trim().is_empty() {
        return Ok(Some(ParsedConfigFile {
            object: BTreeMap::new(),
            source: contents,
        }));
    }

    let parsed = match JsonValue::parse(&contents) {
        Ok(parsed) => parsed,
        Err(_error) if is_legacy_config => return Ok(None),
        Err(error) => return Err(ConfigError::Parse(format!("{}: {error}", path.display()))),
    };
    let Some(object) = parsed.as_object() else {
        if is_legacy_config {
            return Ok(None);
        }
        return Err(ConfigError::Parse(format!(
            "{}: top-level settings value must be a JSON object",
            path.display()
        )));
    };
    Ok(Some(ParsedConfigFile {
        object: object.clone(),
        source: contents,
    }))
}

fn merge_mcp_servers(
    target: &mut BTreeMap<String, ScopedMcpServerConfig>,
    source: ConfigSource,
    root: &BTreeMap<String, JsonValue>,
    path: &Path,
) -> Result<(), ConfigError> {
    let Some(mcp_servers) = root.get("mcpServers") else {
        return Ok(());
    };
    let servers = expect_object(mcp_servers, &format!("{}: mcpServers", path.display()))?;
    for (name, value) in servers {
        let parsed = parse_mcp_server_config(
            name,
            value,
            &format!("{}: mcpServers.{name}", path.display()),
        )?;
        target.insert(
            name.clone(),
            ScopedMcpServerConfig {
                required: optional_bool(
                    expect_object(value, &format!("{}: mcpServers.{name}", path.display()))?,
                    "required",
                    &format!("{}: mcpServers.{name}", path.display()),
                )?
                .unwrap_or(false),
                scope: source,
                config: parsed,
            },
        );
    }
    Ok(())
}

fn parse_optional_model(root: &JsonValue) -> Option<String> {
    root.as_object()
        .and_then(|object| object.get("model"))
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
}

fn parse_optional_aliases(root: &JsonValue) -> Result<BTreeMap<String, String>, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(BTreeMap::new());
    };
    Ok(optional_string_map(object, "aliases", "merged settings")?.unwrap_or_default())
}

fn parse_optional_hooks_config(root: &JsonValue) -> Result<RuntimeHookConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(RuntimeHookConfig::default());
    };
    parse_optional_hooks_config_object(object, "merged settings.hooks")
}

fn parse_optional_hooks_config_object(
    object: &BTreeMap<String, JsonValue>,
    context: &str,
) -> Result<RuntimeHookConfig, ConfigError> {
    let Some(hooks_value) = object.get("hooks") else {
        return Ok(RuntimeHookConfig::default());
    };
    let hooks = expect_object(hooks_value, context)?;
    Ok(RuntimeHookConfig {
        pre_tool_use: optional_string_array(hooks, "PreToolUse", context)?.unwrap_or_default(),
        post_tool_use: optional_string_array(hooks, "PostToolUse", context)?.unwrap_or_default(),
        post_tool_use_failure: optional_string_array(hooks, "PostToolUseFailure", context)?
            .unwrap_or_default(),
        notification: optional_string_array(hooks, "Notification", context)?.unwrap_or_default(),
        stop: optional_string_array(hooks, "Stop", context)?.unwrap_or_default(),
        teammate_idle: optional_string_array(hooks, "TeammateIdle", context)?.unwrap_or_default(),
        task_created: optional_string_array(hooks, "TaskCreated", context)?.unwrap_or_default(),
        task_completed: optional_string_array(hooks, "TaskCompleted", context)?.unwrap_or_default(),
        message_display: optional_string_array(hooks, "MessageDisplay", context)?
            .unwrap_or_default(),
        session_start: optional_string_array(hooks, "SessionStart", context)?.unwrap_or_default(),
    })
}

fn validate_optional_hooks_config(
    root: &BTreeMap<String, JsonValue>,
    path: &Path,
) -> Result<(), ConfigError> {
    parse_optional_hooks_config_object(root, &format!("{}: hooks", path.display())).map(|_| ())
}

fn parse_optional_permission_rules(
    root: &JsonValue,
) -> Result<RuntimePermissionRuleConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(RuntimePermissionRuleConfig::default());
    };
    let Some(permissions) = object.get("permissions").and_then(JsonValue::as_object) else {
        return Ok(RuntimePermissionRuleConfig::default());
    };

    Ok(RuntimePermissionRuleConfig {
        allow: optional_string_array(permissions, "allow", "merged settings.permissions")?
            .unwrap_or_default(),
        deny: optional_string_array(permissions, "deny", "merged settings.permissions")?
            .unwrap_or_default(),
        ask: optional_string_array(permissions, "ask", "merged settings.permissions")?
            .unwrap_or_default(),
        denied_tools: optional_string_array(
            permissions,
            "deniedTools",
            "merged settings.permissions",
        )?
        .unwrap_or_default(),
    })
}

fn parse_optional_plugin_config(root: &JsonValue) -> Result<RuntimePluginConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(RuntimePluginConfig::default());
    };

    let mut config = RuntimePluginConfig::default();
    if let Some(enabled_plugins) = object.get("enabledPlugins") {
        config.enabled_plugins = parse_bool_map(enabled_plugins, "merged settings.enabledPlugins")?;
    }

    let Some(plugins_value) = object.get("plugins") else {
        return Ok(config);
    };
    let plugins = expect_object(plugins_value, "merged settings.plugins")?;

    if let Some(enabled_value) = plugins.get("enabled") {
        config.enabled_plugins = parse_bool_map(enabled_value, "merged settings.plugins.enabled")?;
    }
    config.external_directories =
        optional_string_array(plugins, "externalDirectories", "merged settings.plugins")?
            .unwrap_or_default();
    config.install_root =
        optional_string(plugins, "installRoot", "merged settings.plugins")?.map(str::to_string);
    config.registry_path =
        optional_string(plugins, "registryPath", "merged settings.plugins")?.map(str::to_string);
    config.bundled_root =
        optional_string(plugins, "bundledRoot", "merged settings.plugins")?.map(str::to_string);
    config.max_output_tokens = optional_u32(plugins, "maxOutputTokens", "merged settings.plugins")?;
    Ok(config)
}

fn parse_optional_permission_mode(
    root: &JsonValue,
) -> Result<Option<ResolvedPermissionMode>, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(None);
    };
    if let Some(mode) = object.get("permissionMode").and_then(JsonValue::as_str) {
        return parse_permission_mode_label(mode, "merged settings.permissionMode").map(Some);
    }
    let Some(mode) = object
        .get("permissions")
        .and_then(JsonValue::as_object)
        .and_then(|permissions| permissions.get("defaultMode"))
        .and_then(JsonValue::as_str)
    else {
        return Ok(None);
    };
    parse_permission_mode_label(mode, "merged settings.permissions.defaultMode").map(Some)
}

fn parse_permission_mode_label(
    mode: &str,
    context: &str,
) -> Result<ResolvedPermissionMode, ConfigError> {
    match mode {
        // v2.1.200: "manual" is the new name for the old "default" mode;
        // both are accepted alongside the legacy aliases.
        "default" | "manual" | "plan" | "read-only" => Ok(ResolvedPermissionMode::ReadOnly),
        "acceptEdits" | "auto" | "workspace-write" => Ok(ResolvedPermissionMode::WorkspaceWrite),
        "dontAsk" | "danger-full-access" => Ok(ResolvedPermissionMode::DangerFullAccess),
        other => Err(ConfigError::Parse(format!(
            "{context}: unsupported permission mode {other}"
        ))),
    }
}

fn parse_optional_sandbox_config(root: &JsonValue) -> Result<SandboxConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(SandboxConfig::default());
    };
    let Some(sandbox_value) = object.get("sandbox") else {
        return Ok(SandboxConfig::default());
    };
    let sandbox = expect_object(sandbox_value, "merged settings.sandbox")?;
    let filesystem_mode = optional_string(sandbox, "filesystemMode", "merged settings.sandbox")?
        .map(parse_filesystem_mode_label)
        .transpose()?;
    Ok(SandboxConfig {
        enabled: optional_bool(sandbox, "enabled", "merged settings.sandbox")?,
        namespace_restrictions: optional_bool(
            sandbox,
            "namespaceRestrictions",
            "merged settings.sandbox",
        )?,
        network_isolation: optional_bool(sandbox, "networkIsolation", "merged settings.sandbox")?,
        filesystem_mode,
        allowed_mounts: optional_string_array(sandbox, "allowedMounts", "merged settings.sandbox")?
            .unwrap_or_default(),
        allow_apple_events: optional_bool(sandbox, "allowAppleEvents", "merged settings.sandbox")?,
        credentials: optional_bool(sandbox, "credentials", "merged settings.sandbox")?,
    })
}

fn parse_optional_provider_fallbacks(
    root: &JsonValue,
) -> Result<ProviderFallbackConfig, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(ProviderFallbackConfig::default());
    };
    let Some(value) = object.get("providerFallbacks") else {
        return Ok(ProviderFallbackConfig::default());
    };
    let entry = expect_object(value, "merged settings.providerFallbacks")?;
    let primary =
        optional_string(entry, "primary", "merged settings.providerFallbacks")?.map(str::to_string);
    let fallbacks = optional_string_array(entry, "fallbacks", "merged settings.providerFallbacks")?
        .unwrap_or_default();
    Ok(ProviderFallbackConfig { primary, fallbacks })
}

fn parse_optional_trusted_roots(root: &JsonValue) -> Result<Vec<String>, ConfigError> {
    let Some(object) = root.as_object() else {
        return Ok(Vec::new());
    };
    Ok(
        optional_string_array(object, "trustedRoots", "merged settings.trustedRoots")?
            .unwrap_or_default(),
    )
}

fn parse_optional_string_field(root: &JsonValue, key: &str) -> Option<String> {
    root.as_object()
        .and_then(|object| object.get(key))
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
}

fn parse_optional_worktree_base_ref(root: &JsonValue) -> Option<String> {
    root.as_object()
        .and_then(|object| object.get("worktree"))
        .and_then(JsonValue::as_object)
        .and_then(|worktree| worktree.get("baseRef"))
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
}

fn parse_optional_string_vec(root: &JsonValue, key: &str) -> Option<Vec<String>> {
    let object = root.as_object()?;
    let array = object.get(key)?.as_array()?;
    Some(
        array
            .iter()
            .filter_map(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn parse_optional_disallowed_tools(root: &JsonValue) -> Vec<String> {
    root.as_object()
        .and_then(|object| object.get("permissions"))
        .and_then(JsonValue::as_object)
        .and_then(|permissions| permissions.get("disallowedTools"))
        .and_then(JsonValue::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(JsonValue::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_filesystem_mode_label(value: &str) -> Result<FilesystemIsolationMode, ConfigError> {
    match value {
        "off" => Ok(FilesystemIsolationMode::Off),
        "workspace-only" => Ok(FilesystemIsolationMode::WorkspaceOnly),
        "allow-list" => Ok(FilesystemIsolationMode::AllowList),
        other => Err(ConfigError::Parse(format!(
            "merged settings.sandbox.filesystemMode: unsupported filesystem mode {other}"
        ))),
    }
}

fn parse_optional_oauth_config(
    root: &JsonValue,
    context: &str,
) -> Result<Option<OAuthConfig>, ConfigError> {
    let Some(oauth_value) = root.as_object().and_then(|object| object.get("oauth")) else {
        return Ok(None);
    };
    let object = expect_object(oauth_value, context)?;
    let client_id = expect_string(object, "clientId", context)?.to_string();
    let authorize_url = expect_string(object, "authorizeUrl", context)?.to_string();
    let token_url = expect_string(object, "tokenUrl", context)?.to_string();
    let callback_port = optional_u16(object, "callbackPort", context)?;
    let manual_redirect_url =
        optional_string(object, "manualRedirectUrl", context)?.map(str::to_string);
    let scopes = optional_string_array(object, "scopes", context)?.unwrap_or_default();
    Ok(Some(OAuthConfig {
        client_id,
        authorize_url,
        token_url,
        callback_port,
        manual_redirect_url,
        scopes,
    }))
}

fn parse_mcp_server_config(
    server_name: &str,
    value: &JsonValue,
    context: &str,
) -> Result<McpServerConfig, ConfigError> {
    let object = expect_object(value, context)?;
    let server_type =
        optional_string(object, "type", context)?.unwrap_or_else(|| infer_mcp_server_type(object));
    match server_type {
        "stdio" => Ok(McpServerConfig::Stdio(McpStdioServerConfig {
            command: expect_string(object, "command", context)?.to_string(),
            args: optional_string_array(object, "args", context)?.unwrap_or_default(),
            env: optional_string_map(object, "env", context)?.unwrap_or_default(),
            tool_call_timeout_ms: optional_u64(object, "toolCallTimeoutMs", context)?,
        })),
        "sse" => Ok(McpServerConfig::Sse(parse_mcp_remote_server_config(
            object, context,
        )?)),
        "http" => Ok(McpServerConfig::Http(parse_mcp_remote_server_config(
            object, context,
        )?)),
        "ws" => Ok(McpServerConfig::Ws(McpWebSocketServerConfig {
            url: expect_string(object, "url", context)?.to_string(),
            headers: optional_string_map(object, "headers", context)?.unwrap_or_default(),
            headers_helper: optional_string(object, "headersHelper", context)?.map(str::to_string),
        })),
        "sdk" => Ok(McpServerConfig::Sdk(McpSdkServerConfig {
            name: expect_string(object, "name", context)?.to_string(),
        })),
        "claudeai-proxy" => Ok(McpServerConfig::ManagedProxy(McpManagedProxyServerConfig {
            url: expect_string(object, "url", context)?.to_string(),
            id: expect_string(object, "id", context)?.to_string(),
        })),
        other => Err(ConfigError::Parse(format!(
            "{context}: unsupported MCP server type for {server_name}: {other}"
        ))),
    }
}

fn infer_mcp_server_type(object: &BTreeMap<String, JsonValue>) -> &'static str {
    if object.contains_key("url") {
        "http"
    } else {
        "stdio"
    }
}

fn parse_mcp_remote_server_config(
    object: &BTreeMap<String, JsonValue>,
    context: &str,
) -> Result<McpRemoteServerConfig, ConfigError> {
    Ok(McpRemoteServerConfig {
        url: expect_string(object, "url", context)?.to_string(),
        headers: optional_string_map(object, "headers", context)?.unwrap_or_default(),
        headers_helper: optional_string(object, "headersHelper", context)?.map(str::to_string),
        oauth: parse_optional_mcp_oauth_config(object, context)?,
    })
}

fn parse_optional_mcp_oauth_config(
    object: &BTreeMap<String, JsonValue>,
    context: &str,
) -> Result<Option<McpOAuthConfig>, ConfigError> {
    let Some(value) = object.get("oauth") else {
        return Ok(None);
    };
    let oauth = expect_object(value, &format!("{context}.oauth"))?;
    Ok(Some(McpOAuthConfig {
        client_id: optional_string(oauth, "clientId", context)?.map(str::to_string),
        callback_port: optional_u16(oauth, "callbackPort", context)?,
        auth_server_metadata_url: optional_string(oauth, "authServerMetadataUrl", context)?
            .map(str::to_string),
        xaa: optional_bool(oauth, "xaa", context)?,
    }))
}

fn expect_object<'a>(
    value: &'a JsonValue,
    context: &str,
) -> Result<&'a BTreeMap<String, JsonValue>, ConfigError> {
    value
        .as_object()
        .ok_or_else(|| ConfigError::Parse(format!("{context}: expected JSON object")))
}

fn expect_string<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<&'a str, ConfigError> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| ConfigError::Parse(format!("{context}: missing string field {key}")))
}

fn optional_string<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<&'a str>, ConfigError> {
    match object.get(key) {
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| ConfigError::Parse(format!("{context}: field {key} must be a string"))),
        None => Ok(None),
    }
}

fn parse_optional_bool(value: &JsonValue, key: &str) -> Option<bool> {
    value.as_object()?.get(key)?.as_bool()
}

/// Read a bool from a nested object path, e.g. ["attribution","sessionUrl"].
/// Used for v2.1.183/193 nested settings.
fn nested_bool(root: &JsonValue, path: &[&str]) -> Option<bool> {
    let mut current = root.as_object()?;
    for (i, key) in path.iter().enumerate() {
        if i + 1 == path.len() {
            return current.get(*key)?.as_bool();
        }
        current = current.get(*key)?.as_object()?;
    }
    None
}

/// Read a string from a nested object path, e.g. ["workflow","size"].
/// Used for v2.1.202 nested settings.
fn nested_string<'a>(root: &'a JsonValue, path: &[&str]) -> Option<&'a str> {
    let mut current = root.as_object()?;
    for (i, key) in path.iter().enumerate() {
        if i + 1 == path.len() {
            return current.get(*key)?.as_str();
        }
        current = current.get(*key)?.as_object()?;
    }
    None
}

/// v2.1.212: read a per-session runaway cap from an env var, falling back to
/// `default`. `0` means unlimited. Used for WebSearch / subagent caps.
fn session_cap_from_env(var: &str, default: u32) -> u32 {
    match std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
    {
        Some(n) => n,
        None => default,
    }
}

fn optional_bool(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<bool>, ConfigError> {
    match object.get(key) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| ConfigError::Parse(format!("{context}: field {key} must be a boolean"))),
        None => Ok(None),
    }
}

fn optional_u16(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<u16>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(number) = value.as_i64() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be an integer"
                )));
            };
            let number = u16::try_from(number).map_err(|_| {
                ConfigError::Parse(format!("{context}: field {key} is out of range"))
            })?;
            Ok(Some(number))
        }
        None => Ok(None),
    }
}

fn optional_u32(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<u32>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(number) = value.as_i64() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be a non-negative integer"
                )));
            };
            let number = u32::try_from(number).map_err(|_| {
                ConfigError::Parse(format!("{context}: field {key} is out of range"))
            })?;
            Ok(Some(number))
        }
        None => Ok(None),
    }
}

fn optional_u64(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<u64>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(number) = value.as_i64() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be a non-negative integer"
                )));
            };
            let number = u64::try_from(number).map_err(|_| {
                ConfigError::Parse(format!("{context}: field {key} is out of range"))
            })?;
            Ok(Some(number))
        }
        None => Ok(None),
    }
}

fn parse_bool_map(value: &JsonValue, context: &str) -> Result<BTreeMap<String, bool>, ConfigError> {
    let Some(map) = value.as_object() else {
        return Err(ConfigError::Parse(format!(
            "{context}: expected JSON object"
        )));
    };
    map.iter()
        .map(|(key, value)| {
            value
                .as_bool()
                .map(|enabled| (key.clone(), enabled))
                .ok_or_else(|| {
                    ConfigError::Parse(format!("{context}: field {key} must be a boolean"))
                })
        })
        .collect()
}

fn optional_string_array(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<Vec<String>>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(array) = value.as_array() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be an array"
                )));
            };
            array
                .iter()
                .map(|item| {
                    item.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        ConfigError::Parse(format!(
                            "{context}: field {key} must contain only strings"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        }
        None => Ok(None),
    }
}

fn optional_string_map(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    context: &str,
) -> Result<Option<BTreeMap<String, String>>, ConfigError> {
    match object.get(key) {
        Some(value) => {
            let Some(map) = value.as_object() else {
                return Err(ConfigError::Parse(format!(
                    "{context}: field {key} must be an object"
                )));
            };
            map.iter()
                .map(|(entry_key, entry_value)| {
                    entry_value
                        .as_str()
                        .map(|text| (entry_key.clone(), text.to_string()))
                        .ok_or_else(|| {
                            ConfigError::Parse(format!(
                                "{context}: field {key} must contain only string values"
                            ))
                        })
                })
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map(Some)
        }
        None => Ok(None),
    }
}

fn deep_merge_objects(
    target: &mut BTreeMap<String, JsonValue>,
    source: &BTreeMap<String, JsonValue>,
) {
    for (key, value) in source {
        match (target.get_mut(key), value) {
            (Some(JsonValue::Object(existing)), JsonValue::Object(incoming)) => {
                deep_merge_objects(existing, incoming);
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn extend_unique(target: &mut Vec<String>, values: &[String]) {
    for value in values {
        push_unique(target, value.clone());
    }
}

fn push_unique(target: &mut Vec<String>, value: String) {
    if !target.iter().any(|existing| existing == &value) {
        target.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        deep_merge_objects, parse_permission_mode_label, ConfigLoader, ConfigSource,
        McpServerConfig, McpTransport, ResolvedPermissionMode, RuntimeFeatureConfig,
        RuntimeHookConfig, RuntimePluginConfig, CLAW_SETTINGS_SCHEMA_NAME,
    };
    use crate::json::JsonValue;
    use crate::sandbox::FilesystemIsolationMode;
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> std::path::PathBuf {
        // #149: previously used `runtime-config-{nanos}` which collided
        // under parallel `cargo test --workspace` when multiple tests
        // started within the same nanosecond bucket on fast machines.
        // Add process id + a monotonically-incrementing atomic counter
        // so every callsite gets a provably-unique directory regardless
        // of clock resolution or scheduling.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("runtime-config-{pid}-{nanos}-{seq}"))
    }

    #[test]
    fn rejects_non_object_settings_files() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "[]").expect("write bad settings");

        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");
        assert!(error
            .to_string()
            .contains("top-level settings value must be a JSON object"));

        if root.exists() {
            fs::remove_dir_all(root).expect("cleanup temp dir");
        }
    }

    #[test]
    fn loads_and_merges_claude_code_config_files_by_precedence() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.parent().expect("home parent").join(".claw.json"),
            r#"{"model":"haiku","env":{"A":"1"},"mcpServers":{"home":{"command":"uvx","args":["home"]}}}"#,
        )
        .expect("write user compat config");
        fs::write(
            home.join("settings.json"),
            r#"{"model":"sonnet","env":{"A2":"1"},"hooks":{"PreToolUse":["base"]},"permissions":{"defaultMode":"plan","allow":["Read"],"deny":["Bash(rm -rf)"]}}"#,
        )
        .expect("write user settings");
        fs::write(
            cwd.join(".claw.json"),
            r#"{"model":"project-compat","env":{"B":"2"}}"#,
        )
        .expect("write project compat config");
        fs::write(
            cwd.join(".claw").join("settings.json"),
            r#"{"env":{"C":"3"},"hooks":{"PostToolUse":["project"],"PostToolUseFailure":["project-failure"]},"permissions":{"ask":["Edit"]},"mcpServers":{"project":{"command":"uvx","args":["project"]}}}"#,
        )
        .expect("write project settings");
        fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{"model":"opus","permissionMode":"acceptEdits"}"#,
        )
        .expect("write local settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(CLAW_SETTINGS_SCHEMA_NAME, "SettingsSchema");
        assert_eq!(loaded.loaded_entries().len(), 5);
        assert_eq!(loaded.loaded_entries()[0].source, ConfigSource::User);
        assert_eq!(
            loaded.get("model"),
            Some(&JsonValue::String("opus".to_string()))
        );
        assert_eq!(loaded.model(), Some("opus"));
        assert_eq!(
            loaded.permission_mode(),
            Some(ResolvedPermissionMode::WorkspaceWrite)
        );
        assert_eq!(
            loaded
                .get("env")
                .and_then(JsonValue::as_object)
                .expect("env object")
                .len(),
            4
        );
        assert!(loaded
            .get("hooks")
            .and_then(JsonValue::as_object)
            .expect("hooks object")
            .contains_key("PreToolUse"));
        assert!(loaded
            .get("hooks")
            .and_then(JsonValue::as_object)
            .expect("hooks object")
            .contains_key("PostToolUse"));
        assert_eq!(loaded.hooks().pre_tool_use(), &["base".to_string()]);
        assert_eq!(loaded.hooks().post_tool_use(), &["project".to_string()]);
        assert_eq!(
            loaded.hooks().post_tool_use_failure(),
            &["project-failure".to_string()]
        );
        assert_eq!(loaded.permission_rules().allow(), &["Read".to_string()]);
        assert_eq!(
            loaded.permission_rules().deny(),
            &["Bash(rm -rf)".to_string()]
        );
        assert_eq!(loaded.permission_rules().ask(), &["Edit".to_string()]);
        assert!(loaded.mcp().get("home").is_some());
        assert!(loaded.mcp().get("project").is_some());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_sandbox_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{
              "sandbox": {
                "enabled": true,
                "namespaceRestrictions": false,
                "networkIsolation": true,
                "filesystemMode": "allow-list",
                "allowedMounts": ["logs", "tmp/cache"]
              }
            }"#,
        )
        .expect("write local settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(loaded.sandbox().enabled, Some(true));
        assert_eq!(loaded.sandbox().namespace_restrictions, Some(false));
        assert_eq!(loaded.sandbox().network_isolation, Some(true));
        assert_eq!(
            loaded.sandbox().filesystem_mode,
            Some(FilesystemIsolationMode::AllowList)
        );
        assert_eq!(loaded.sandbox().allowed_mounts, vec!["logs", "tmp/cache"]);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_provider_fallbacks_chain_with_primary_and_ordered_fallbacks() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");
        fs::write(
            home.join("settings.json"),
            r#"{
              "providerFallbacks": {
                "primary": "claude-opus-4-6",
                "fallbacks": ["grok-3", "grok-3-mini"]
              }
            }"#,
        )
        .expect("write provider fallback settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        let chain = loaded.provider_fallbacks();
        assert_eq!(chain.primary(), Some("claude-opus-4-6"));
        assert_eq!(
            chain.fallbacks(),
            &["grok-3".to_string(), "grok-3-mini".to_string()]
        );
        assert!(!chain.is_empty());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn provider_fallbacks_default_is_empty_when_unset() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "{}").expect("write empty settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        let chain = loaded.provider_fallbacks();
        assert_eq!(chain.primary(), None);
        assert!(chain.fallbacks().is_empty());
        assert!(chain.is_empty());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_trusted_roots_from_settings() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"trustedRoots": ["/tmp/worktrees", "/home/user/projects"]}"#,
        )
        .expect("write settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        let roots = loaded.trusted_roots();
        assert_eq!(roots, ["/tmp/worktrees", "/home/user/projects"]);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn trusted_roots_with_overrides_preserves_config_defaults_and_adds_per_call_roots() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"trustedRoots": ["/tmp/config-default", "/tmp/shared"]}"#,
        )
        .expect("write settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");
        let merged = loaded.trusted_roots_with_overrides(&[
            "/tmp/per-call".to_string(),
            "/tmp/shared".to_string(),
        ]);

        // then
        assert_eq!(
            merged,
            ["/tmp/config-default", "/tmp/shared", "/tmp/per-call"]
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn runtime_feature_trusted_roots_with_overrides_matches_runtime_config_merge() {
        let config = RuntimeFeatureConfig {
            trusted_roots: vec!["/tmp/config".to_string()],
            ..RuntimeFeatureConfig::default()
        };

        assert_eq!(
            config.trusted_roots_with_overrides(&["/tmp/per-call".to_string()]),
            ["/tmp/config", "/tmp/per-call"]
        );
    }

    #[test]
    fn trusted_roots_default_is_empty_when_unset() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "{}").expect("write empty settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        assert!(loaded.trusted_roots().is_empty());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_typed_mcp_and_oauth_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{
              "mcpServers": {
                "stdio-server": {
                  "command": "uvx",
                  "args": ["mcp-server"],
                  "env": {"TOKEN": "secret"},
                  "required": true
                },
                "remote-server": {
                  "type": "http",
                  "url": "https://example.test/mcp",
                  "headers": {"Authorization": "Bearer token"},
                  "headersHelper": "helper.sh",
                  "oauth": {
                    "clientId": "mcp-client",
                    "callbackPort": 7777,
                    "authServerMetadataUrl": "https://issuer.test/.well-known/oauth-authorization-server",
                    "xaa": true
                  }
                }
              },
              "oauth": {
                "clientId": "runtime-client",
                "authorizeUrl": "https://console.test/oauth/authorize",
                "tokenUrl": "https://console.test/oauth/token",
                "callbackPort": 54545,
                "manualRedirectUrl": "https://console.test/oauth/callback",
                "scopes": ["org:read", "user:write"]
              }
            }"#,
        )
        .expect("write user settings");
        fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{
              "mcpServers": {
                "remote-server": {
                  "type": "ws",
                  "url": "wss://override.test/mcp",
                  "headers": {"X-Env": "local"}
                }
              }
            }"#,
        )
        .expect("write local settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        let stdio_server = loaded
            .mcp()
            .get("stdio-server")
            .expect("stdio server should exist");
        assert_eq!(stdio_server.scope, ConfigSource::User);
        assert!(stdio_server.required);
        assert_eq!(stdio_server.transport(), McpTransport::Stdio);

        let remote_server = loaded
            .mcp()
            .get("remote-server")
            .expect("remote server should exist");
        assert_eq!(remote_server.scope, ConfigSource::Local);
        assert!(!remote_server.required);
        assert_eq!(remote_server.transport(), McpTransport::Ws);
        match &remote_server.config {
            McpServerConfig::Ws(config) => {
                assert_eq!(config.url, "wss://override.test/mcp");
                assert_eq!(
                    config.headers.get("X-Env").map(String::as_str),
                    Some("local")
                );
            }
            other => panic!("expected ws config, got {other:?}"),
        }

        let oauth = loaded.oauth().expect("oauth config should exist");
        assert_eq!(oauth.client_id, "runtime-client");
        assert_eq!(oauth.callback_port, Some(54_545));
        assert_eq!(oauth.scopes, vec!["org:read", "user:write"]);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn infers_http_mcp_servers_from_url_only_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{
              "mcpServers": {
                "remote": {
                  "url": "https://example.test/mcp"
                }
              }
            }"#,
        )
        .expect("write mcp settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        let remote_server = loaded
            .mcp()
            .get("remote")
            .expect("remote server should exist");
        assert_eq!(remote_server.transport(), McpTransport::Http);
        match &remote_server.config {
            McpServerConfig::Http(config) => {
                assert_eq!(config.url, "https://example.test/mcp");
            }
            other => panic!("expected http config, got {other:?}"),
        }

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_plugin_config_from_enabled_plugins() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{
              "enabledPlugins": {
                "tool-guard@builtin": true,
                "sample-plugin@external": false
              }
            }"#,
        )
        .expect("write user settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(
            loaded.plugins().enabled_plugins().get("tool-guard@builtin"),
            Some(&true)
        );
        assert_eq!(
            loaded
                .plugins()
                .enabled_plugins()
                .get("sample-plugin@external"),
            Some(&false)
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_plugin_config() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{
              "enabledPlugins": {
                "core-helpers@builtin": true
              },
              "plugins": {
                "externalDirectories": ["./external-plugins"],
                "installRoot": "plugin-cache/installed",
                "registryPath": "plugin-cache/installed.json",
                "bundledRoot": "./bundled-plugins"
              }
            }"#,
        )
        .expect("write plugin settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(
            loaded
                .plugins()
                .enabled_plugins()
                .get("core-helpers@builtin"),
            Some(&true)
        );
        assert_eq!(
            loaded.plugins().external_directories(),
            &["./external-plugins".to_string()]
        );
        assert_eq!(
            loaded.plugins().install_root(),
            Some("plugin-cache/installed")
        );
        assert_eq!(
            loaded.plugins().registry_path(),
            Some("plugin-cache/installed.json")
        );
        assert_eq!(loaded.plugins().bundled_root(), Some("./bundled-plugins"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn rejects_invalid_mcp_server_shapes() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"mcpServers":{"broken":{"type":"http","url":123}}}"#,
        )
        .expect("write broken settings");

        // when
        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");

        // then
        assert!(error
            .to_string()
            .contains("mcpServers.broken: missing string field url"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_user_defined_model_aliases_from_settings() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{"aliases":{"fast":"claude-haiku-4-5-20251213","smart":"claude-opus-4-6"}}"#,
        )
        .expect("write user settings");
        fs::write(
            cwd.join(".claw").join("settings.local.json"),
            r#"{"aliases":{"smart":"claude-sonnet-4-6","cheap":"grok-3-mini"}}"#,
        )
        .expect("write local settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        // then
        let aliases = loaded.aliases();
        assert_eq!(
            aliases.get("fast").map(String::as_str),
            Some("claude-haiku-4-5-20251213")
        );
        assert_eq!(
            aliases.get("smart").map(String::as_str),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(
            aliases.get("cheap").map(String::as_str),
            Some("grok-3-mini")
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn empty_settings_file_loads_defaults() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "").expect("write empty settings");

        // when
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("empty settings should still load");

        // then
        assert_eq!(loaded.loaded_entries().len(), 1);
        assert_eq!(loaded.permission_mode(), None);
        assert_eq!(loaded.plugins().enabled_plugins().len(), 0);

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn deep_merge_objects_merges_nested_maps() {
        // given
        let mut target = JsonValue::parse(r#"{"env":{"A":"1","B":"2"},"model":"haiku"}"#)
            .expect("target JSON should parse")
            .as_object()
            .expect("target should be an object")
            .clone();
        let source =
            JsonValue::parse(r#"{"env":{"B":"override","C":"3"},"sandbox":{"enabled":true}}"#)
                .expect("source JSON should parse")
                .as_object()
                .expect("source should be an object")
                .clone();

        // when
        deep_merge_objects(&mut target, &source);

        // then
        let env = target
            .get("env")
            .and_then(JsonValue::as_object)
            .expect("env should remain an object");
        assert_eq!(env.get("A"), Some(&JsonValue::String("1".to_string())));
        assert_eq!(
            env.get("B"),
            Some(&JsonValue::String("override".to_string()))
        );
        assert_eq!(env.get("C"), Some(&JsonValue::String("3".to_string())));
        assert!(target.contains_key("sandbox"));
    }

    #[test]
    fn rejects_invalid_hook_entries_before_merge() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let project_settings = cwd.join(".claw").join("settings.json");
        fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        fs::create_dir_all(&home).expect("home config dir");

        fs::write(
            home.join("settings.json"),
            r#"{"hooks":{"PreToolUse":["base"]}}"#,
        )
        .expect("write user settings");
        fs::write(
            &project_settings,
            r#"{"hooks":{"PreToolUse":["project",42]}}"#,
        )
        .expect("write invalid project settings");

        // when
        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");

        // then — config validation now catches the mixed array before the hooks parser
        let rendered = error.to_string();
        assert!(
            rendered.contains("hooks.PreToolUse")
                && rendered.contains("must be an array of strings"),
            "expected validation error for hooks.PreToolUse, got: {rendered}"
        );
        assert!(!rendered.contains("merged settings.hooks"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn permission_mode_aliases_resolve_to_expected_modes() {
        // given / when / then
        assert_eq!(
            parse_permission_mode_label("plan", "test").expect("plan should resolve"),
            ResolvedPermissionMode::ReadOnly
        );
        assert_eq!(
            parse_permission_mode_label("acceptEdits", "test").expect("acceptEdits should resolve"),
            ResolvedPermissionMode::WorkspaceWrite
        );
        assert_eq!(
            parse_permission_mode_label("dontAsk", "test").expect("dontAsk should resolve"),
            ResolvedPermissionMode::DangerFullAccess
        );
    }

    #[test]
    fn hook_config_merge_preserves_uniques() {
        // given
        let base = RuntimeHookConfig::new(
            vec!["pre-a".to_string()],
            vec!["post-a".to_string()],
            vec!["failure-a".to_string()],
        );
        let overlay = RuntimeHookConfig::new(
            vec!["pre-a".to_string(), "pre-b".to_string()],
            vec!["post-a".to_string(), "post-b".to_string()],
            vec!["failure-b".to_string()],
        );

        // when
        let merged = base.merged(&overlay);

        // then
        assert_eq!(
            merged.pre_tool_use(),
            &["pre-a".to_string(), "pre-b".to_string()]
        );
        assert_eq!(
            merged.post_tool_use(),
            &["post-a".to_string(), "post-b".to_string()]
        );
        assert_eq!(
            merged.post_tool_use_failure(),
            &["failure-a".to_string(), "failure-b".to_string()]
        );
    }

    #[test]
    fn plugin_state_falls_back_to_default_for_unknown_plugin() {
        // given
        let mut config = RuntimePluginConfig::default();
        config.set_plugin_state("known".to_string(), true);

        // when / then
        assert!(config.state_for("known", false));
        assert!(config.state_for("missing", true));
        assert!(!config.state_for("missing", false));
    }

    #[test]
    fn validates_unknown_top_level_keys_with_line_and_field_name() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let user_settings = home.join("settings.json");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            &user_settings,
            "{\n  \"model\": \"opus\",\n  \"telemetry\": true\n}\n",
        )
        .expect("write user settings");

        // when
        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");

        // then
        let rendered = error.to_string();
        assert!(
            rendered.contains(&user_settings.display().to_string()),
            "error should include file path, got: {rendered}"
        );
        assert!(
            rendered.contains("line 3"),
            "error should include line number, got: {rendered}"
        );
        assert!(
            rendered.contains("telemetry"),
            "error should name the offending field, got: {rendered}"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn validates_deprecated_top_level_keys_with_replacement_guidance() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let user_settings = home.join("settings.json");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            &user_settings,
            "{\n  \"model\": \"opus\",\n  \"allowedTools\": [\"Read\"]\n}\n",
        )
        .expect("write user settings");

        // when
        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");

        // then
        let rendered = error.to_string();
        assert!(
            rendered.contains(&user_settings.display().to_string()),
            "error should include file path, got: {rendered}"
        );
        assert!(
            rendered.contains("line 3"),
            "error should include line number, got: {rendered}"
        );
        assert!(
            rendered.contains("allowedTools"),
            "error should call out the unknown field, got: {rendered}"
        );
        // allowedTools is an unknown key; validator should name it in the error
        assert!(
            rendered.contains("allowedTools"),
            "error should name the offending field, got: {rendered}"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn validates_wrong_type_for_known_field_with_field_path() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let user_settings = home.join("settings.json");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            &user_settings,
            "{\n  \"hooks\": {\n    \"PreToolUse\": \"not-an-array\"\n  }\n}\n",
        )
        .expect("write user settings");

        // when
        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");

        // then
        let rendered = error.to_string();
        assert!(
            rendered.contains(&user_settings.display().to_string()),
            "error should include file path, got: {rendered}"
        );
        assert!(
            rendered.contains("hooks"),
            "error should include field path component 'hooks', got: {rendered}"
        );
        assert!(
            rendered.contains("PreToolUse"),
            "error should describe the type mismatch, got: {rendered}"
        );
        assert!(
            rendered.contains("array"),
            "error should describe the expected type, got: {rendered}"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn unknown_top_level_key_suggests_closest_match() {
        // given
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        let user_settings = home.join("settings.json");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(&user_settings, "{\n  \"modle\": \"opus\"\n}\n").expect("write user settings");

        // when
        let error = ConfigLoader::new(&cwd, &home)
            .load()
            .expect_err("config should fail");

        // then
        let rendered = error.to_string();
        assert!(
            rendered.contains("modle"),
            "error should name the offending field, got: {rendered}"
        );
        assert!(
            rendered.contains("model"),
            "error should suggest the closest known key, got: {rendered}"
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_notification_hooks_from_json() {
        let mut hooks_map = BTreeMap::new();
        let mut inner = BTreeMap::new();
        inner.insert(
            "Notification".to_string(),
            JsonValue::Array(vec![JsonValue::String("echo 'notified'".to_string())]),
        );
        hooks_map.insert("hooks".to_string(), JsonValue::Object(inner));
        let root = JsonValue::Object(hooks_map);
        let hooks = super::parse_optional_hooks_config(&root).expect("parse hooks");
        assert_eq!(hooks.notification(), &["echo 'notified'".to_string()]);
        assert!(hooks.pre_tool_use().is_empty());
        assert!(hooks.stop().is_empty());
    }

    #[test]
    fn parses_stop_hooks_from_json() {
        let mut hooks_map = BTreeMap::new();
        let mut inner = BTreeMap::new();
        inner.insert(
            "Stop".to_string(),
            JsonValue::Array(vec![JsonValue::String("echo 'stopped'".to_string())]),
        );
        hooks_map.insert("hooks".to_string(), JsonValue::Object(inner));
        let root = JsonValue::Object(hooks_map);
        let hooks = super::parse_optional_hooks_config(&root).expect("parse hooks");
        assert_eq!(hooks.stop(), &["echo 'stopped'".to_string()]);
        assert!(hooks.pre_tool_use().is_empty());
    }

    #[test]
    fn parses_all_five_hook_types_from_json() {
        let mut hooks_map = BTreeMap::new();
        let mut inner = BTreeMap::new();
        inner.insert(
            "PreToolUse".to_string(),
            JsonValue::Array(vec![JsonValue::String("pre.sh".to_string())]),
        );
        inner.insert(
            "PostToolUse".to_string(),
            JsonValue::Array(vec![JsonValue::String("post.sh".to_string())]),
        );
        inner.insert(
            "PostToolUseFailure".to_string(),
            JsonValue::Array(vec![JsonValue::String("fail.sh".to_string())]),
        );
        inner.insert(
            "Notification".to_string(),
            JsonValue::Array(vec![JsonValue::String("notify.sh".to_string())]),
        );
        inner.insert(
            "Stop".to_string(),
            JsonValue::Array(vec![JsonValue::String("stop.sh".to_string())]),
        );
        hooks_map.insert("hooks".to_string(), JsonValue::Object(inner));
        let root = JsonValue::Object(hooks_map);
        let hooks = super::parse_optional_hooks_config(&root).expect("parse hooks");
        assert_eq!(hooks.pre_tool_use(), &["pre.sh".to_string()]);
        assert_eq!(hooks.post_tool_use(), &["post.sh".to_string()]);
        assert_eq!(hooks.post_tool_use_failure(), &["fail.sh".to_string()]);
        assert_eq!(hooks.notification(), &["notify.sh".to_string()]);
        assert_eq!(hooks.stop(), &["stop.sh".to_string()]);
    }

    #[test]
    fn defaults_notification_and_stop_to_empty_when_absent() {
        let mut hooks_map = BTreeMap::new();
        let mut inner = BTreeMap::new();
        inner.insert(
            "PreToolUse".to_string(),
            JsonValue::Array(vec![JsonValue::String("pre.sh".to_string())]),
        );
        hooks_map.insert("hooks".to_string(), JsonValue::Object(inner));
        let root = JsonValue::Object(hooks_map);
        let hooks = super::parse_optional_hooks_config(&root).expect("parse hooks");
        assert_eq!(hooks.pre_tool_use(), &["pre.sh".to_string()]);
        assert!(hooks.notification().is_empty());
        assert!(hooks.stop().is_empty());
    }

    #[test]
    fn hook_config_builder_chains_correctly() {
        let config = RuntimeHookConfig::new(
            vec!["pre".to_string()],
            vec!["post".to_string()],
            vec!["fail".to_string()],
        )
        .with_notification(vec!["notify".to_string()])
        .with_stop(vec!["stop".to_string()]);

        assert_eq!(config.pre_tool_use(), &["pre".to_string()]);
        assert_eq!(config.notification(), &["notify".to_string()]);
        assert_eq!(config.stop(), &["stop".to_string()]);
    }

    #[test]
    fn parses_message_display_and_session_start_hooks() {
        let mut hooks_map = BTreeMap::new();
        let mut inner = BTreeMap::new();
        inner.insert(
            "MessageDisplay".to_string(),
            JsonValue::Array(vec![JsonValue::String("echo 'display'".to_string())]),
        );
        inner.insert(
            "SessionStart".to_string(),
            JsonValue::Array(vec![JsonValue::String("echo 'start'".to_string())]),
        );
        hooks_map.insert("hooks".to_string(), JsonValue::Object(inner));
        let root = JsonValue::Object(hooks_map);
        let hooks = super::parse_optional_hooks_config(&root).expect("parse hooks");
        assert_eq!(hooks.message_display(), &["echo 'display'".to_string()]);
        assert_eq!(hooks.session_start(), &["echo 'start'".to_string()]);
        assert!(hooks.pre_tool_use().is_empty());
    }

    #[test]
    fn parses_fallback_model_from_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"fallbackModel":"claude-haiku-4-5-20251213"}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(loaded.fallback_model(), Some("claude-haiku-4-5-20251213"));
        assert_eq!(
            loaded.feature_config().fallback_model(),
            Some("claude-haiku-4-5-20251213")
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_v2175_v2176_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{
                "enforceAvailableModels": true,
                "availableModels": ["anthropic/claude-opus-4-6", "anthropic/claude-fable-5"],
                "language": "zh",
                "disableBundledSkills": true
            }"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert!(loaded.enforce_available_models());
        assert_eq!(
            loaded.available_models(),
            ["anthropic/claude-opus-4-6", "anthropic/claude-fable-5"]
        );
        assert_eq!(loaded.language(), Some("zh"));
        assert!(loaded.disable_bundled_skills());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn disable_bundled_skills_defaults_false() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "{}").expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert!(!loaded.enforce_available_models());
        assert!(loaded.available_models().is_empty());
        assert_eq!(loaded.language(), None);
        assert!(!loaded.disable_bundled_skills());
        // v2.1.186/183/193 defaults
        assert!(loaded.respond_to_bash_commands());
        assert!(loaded.attribution_session_url());
        assert!(!loaded.auto_mode_classify_all_shell());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_v2186_v2193_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{
                "respondToBashCommands": false,
                "attribution": {"sessionUrl": false},
                "autoMode": {"classifyAllShell": true},
                "sandbox": {"allowAppleEvents": true, "credentials": false}
            }"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert!(!loaded.respond_to_bash_commands());
        assert!(!loaded.attribution_session_url());
        assert!(loaded.auto_mode_classify_all_shell());
        assert_eq!(loaded.sandbox().allow_apple_events, Some(true));
        assert_eq!(loaded.sandbox().credentials, Some(false));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_v2200_v2202_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{
                "permissions": {"defaultMode": "manual"},
                "workflow": {"size": "large"}
            }"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");
        // v2.1.200: "manual" accepted as permission mode
        assert_eq!(
            loaded.permission_mode(),
            Some(ResolvedPermissionMode::ReadOnly)
        );
        // v2.1.202: workflow size advisory
        assert_eq!(loaded.workflow_size(), Some("large"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn workflow_size_maps_to_agent_count() {
        // v2.1.202: WorkflowScript::size_to_agent_count
        use crate::WorkflowScript;
        assert_eq!(WorkflowScript::size_to_agent_count("small"), Some(3));
        assert_eq!(WorkflowScript::size_to_agent_count("Medium"), Some(8));
        assert_eq!(WorkflowScript::size_to_agent_count("LARGE"), Some(16));
        assert_eq!(WorkflowScript::size_to_agent_count("huge"), None);
    }

    #[test]
    fn session_caps_default_to_200() {
        // v2.1.212: runaway caps default to 200 when env unset.
        // (Env may be set in CI; just assert a sane positive default.)
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "{}").expect("write settings");
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");
        assert!(loaded.max_web_searches_per_session() > 0);
        assert!(loaded.max_subagents_per_session() > 0);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn v2233_env_defaults() {
        // v2.1.233: memory limit off by default; WebFetch TTL 15 min.
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "{}").expect("write settings");
        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");
        assert_eq!(
            loaded.tool_memory_limit_mib(),
            0,
            "memory limit off by default"
        );
        assert_eq!(
            loaded.webfetch_cache_ttl_ms(),
            900_000,
            "WebFetch TTL default 15min"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn parses_worktree_base_ref_from_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"worktree":{"baseRef":"fresh"}}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(loaded.worktree_base_ref(), Some("fresh"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_allow_all_claude_ai_mcps_from_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"allowAllClaudeAiMcps":true}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert!(loaded.allow_all_claude_ai_mcps());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_lean_system_prompt_default_from_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"leanSystemPromptDefault":false}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert!(!loaded.lean_system_prompt_default());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn lean_system_prompt_defaults_to_true_when_absent() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "{}").expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert!(loaded.lean_system_prompt_default());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_plugin_suggestion_marketplaces_from_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"pluginSuggestionMarketplaces":["https://registry.example.com","https://alt.example.com"]}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(
            loaded.plugin_suggestion_marketplaces(),
            &[
                "https://registry.example.com".to_string(),
                "https://alt.example.com".to_string()
            ]
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn parses_disallowed_tools_from_settings() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(
            home.join("settings.json"),
            r#"{"permissions":{"disallowedTools":["Bash","Write"]}}"#,
        )
        .expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert_eq!(
            loaded.disallowed_tools(),
            &["Bash".to_string(), "Write".to_string()]
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn disallowed_tools_defaults_to_empty_when_absent() {
        let root = temp_dir();
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        fs::create_dir_all(&home).expect("home config dir");
        fs::create_dir_all(&cwd).expect("project dir");
        fs::write(home.join("settings.json"), "{}").expect("write settings");

        let loaded = ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");

        assert!(loaded.disallowed_tools().is_empty());

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }
}
