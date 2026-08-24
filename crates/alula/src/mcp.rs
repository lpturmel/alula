use std::{path::PathBuf, sync::Arc};

use serde_json::{Value, json};

use crate::{
    AppConfig, EnvironmentAgentCommand, EnvironmentStore, HistoryAgentCommand, HistoryStore,
    StatePaths, ThemeAgentCommand, Workspace, apply_environment_agent_command,
    apply_history_agent_command, apply_theme_agent_command, config_path, load_or_default,
    mcp_tools, save_toml,
};

/// Transport-independent MCP JSON-RPC dispatcher. The desktop process can own
/// one directly; `alula-mcp` wraps it with the newline-delimited stdio transport.
pub struct McpServer {
    theme: AppConfig,
    config_path: PathBuf,
    state_paths: StatePaths,
    workspace: Workspace,
    environments: EnvironmentStore,
    history: HistoryStore,
    tool_handler: Option<McpToolHandler>,
}

pub type McpToolHandler = Arc<dyn Fn(&str, Value) -> Value + Send + Sync>;

pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-03-26", "2025-06-18", LATEST_PROTOCOL_VERSION];

impl Default for McpServer {
    fn default() -> Self {
        Self::new(config_path())
    }
}

impl McpServer {
    pub fn new(config_path: PathBuf) -> Self {
        let theme = AppConfig::load(&config_path).unwrap_or_default();
        let state_paths = StatePaths::beside(&config_path);
        let workspace = load_or_default::<Workspace>(&state_paths.workspace)
            .unwrap_or_default()
            .normalize();
        let environments = load_or_default(&state_paths.environments).unwrap_or_default();
        let history = load_or_default(&state_paths.history).unwrap_or_default();
        Self {
            theme,
            config_path,
            state_paths,
            workspace,
            environments,
            history,
            tool_handler: None,
        }
    }

    pub fn with_tool_handler(mut self, handler: McpToolHandler) -> Self {
        self.tool_handler = Some(handler);
        self
    }

    pub fn theme(&self) -> &AppConfig {
        &self.theme
    }

