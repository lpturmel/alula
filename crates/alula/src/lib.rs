pub mod agent;
pub mod config;
pub mod format;
pub mod http;
pub mod mcp;
pub mod mcp_http;
pub mod model;
pub mod persistence;
pub mod settings;
pub mod shortcuts;
pub mod tls;
pub mod variables;
pub mod websocket;

pub use agent::{
    AgentCommand, AgentReply, EnvironmentAgentCommand, HistoryAgentCommand, MCP_TOOL_COUNT,
    ThemeAgentCommand, apply_agent_command, apply_environment_agent_command,
    apply_history_agent_command, apply_theme_agent_command, mcp_tools, theme_authoring_schema,
};
pub use config::{
    AgentSettings, AppConfig, ApplicationSettings, KeybindingSettings, ShortcutCommand,
    SyntaxPalette, ThemeModePreference, ThemePalette, config_path, default_config_path,
    import_editor_theme, save_config_location,
};
pub use format::{
    CachedFormattedBody, FormattedBody, ResponseBodyCache, chunked_fenced_code_blocks,
    fenced_code_block, format_response_body, syntax_language, trim_response_formatting_start,
};
pub use http::{HttpExecutor, HttpSession, HttpStreamEvent};
pub use mcp::{
    LATEST_PROTOCOL_VERSION, McpServer, McpToolHandler, SUPPORTED_PROTOCOL_VERSIONS, reply_to_tool,
};
pub use mcp_http::McpHttpServer;
pub use model::{HttpMethod, KeyValueField, RequestDraft, ResponseSnapshot, Workspace};
pub use persistence::{
    Environment, EnvironmentFolder, EnvironmentStore, EnvironmentVariable, HistoryEntry,
    HistoryStore, PersistedState, StatePaths, load_or_default, save_toml,
};
pub use settings::{SettingsView, apply_theme};
pub use shortcuts::*;
pub use tls::install_tls_crypto_provider;
pub use variables::{
    TemplateInspection, VariableError, VariableErrorKind, delete_secret, inspect_template,
    load_secret, resolve_request, resolve_template, store_secret, valid_variable_name,
};
pub use websocket::{
    WebSocketDirection, WebSocketExecutor, WebSocketMessageKind, WebSocketMessageSnapshot,
    WebSocketStreamEvent, is_websocket_request,
};
