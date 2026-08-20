pub mod agent;
pub mod config;
pub mod format;
pub mod http;
pub mod mcp;
pub mod model;
pub mod persistence;
pub mod settings;

pub use agent::{
    AgentCommand, AgentReply, EnvironmentAgentCommand, HistoryAgentCommand, ThemeAgentCommand,
    apply_agent_command, apply_environment_agent_command, apply_history_agent_command,
    apply_theme_agent_command, mcp_tools, theme_authoring_schema,
};
pub use config::{
    AgentSettings, AppConfig, ApplicationSettings, SyntaxPalette, ThemeModePreference,
    ThemePalette, config_path, default_config_path, import_editor_theme, save_config_location,
};
pub use format::{
    CachedFormattedBody, FormattedBody, ResponseBodyCache, chunked_fenced_code_blocks,
    fenced_code_block, format_response_body, syntax_language,
};
pub use http::{HttpExecutor, HttpStreamEvent};
pub use mcp::McpServer;
pub use model::{HttpMethod, KeyValueField, RequestDraft, ResponseSnapshot, Workspace};
pub use persistence::{
    Environment, EnvironmentStore, HistoryEntry, HistoryStore, PersistedState, StatePaths,
    load_or_default, save_toml,
};
pub use settings::{SettingsView, apply_theme};