    /// Handles one JSON-RPC message. Notifications intentionally return None.
    pub fn handle(&mut self, source: &str) -> Option<String> {
        let request: Value = match serde_json::from_str(source) {
            Ok(request) => request,
            Err(error) => {
                return Some(rpc_error(Value::Null, -32700, error.to_string()).to_string());
            }
        };
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_none() {
            return None;
        }
        let id = id.unwrap_or(Value::Null);
        let response = match method {
            "initialize" => {
                let requested_version = request
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(LATEST_PROTOCOL_VERSION);
                let negotiated_version = SUPPORTED_PROTOCOL_VERSIONS
                    .contains(&requested_version)
                    .then_some(requested_version)
                    .unwrap_or(LATEST_PROTOCOL_VERSION);
                rpc_result(
                    id,
                    json!({
                        "protocolVersion": negotiated_version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "alula", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": "Workspace tabs, execution history, and environments are separate persistent data sets. Use get_theme_schema before authoring themes; preview first, then save only when the user asks to persist."
                    }),
                )
            }
            "tools/list" => rpc_result(id, json!({ "tools": mcp_tools() })),
            "tools/call" => {
                let Some(name) = request.pointer("/params/name").and_then(Value::as_str) else {
                    return Some(rpc_error(id, -32602, "missing tool name").to_string());
                };
                let arguments = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                rpc_result(id, self.call_tool(name, arguments))
            }
            _ => rpc_error(id, -32601, format!("method not found: {method}")),
        };
        Some(response.to_string())
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        if let Some(handler) = self.tool_handler.clone() {
            return handler(name, arguments);
        }
        if name == "get_theme"
            && self.config_path.exists()
            && let Ok(theme) = AppConfig::load(&self.config_path)
        {
            self.theme = theme;
        }
        match name {
            "list_requests" => {
                self.reload_workspace();
                return reply_to_tool(crate::AgentReply::success(
                    "requests listed",
                    &self.workspace.requests,
                ));
            }
            "list_environments" => {
                self.reload_environments();
                return reply_to_tool(apply_environment_agent_command(
                    &self.workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::ListEnvironments,
                ));
            }
            "create_environment" => {
                let name = match required_string(&arguments, "name") {
                    Ok(name) => name,
                    Err(error) => return tool_error(error),
                };
                self.reload_environments();
                let reply = apply_environment_agent_command(
                    &self.workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::CreateEnvironment { name },
                );
                if reply.ok
                    && let Err(error) =
                        save_toml(&self.state_paths.environments, &self.environments)
                {
                    return tool_error(format!("failed to save environments: {error:#}"));
                }
                return reply_to_tool(reply);
            }
            "delete_environment" => {
                let environment_id = match required_string(&arguments, "environment_id") {
                    Ok(id) => id,
                    Err(error) => return tool_error(error),
                };
                self.reload_environments();
                let reply = apply_environment_agent_command(
                    &self.workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::DeleteEnvironment { environment_id },
                );
                if reply.ok
                    && let Err(error) =
                        save_toml(&self.state_paths.environments, &self.environments)
                {
                    return tool_error(format!("failed to save environments: {error:#}"));
                }
                return reply_to_tool(reply);
            }
            "set_environment_variable" => {
                let environment_id = match required_string(&arguments, "environment_id") {
                    Ok(id) => id,
                    Err(error) => return tool_error(error),
                };
                let name = match required_string(&arguments, "name") {
                    Ok(name) => name,
                    Err(error) => return tool_error(error),
                };
                let value = match required_string(&arguments, "value") {
                    Ok(value) => value,
                    Err(error) => return tool_error(error),
                };
                if arguments
                    .get("secret")
                    .is_some_and(|value| !value.is_boolean())
                {
                    return tool_error("secret must be a boolean");
                }
                self.reload_workspace();
                self.reload_environments();
                let reply = apply_environment_agent_command(
                    &self.workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::SetVariable {
                        environment_id,
                        name,
                        value,
                        secret: arguments.get("secret").and_then(Value::as_bool),
                    },
                );
                if reply.ok
                    && let Err(error) =
                        save_toml(&self.state_paths.environments, &self.environments)
                {
                    return tool_error(format!("failed to save environments: {error:#}"));
                }
                return reply_to_tool(reply);
            }
            "assign_request_to_environment" => {
                let environment_id = match required_string(&arguments, "environment_id") {
                    Ok(id) => id,
                    Err(error) => return tool_error(error),
                };
                let request_id = match required_string(&arguments, "request_id") {
                    Ok(id) => id,
                    Err(error) => return tool_error(error),
                };
                self.reload_workspace();
                self.reload_environments();
                let reply = apply_environment_agent_command(
                    &self.workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::AssignRequest {
                        environment_id,
                        request_id,
                    },
                );
                if reply.ok
                    && let Err(error) =
                        save_toml(&self.state_paths.environments, &self.environments)
                {
                    return tool_error(format!("failed to save environments: {error:#}"));
                }
                return reply_to_tool(reply);
            }
            "remove_request_from_environment" => {
                let request_id = match required_string(&arguments, "request_id") {
                    Ok(id) => id,
                    Err(error) => return tool_error(error),
                };
                self.reload_environments();
                let reply = apply_environment_agent_command(
                    &self.workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::RemoveRequest { request_id },
                );
                if reply.ok
                    && let Err(error) =
                        save_toml(&self.state_paths.environments, &self.environments)
                {
                    return tool_error(format!("failed to save environments: {error:#}"));
                }
                return reply_to_tool(reply);
            }
            "list_history" => {
                self.reload_history();
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|limit| limit as usize);
                return reply_to_tool(apply_history_agent_command(
                    &mut self.history,
                    HistoryAgentCommand::ListHistory { limit },
                ));
            }
            "get_history_entry" => {
                let history_id = match required_string(&arguments, "history_id") {
                    Ok(id) => id,
                    Err(error) => return tool_error(error),
                };
                self.reload_history();
                return reply_to_tool(apply_history_agent_command(
                    &mut self.history,
                    HistoryAgentCommand::GetHistoryEntry { history_id },
                ));
            }
            "delete_history_entry" => {
                let history_id = match required_string(&arguments, "history_id") {
                    Ok(id) => id,
                    Err(error) => return tool_error(error),
                };
                self.reload_history();
                let reply = apply_history_agent_command(
                    &mut self.history,
                    HistoryAgentCommand::DeleteHistoryEntry { history_id },
                );
                if reply.ok
                    && let Err(error) = save_toml(&self.state_paths.history, &self.history)
                {
                    return tool_error(format!("failed to save history: {error:#}"));
                }
                return reply_to_tool(reply);
            }
            _ => {}
        }

