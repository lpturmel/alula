use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    config::{AppConfig, import_editor_theme},
    model::{HttpMethod, KeyValueField, RequestDraft, Workspace},
    persistence::{EnvironmentStore, EnvironmentVariable, HistoryStore},
    variables::{delete_secret, store_secret, valid_variable_name},
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
    pub fn success(message: impl Into<String>, data: impl Serialize) -> Self {
        Self {
            ok: true,
            message: message.into(),
            data: serde_json::to_value(data).ok(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
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
    DeleteEnvironment {
        environment_id: String,
    },
    CreateFolder {
        environment_id: String,
        parent_folder_id: Option<String>,
        name: String,
    },
    RenameFolder {
        environment_id: String,
        folder_id: String,
        name: String,
    },
    DeleteFolder {
        environment_id: String,
        folder_id: String,
    },
    SetVariable {
        environment_id: String,
        name: String,
        value: String,
        secret: Option<bool>,
    },
    AssignRequest {
        environment_id: String,
        folder_id: Option<String>,
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
    DeleteHistoryEntry { history_id: String },
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
                theme.keybindings = current.keybindings.clone();
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
                theme.keybindings = current.keybindings.clone();
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
                theme.keybindings = current.keybindings.clone();
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
            "Return a complete configuration, including [theme], [theme.colors], and [syntax]. Application, agent, and keybinding settings are preserved by theme tools.",
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
        EnvironmentAgentCommand::DeleteEnvironment { environment_id } => {
            let Some(environment) = environments
                .environments
                .iter()
                .find(|environment| environment.id == environment_id)
            else {
                return AgentReply::error("environment not found");
            };
            for variable in environment
                .variables
                .iter()
                .filter(|variable| variable.secret)
            {
                if let Err(error) = delete_secret(&environment_id, &variable.id) {
                    return AgentReply::error(format!(
                        "could not remove environment secret: {error:#}"
                    ));
                }
            }
            let environment = environments
                .remove(&environment_id)
                .expect("environment existence checked");
            AgentReply::success("environment deleted", environment)
        }
        EnvironmentAgentCommand::CreateFolder {
            environment_id,
            parent_folder_id,
            name,
        } => {
            let name = name.trim();
            if name.is_empty() {
                return AgentReply::error("folder name cannot be empty");
            }
            let duplicate = environments
                .environments
                .iter()
                .find(|environment| environment.id == environment_id)
                .and_then(|environment| {
                    environment.folder_name_exists_in(parent_folder_id.as_deref(), name)
                })
                .unwrap_or(false);
            if duplicate {
                return AgentReply::error("a folder with that name already exists");
            }
            match environments.create_folder(&environment_id, parent_folder_id.as_deref(), name) {
                Ok(folder_id) => AgentReply::success(
                    "folder created",
                    json!({
                        "environment_id": environment_id,
                        "parent_folder_id": parent_folder_id,
                        "folder_id": folder_id,
                    }),
                ),
                Err(error) => AgentReply::error(error.to_string()),
            }
        }
        EnvironmentAgentCommand::RenameFolder {
            environment_id,
            folder_id,
            name,
        } => {
            let name = name.trim();
            if name.is_empty() {
                return AgentReply::error("folder name cannot be empty");
            }
            let duplicate = environments
                .environments
                .iter()
                .find(|environment| environment.id == environment_id)
                .and_then(|environment| environment.sibling_folder_name_exists(&folder_id, name))
                .unwrap_or(false);
            if duplicate {
                return AgentReply::error("a folder with that name already exists");
            }
            match environments.rename_folder(&environment_id, &folder_id, name) {
                Ok(()) => AgentReply::success(
                    "folder renamed",
                    json!({
                        "environment_id": environment_id,
                        "folder_id": folder_id,
                        "name": name,
                    }),
                ),
                Err(error) => AgentReply::error(error.to_string()),
            }
        }
        EnvironmentAgentCommand::DeleteFolder {
            environment_id,
            folder_id,
        } => match environments.delete_folder(&environment_id, &folder_id) {
            Ok(moved_request_count) => AgentReply::success(
                "folder deleted; its contents were moved to its parent",
                json!({
                    "environment_id": environment_id,
                    "folder_id": folder_id,
                    "moved_request_count": moved_request_count,
                }),
            ),
            Err(error) => AgentReply::error(error.to_string()),
        },
        EnvironmentAgentCommand::SetVariable {
            environment_id,
            name,
            value,
            secret,
        } => {
            let name = name.trim();
            if !valid_variable_name(name) {
                return AgentReply::error(
                    "variable names must start with a letter or underscore and contain only letters, numbers, _, -, or .",
                );
            }
            let Some(environment) = environments
                .environments
                .iter_mut()
                .find(|environment| environment.id == environment_id)
            else {
                return AgentReply::error("environment not found");
            };
            let existing = environment
                .variables
                .iter()
                .find(|variable| variable.name == name)
                .cloned();
            let is_secret = secret.unwrap_or(false);
            let mut variable = if is_secret {
                EnvironmentVariable::secret(name, Some(value))
            } else {
                EnvironmentVariable::public(name, value)
            };
            if let Some(existing) = &existing {
                variable.id.clone_from(&existing.id);
            }
            if is_secret {
                if let Err(error) = store_secret(
                    &environment_id,
                    &variable.id,
                    variable.value.as_deref().unwrap_or_default(),
                ) {
                    return AgentReply::error(format!(
                        "could not store variable securely: {error:#}"
                    ));
                }
            } else if existing.as_ref().is_some_and(|variable| variable.secret)
                && let Err(error) = delete_secret(&environment_id, &variable.id)
            {
                return AgentReply::error(format!("could not remove old secret: {error:#}"));
            }
            let variable_id = variable.id.clone();
            if let Some(position) = environment
                .variables
                .iter()
                .position(|item| item.id == variable_id)
            {
                environment.variables[position] = variable;
            } else {
                environment.variables.push(variable);
            }
            AgentReply::success(
                if existing.is_some() {
                    "environment variable updated"
                } else {
                    "environment variable created"
                },
                json!({
                    "environment_id": environment_id,
                    "variable_id": variable_id,
                    "name": name,
                    "secret": is_secret,
                }),
            )
        }
        EnvironmentAgentCommand::AssignRequest {
            environment_id,
            folder_id,
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
            match environments.assign_to_folder(&environment_id, folder_id.as_deref(), request) {
                Ok(()) => AgentReply::success(
                    "request assigned to environment folder",
                    json!({
                        "environment_id": environment_id,
                        "folder_id": folder_id,
                        "request_id": request_id,
                    }),
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
    history: &mut HistoryStore,
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
        HistoryAgentCommand::DeleteHistoryEntry { history_id } => {
            if history.remove(&history_id) {
                AgentReply::success("history entry deleted", json!({ "history_id": history_id }))
            } else {
                AgentReply::error("history entry not found")
            }
        }
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

/// Number of contracts returned by [`mcp_tools`]. Kept separately so UI paint
/// paths do not allocate and construct every JSON schema merely to show a badge.
pub const MCP_TOOL_COUNT: usize = 21;

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
                    "patch": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] },
                            "url": { "type": "string" },
                            "body": { "type": "string" },
                            "headers": { "$ref": "#/$defs/keyValueFields" },
                            "parameters": { "$ref": "#/$defs/keyValueFields" }
                        },
                        "additionalProperties": false
                    }
                },
                "additionalProperties": false,
                "$defs": {
                    "keyValueFields": {
                        "oneOf": [
                            {
                                "type": "object",
                                "additionalProperties": { "type": "string" }
                            },
                            {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "required": ["key", "value"],
                                    "properties": {
                                        "key": { "type": "string" },
                                        "value": { "type": "string" },
                                        "enabled": { "type": "boolean", "default": true }
                                    },
                                    "additionalProperties": false
                                }
                            }
                        ]
                    }
                }
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
            "name": "delete_environment",
            "description": "Permanently delete an environment, its saved request snapshots, variables, and stored secrets.",
            "inputSchema": {
                "type": "object", "required": ["environment_id"],
                "properties": { "environment_id": { "type": "string" } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "create_environment_folder",
            "description": "Create a root request folder or a nested folder inside an existing environment folder.",
            "inputSchema": {
                "type": "object", "required": ["environment_id", "name"],
                "properties": {
                    "environment_id": { "type": "string" },
                    "parent_folder_id": { "type": "string", "description": "Optional parent folder ID. Omit to create a root folder." },
                    "name": { "type": "string" }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "rename_environment_folder",
            "description": "Rename a request folder inside an environment.",
            "inputSchema": {
                "type": "object", "required": ["environment_id", "folder_id", "name"],
                "properties": {
                    "environment_id": { "type": "string" },
                    "folder_id": { "type": "string" },
                    "name": { "type": "string" }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "delete_environment_folder",
            "description": "Delete a folder and promote its requests and child folders to its parent without closing tabs or changing history.",
            "inputSchema": {
                "type": "object", "required": ["environment_id", "folder_id"],
                "properties": {
                    "environment_id": { "type": "string" },
                    "folder_id": { "type": "string" }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "set_environment_variable",
            "description": "Create or update a variable in an environment. Secret values are stored in the OS credential store and are never written to environment files.",
            "inputSchema": {
                "type": "object", "required": ["environment_id", "name", "value"],
                "properties": {
                    "environment_id": { "type": "string" },
                    "name": { "type": "string", "pattern": "^[A-Za-z_][A-Za-z0-9_.-]*$" },
                    "value": { "type": "string" },
                    "secret": { "type": "boolean", "default": false }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "assign_request_to_environment",
            "description": "Move an open request's saved snapshot into an environment root or one of its folders. History is not changed.",
            "inputSchema": {
                "type": "object", "required": ["environment_id", "request_id"],
                "properties": {
                    "environment_id": { "type": "string" },
                    "folder_id": { "type": "string", "description": "Optional folder ID. Omit to place the request at the environment root." },
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
            "name": "delete_history_entry",
            "description": "Permanently delete one request execution history entry by stable ID.",
            "inputSchema": {
                "type": "object", "required": ["history_id"],
                "properties": { "history_id": { "type": "string" } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false }
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
    fn advertised_tool_count_stays_in_sync() {
        assert_eq!(mcp_tools().len(), MCP_TOOL_COUNT);
    }

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
        theme.keybindings.send_request = "secondary-shift-enter".into();
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
        assert_eq!(theme.keybindings.send_request, "secondary-shift-enter");
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
        assert_eq!(saved.keybindings.send_request, "secondary-shift-enter");
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
                folder_id: None,
                request_id,
            },
        );
        assert!(assigned.ok);
        assert_eq!(environments.environments[0].requests.len(), 1);
        assert!(history.entries.is_empty());
    }

    #[test]
    fn agent_can_create_and_update_public_environment_variables() {
        let workspace = Workspace::default();
        let mut environments = EnvironmentStore::default();
        let environment_id = environments.create("Staging");

        let created = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::SetVariable {
                environment_id: environment_id.clone(),
                name: "api_url".into(),
                value: "https://api.stag.example.com".into(),
                secret: Some(false),
            },
        );
        assert!(created.ok);
        let variable_id = created.data.unwrap()["variable_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let updated = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::SetVariable {
                environment_id,
                name: "api_url".into(),
                value: "https://api.stag.compile.sh".into(),
                secret: None,
            },
        );
        assert!(updated.ok);
        assert_eq!(environments.environments[0].variables.len(), 1);
        assert_eq!(environments.environments[0].variables[0].id, variable_id);
        assert_eq!(
            environments.environments[0].variables[0].value.as_deref(),
            Some("https://api.stag.compile.sh")
        );
    }

    #[test]
    fn agent_can_manage_folders_and_place_requests() {
        let workspace = Workspace::default();
        let request_id = workspace.active_request_id.clone();
        let mut environments = EnvironmentStore::default();
        let environment_id = environments.create("Staging");

        let created = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::CreateFolder {
                environment_id: environment_id.clone(),
                parent_folder_id: None,
                name: "Authentication".into(),
            },
        );
        assert!(created.ok);
        let folder_id = created.data.unwrap()["folder_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let nested = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::CreateFolder {
                environment_id: environment_id.clone(),
                parent_folder_id: Some(folder_id.clone()),
                name: "Login".into(),
            },
        );
        assert!(nested.ok);
        let nested_folder_id = nested.data.unwrap()["folder_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let other_root = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::CreateFolder {
                environment_id: environment_id.clone(),
                parent_folder_id: None,
                name: "Backend".into(),
            },
        );
        assert!(other_root.ok);
        let other_root_id = other_root.data.unwrap()["folder_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let same_name_in_other_scope = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::CreateFolder {
                environment_id: environment_id.clone(),
                parent_folder_id: Some(other_root_id),
                name: "Login".into(),
            },
        );
        assert!(same_name_in_other_scope.ok);
        let duplicate_sibling = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::CreateFolder {
                environment_id: environment_id.clone(),
                parent_folder_id: Some(folder_id.clone()),
                name: "login".into(),
            },
        );
        assert!(!duplicate_sibling.ok);

        let assigned = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::AssignRequest {
                environment_id: environment_id.clone(),
                folder_id: Some(nested_folder_id.clone()),
                request_id: request_id.clone(),
            },
        );
        assert!(assigned.ok);
        assert_eq!(
            environments.folder_for_request(&request_id).unwrap().name,
            "Login"
        );

        let renamed = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::RenameFolder {
                environment_id: environment_id.clone(),
                folder_id: nested_folder_id.clone(),
                name: "Credentials".into(),
            },
        );
        assert!(renamed.ok);
        assert_eq!(
            environments.environments[0].folders[0].folders[0].name,
            "Credentials"
        );

        let deleted = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::DeleteFolder {
                environment_id: environment_id.clone(),
                folder_id: nested_folder_id,
            },
        );
        assert!(deleted.ok);
        assert_eq!(
            environments.folder_for_request(&request_id).unwrap().id,
            folder_id
        );
        let deleted_parent = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::DeleteFolder {
                environment_id,
                folder_id,
            },
        );
        assert!(deleted_parent.ok);
        assert!(environments.folder_for_request(&request_id).is_none());
        assert!(environments.environment_for_request(&request_id).is_some());
    }

    #[test]
    fn agent_can_delete_environments_and_history_entries() {
        let workspace = Workspace::default();
        let mut environments = EnvironmentStore::default();
        let environment_id = environments.create("Disposable");
        environments
            .assign(&environment_id, workspace.active().unwrap().clone())
            .unwrap();

        let deleted_environment = apply_environment_agent_command(
            &workspace,
            &mut environments,
            EnvironmentAgentCommand::DeleteEnvironment { environment_id },
        );
        assert!(deleted_environment.ok);
        assert!(environments.environments.is_empty());

        let mut history = HistoryStore::default();
        let entry = crate::persistence::HistoryEntry::failure(
            workspace.active().unwrap().clone(),
            "fixture failure",
        );
        let history_id = entry.id.clone();
        history.push(entry);
        let deleted_history = apply_history_agent_command(
            &mut history,
            HistoryAgentCommand::DeleteHistoryEntry { history_id },
        );
        assert!(deleted_history.ok);
        assert!(history.entries.is_empty());
    }
}
