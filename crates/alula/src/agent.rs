use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    config::{AppConfig, import_editor_theme},
    model::{HttpMethod, KeyValueField, RequestDraft, Workspace},
    persistence::{EnvironmentStore, HistoryStore},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum AgentCommand {
    ListRequests,
    GetRequest {
        request_id: String,
    },
    CreateRequest {
        name: Option<String>,
        method: Option<HttpMethod>,
        url: Option<String>,
    },
    SelectRequest {
        request_id: String,
    },
    SetMethod {
        request_id: String,
        method: HttpMethod,
    },
    SetUrl {
        request_id: String,
        url: String,
    },
    SetBody {
        request_id: String,
        body: String,
    },
    SetHeader {
        request_id: String,
        key: String,
        value: String,
        enabled: Option<bool>,
    },
    SetParameter {
        request_id: String,
        key: String,
        value: String,
        enabled: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReply {
    pub ok: bool,
    pub message: String,
    pub data: Option<Value>,
}

impl AgentReply {
    pub(crate) fn success(message: impl Into<String>, data: impl Serialize) -> Self {
        Self {
            ok: true,
            message: message.into(),
            data: serde_json::to_value(data).ok(),
        }
    }

    pub(crate) fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            data: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ThemeAgentCommand {
    GetTheme,
    GetThemeSchema,
    PreviewTheme { theme_toml: String },
    SaveTheme { theme_toml: String },
    ImportTheme { path: PathBuf, save: Option<bool> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum EnvironmentAgentCommand {
    ListEnvironments,
    CreateEnvironment {
        name: String,
    },
    AssignRequest {
        environment_id: String,
        request_id: String,
    },
    RemoveRequest {
        request_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum HistoryAgentCommand {
    ListHistory { limit: Option<usize> },
    GetHistoryEntry { history_id: String },
}

/// Applies an agent-authored theme through the same parser and validation path
/// as the native settings dialog. `PreviewTheme` updates only the live state;
/// `SaveTheme` and a saving import atomically replace the configured TOML file.
pub fn apply_theme_agent_command(
    current: &mut AppConfig,
    config_path: &Path,
    command: ThemeAgentCommand,
) -> AgentReply {
    match command {
        ThemeAgentCommand::GetTheme => AgentReply::success(
            "active theme returned",
            json!({
                "config_path": config_path,
                "theme": current,
                "theme_toml": current.to_toml().ok(),
            }),
        ),
        ThemeAgentCommand::GetThemeSchema => {
            AgentReply::success("theme authoring schema returned", theme_authoring_schema())
        }
        ThemeAgentCommand::PreviewTheme { theme_toml } => match AppConfig::from_toml(&theme_toml) {
            Ok(mut theme) => {
                theme.application = current.application.clone();
                theme.agent = current.agent.clone();
                *current = theme;
                AgentReply::success(
                    "theme preview applied; call save_theme to persist it",
                    current,
                )
            }
            Err(error) => AgentReply::error(format!("theme rejected: {error:#}")),
        },
        ThemeAgentCommand::SaveTheme { theme_toml } => match AppConfig::from_toml(&theme_toml) {
            Ok(mut theme) => {
                theme.application = current.application.clone();
                theme.agent = current.agent.clone();
                match theme.save(config_path) {
                    Ok(()) => {
                        *current = theme;
                        AgentReply::success(
                            format!("theme saved to {}", config_path.display()),
                            current,
                        )
                    }
                    Err(error) => AgentReply::error(format!("failed to save theme: {error:#}")),
                }
            }
            Err(error) => AgentReply::error(format!("theme rejected: {error:#}")),
        },
        ThemeAgentCommand::ImportTheme { path, save } => match import_editor_theme(&path) {
            Ok(mut theme) => {
                theme.application = current.application.clone();
                theme.agent = current.agent.clone();
                if save.unwrap_or(false)
                    && let Err(error) = theme.save(config_path)
                {
                    return AgentReply::error(format!("failed to save imported theme: {error:#}"));
                }
                *current = theme;
                AgentReply::success(format!("theme imported from {}", path.display()), current)
            }
            Err(error) => AgentReply::error(format!("theme import failed: {error:#}")),
        },
    }
}

pub fn theme_authoring_schema() -> Value {
    let example = AppConfig::default().to_toml().unwrap_or_default();
    json!({
        "format": "TOML",
        "version": 1,
        "requirements": [
            "Return a complete configuration, including [theme], [theme.colors], and [syntax]. Application and agent settings are preserved by theme tools.",
            "Every color must be #RRGGBB or #RRGGBBAA.",
            "Keep text/background and accent/foreground pairs accessible.",
            "Theme mode must be light or dark."
        ],
        "example": example
    })
}

pub fn apply_agent_command(workspace: &mut Workspace, command: AgentCommand) -> AgentReply {
    match command {
        AgentCommand::ListRequests => AgentReply::success("requests listed", &workspace.requests),
        AgentCommand::GetRequest { request_id } => workspace
            .requests
            .iter()
            .find(|request| request.id == request_id)
            .map(|request| AgentReply::success("request found", request))
            .unwrap_or_else(|| AgentReply::error("request not found")),
        AgentCommand::CreateRequest { name, method, url } => {
            let mut request = RequestDraft::default();
            if let Some(name) = name {
                request.name = name;
            }
            if let Some(method) = method {
                request.method = method;
            }
            if let Some(url) = url {
                request.url = url;
            }
            let id = workspace.add_request(request);
            AgentReply::success("request created", json!({ "request_id": id }))
        }
        AgentCommand::SelectRequest { request_id } => {
            if workspace
                .requests
                .iter()
                .any(|request| request.id == request_id)
            {
                workspace.active_request_id = request_id;
                AgentReply::success("request selected", json!({}))
            } else {
                AgentReply::error("request not found")
            }
        }
        AgentCommand::SetMethod { request_id, method } => {
            update_request(workspace, &request_id, |request| request.method = method)
        }
        AgentCommand::SetUrl { request_id, url } => {
            update_request(workspace, &request_id, |request| request.url = url)
        }
        AgentCommand::SetBody { request_id, body } => {
            update_request(workspace, &request_id, |request| request.body = body)
        }
        AgentCommand::SetHeader {
            request_id,
            key,
            value,
            enabled,
        } => update_request(workspace, &request_id, |request| {
            upsert_field(&mut request.headers, key, value, enabled)
        }),
        AgentCommand::SetParameter {
            request_id,
            key,
            value,
            enabled,
        } => update_request(workspace, &request_id, |request| {
            upsert_field(&mut request.parameters, key, value, enabled)
        }),
    }
}

pub fn apply_environment_agent_command(
    workspace: &Workspace,
    environments: &mut EnvironmentStore,
    command: EnvironmentAgentCommand,
) -> AgentReply {
    match command {
        EnvironmentAgentCommand::ListEnvironments => {
            AgentReply::success("environments listed", &environments.environments)
        }
        EnvironmentAgentCommand::CreateEnvironment { name } => {
            let name = name.trim();
            if name.is_empty() {
                return AgentReply::error("environment name cannot be empty");
            }
            let id = environments.create(name);
            AgentReply::success("environment created", json!({ "environment_id": id }))
        }
        EnvironmentAgentCommand::AssignRequest {
            environment_id,
            request_id,
        } => {
            let Some(request) = workspace
                .requests
                .iter()
                .find(|request| request.id == request_id)
                .cloned()
            else {
                return AgentReply::error("request not found");
            };
            match environments.assign(&environment_id, request) {
                Ok(()) => AgentReply::success(
                    "request assigned to environment",
                    json!({ "environment_id": environment_id, "request_id": request_id }),
                ),
                Err(error) => AgentReply::error(error.to_string()),
            }
        }
        EnvironmentAgentCommand::RemoveRequest { request_id } => {
            if environments.remove_request(&request_id) {
                AgentReply::success(
                    "request removed from environment",
                    json!({ "request_id": request_id }),
                )
            } else {
                AgentReply::error("request is not assigned to an environment")
            }
        }
    }
}

pub fn apply_history_agent_command(
    history: &HistoryStore,
    command: HistoryAgentCommand,
) -> AgentReply {
    match command {
        HistoryAgentCommand::ListHistory { limit } => {
            let limit = limit.unwrap_or(50).min(500);
            AgentReply::success(
                "history listed",
                history.entries.iter().take(limit).collect::<Vec<_>>(),
            )
        }
        HistoryAgentCommand::GetHistoryEntry { history_id } => history
            .entries
            .iter()
            .find(|entry| entry.id == history_id)
            .map(|entry| AgentReply::success("history entry found", entry))
            .unwrap_or_else(|| AgentReply::error("history entry not found")),
    }
}

fn update_request(
    workspace: &mut Workspace,
    request_id: &str,
    update: impl FnOnce(&mut RequestDraft),
) -> AgentReply {
    match workspace
        .requests
        .iter_mut()
        .find(|request| request.id == request_id)
    {
        Some(request) => {
            update(request);
            AgentReply::success("request updated", request)
        }
        None => AgentReply::error("request not found"),
    }
}

fn upsert_field(
    fields: &mut Vec<KeyValueField>,
    key: String,
    value: String,
    enabled: Option<bool>,
) {
    if let Some(field) = fields.iter_mut().find(|field| field.key == key) {
        field.value = value;
        if let Some(enabled) = enabled {
            field.enabled = enabled;
        }
    } else {
        let mut field = KeyValueField::new(key, value);
        field.enabled = enabled.unwrap_or(true);
        fields.push(field);
    }
}

/// MCP-compatible tool descriptors. A transport adapter can publish these over
/// stdio or Streamable HTTP without coupling the app core to a model vendor.
pub fn mcp_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "list_requests",
            "description": "List open Alula HTTP requests and their stable IDs.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "create_request",
            "description": "Create and select a new request tab.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] },
                    "url": { "type": "string" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "update_request",
            "description": "Update the method, URL, body, header, or query parameter of an open request.",
            "inputSchema": {
                "type": "object",
                "required": ["request_id", "patch"],
                "properties": {
                    "request_id": { "type": "string" },
                    "patch": { "type": "object" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "send_request",
            "description": "Send a request. This is a sensitive action and should require user approval by default.",
            "inputSchema": {
                "type": "object",
                "required": ["request_id"],
                "properties": { "request_id": { "type": "string" } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "openWorldHint": true }
        }),
        json!({
            "name": "list_environments",
            "description": "List persistent Alula environments and their organized request snapshots.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "create_environment",
            "description": "Create a persistent named environment for organizing requests.",
            "inputSchema": {
                "type": "object", "required": ["name"],
                "properties": { "name": { "type": "string" } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "assign_request_to_environment",
            "description": "Move an open request's saved snapshot into an existing persistent environment. History is not changed.",
            "inputSchema": {
                "type": "object", "required": ["environment_id", "request_id"],
                "properties": {
                    "environment_id": { "type": "string" },
                    "request_id": { "type": "string" }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "remove_request_from_environment",
            "description": "Remove a request from its environment without closing its tab or changing history.",
            "inputSchema": {
                "type": "object", "required": ["request_id"],
                "properties": { "request_id": { "type": "string" } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "list_history",
            "description": "List persistent immutable request execution history, newest first. Response bodies are intentionally excluded.",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "get_history_entry",
            "description": "Read one persistent request execution history entry by stable ID.",
            "inputSchema": {
                "type": "object", "required": ["history_id"],
                "properties": { "history_id": { "type": "string" } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "get_theme",
            "description": "Read Alula's active UI and syntax-highlighting theme as structured data and TOML. Use this before designing a variant.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "get_theme_schema",
            "description": "Get the complete authoring contract and an example TOML theme. Agents should use this before creating a theme.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "preview_theme",
            "description": "Validate and preview a complete agent-created Alula TOML theme without persisting it. UI colors and syntax highlighting use the same theme.",
            "inputSchema": {
                "type": "object",
                "required": ["theme_toml"],
                "properties": { "theme_toml": { "type": "string", "description": "Complete Alula theme TOML." } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "save_theme",
            "description": "Validate, apply, and atomically save a complete agent-created Alula TOML theme.",
            "inputSchema": {
                "type": "object",
                "required": ["theme_toml"],
                "properties": { "theme_toml": { "type": "string", "description": "Complete Alula theme TOML." } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "import_theme",
            "description": "Import an Alula TOML, VS Code JSON, or Zed JSON theme from a local path and optionally persist it.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string" },
                    "save": { "type": "boolean", "default": false }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "openWorldHint": false }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_edit_requests_by_stable_id() {
        let mut workspace = Workspace::default();
        let id = workspace.active_request_id.clone();
        let reply = apply_agent_command(
            &mut workspace,
            AgentCommand::SetHeader {
                request_id: id.clone(),
                key: "X-Agent".into(),
                value: "codex".into(),
                enabled: None,
            },
        );

        assert!(reply.ok);
        assert!(
            workspace
                .requests
                .iter()
                .find(|request| request.id == id)
                .unwrap()
                .headers
                .iter()
                .any(|header| header.key == "X-Agent" && header.value == "codex")
        );
    }

    #[test]
    fn agent_can_preview_and_save_a_theme() {
        let directory =
            std::env::temp_dir().join(format!("alula-agent-theme-{}", std::process::id()));
        let path = directory.join("config.toml");
        let mut theme = AppConfig::default();
        theme.agent.port = 43_210;
        let mut authored = AppConfig::default();
        authored.theme.name = "Agent Violet".into();
        authored.theme.colors.accent = "#8b5cf6".into();
        authored.syntax.keyword = "#c084fc".into();
        let source = authored.to_toml().unwrap();

        let preview = apply_theme_agent_command(
            &mut theme,
            &path,
            ThemeAgentCommand::PreviewTheme {
                theme_toml: source.clone(),
            },
        );
        assert!(preview.ok);
        assert_eq!(theme.theme.name, "Agent Violet");
        assert_eq!(theme.agent.port, 43_210);
        assert!(!path.exists());

        let save = apply_theme_agent_command(
            &mut theme,
            &path,
            ThemeAgentCommand::SaveTheme { theme_toml: source },
        );
        assert!(save.ok);
        let saved = AppConfig::load(&path).unwrap();
        assert_eq!(saved.theme, authored.theme);
        assert_eq!(saved.syntax, authored.syntax);
        assert_eq!(saved.agent.port, 43_210);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn agent_environment_commands_do_not_mutate_history() {
        let workspace = Workspace::default();
        let request_id = workspace.active_request_id.clone();
        let mut environments = EnvironmentStore::default();
        let history = HistoryStore::default();
        let created = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::CreateEnvironment {
                name: "Staging".into(),
            },
        );
        let environment_id = created.data.unwrap()["environment_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let assigned = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::AssignRequest {
                environment_id,
                request_id,
            },
        );
        assert!(assigned.ok);
        assert_eq!(environments.environments[0].requests.len(), 1);
        assert!(history.entries.is_empty());
    }
}