        let command = match name {
            "get_theme" => ThemeAgentCommand::GetTheme,
            "get_theme_schema" => ThemeAgentCommand::GetThemeSchema,
            "preview_theme" => match required_string(&arguments, "theme_toml") {
                Ok(theme_toml) => ThemeAgentCommand::PreviewTheme { theme_toml },
                Err(error) => return tool_error(error),
            },
            "save_theme" => match required_string(&arguments, "theme_toml") {
                Ok(theme_toml) => ThemeAgentCommand::SaveTheme { theme_toml },
                Err(error) => return tool_error(error),
            },
            "import_theme" => {
                let path = match required_string(&arguments, "path") {
                    Ok(path) => PathBuf::from(path),
                    Err(error) => return tool_error(error),
                };
                ThemeAgentCommand::ImportTheme {
                    path,
                    save: arguments.get("save").and_then(Value::as_bool),
                }
            }
            _ => {
                return tool_error(format!(
                    "tool is not available in the standalone Alula bridge: {name}"
                ));
            }
        };
        let reply = apply_theme_agent_command(&mut self.theme, &self.config_path, command);
        reply_to_tool(reply)
    }

    fn reload_workspace(&mut self) {
        if let Ok(workspace) = load_or_default::<Workspace>(&self.state_paths.workspace) {
            self.workspace = workspace.normalize();
        }
    }

    fn reload_environments(&mut self) {
        if let Ok(environments) = load_or_default(&self.state_paths.environments) {
            self.environments = environments;
        }
    }

    fn reload_history(&mut self) {
        if let Ok(history) = load_or_default(&self.state_paths.history) {
            self.history = history;
        }
    }
}

pub fn reply_to_tool(reply: crate::AgentReply) -> Value {
    let text = reply.message.clone();
    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": !reply.ok
    });
    if let Some(data) = reply.data {
        result["structuredContent"] = data;
    }
    result
}

fn required_string(arguments: &Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{key} must be a string"))
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true
    })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_mcp_theme_preview_and_save() {
        let directory = std::env::temp_dir().join(format!("alula-mcp-{}", std::process::id()));
        let path = directory.join("config.toml");
        let mut server = McpServer::new(path.clone());

        let initialized: Value = serde_json::from_str(
            &server.handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#).unwrap()
        ).unwrap();
        assert_eq!(initialized["result"]["serverInfo"]["name"], "alula");

        let mut theme = AppConfig::default();
        theme.theme.name = "MCP Mint".into();
        theme.theme.colors.accent = "#10b981".into();
        let request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "save_theme",
                "arguments": { "theme_toml": theme.to_toml().unwrap() }
            }
        });
        let saved: Value =
            serde_json::from_str(&server.handle(&request.to_string()).unwrap()).unwrap();
        assert_eq!(saved["result"]["isError"], false);
        assert_eq!(AppConfig::load(&path).unwrap().theme.name, "MCP Mint");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn creates_environment_variables_over_mcp() {
        let directory =
            std::env::temp_dir().join(format!("alula-mcp-environment-{}", std::process::id()));
        let path = directory.join("config.toml");
        let mut server = McpServer::new(path.clone());
        let create = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_environment",
                "arguments": { "name": "Staging" }
            }
        });
        let created: Value =
            serde_json::from_str(&server.handle(&create.to_string()).unwrap()).unwrap();
        let environment_id = created["result"]["structuredContent"]["environment_id"]
            .as_str()
            .unwrap();
        let set_variable = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "set_environment_variable",
                "arguments": {
                    "environment_id": environment_id,
                    "name": "api_url",
                    "value": "https://api.stag.compile.sh"
                }
            }
        });
        let updated: Value =
            serde_json::from_str(&server.handle(&set_variable.to_string()).unwrap()).unwrap();
        assert_eq!(updated["result"]["isError"], false);

        let saved: EnvironmentStore =
            load_or_default(&StatePaths::beside(&path).environments).unwrap();
        assert_eq!(saved.environments[0].variables[0].name, "api_url");
        assert_eq!(
            saved.environments[0].variables[0].value.as_deref(),
            Some("https://api.stag.compile.sh")
        );
        let _ = std::fs::remove_dir_all(directory);
    }
}
