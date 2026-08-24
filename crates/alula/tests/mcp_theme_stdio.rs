use std::{
    io::BufReader,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use alula::{
    AppConfig, EnvironmentStore, HistoryEntry, HistoryStore, StatePaths, Workspace,
    load_or_default, save_toml,
};
use serde_json::{Value, json};

fn call(
    stdin: &mut impl std::io::Write,
    stdout: &mut impl std::io::BufRead,
    request: Value,
) -> Value {
    writeln!(stdin, "{request}").unwrap();
    stdin.flush().unwrap();
    let mut response = String::new();
    stdout.read_line(&mut response).unwrap();
    serde_json::from_str(&response).unwrap()
}

#[test]
fn agent_creates_previews_saves_and_reads_theme_over_mcp_stdio() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("alula-mcp-stdio-{nonce}"));
    let path = directory.join("config.toml");
    let state_paths = StatePaths::beside(&path);
    let workspace = Workspace::default();
    let request_id = workspace.active_request_id.clone();
    save_toml(&state_paths.workspace, &workspace).unwrap();
    let mut history = HistoryStore::default();
    history.push(HistoryEntry::failure(
        workspace.active().unwrap().clone(),
        "fixture failure",
    ));
    let history_id = history.entries[0].id.clone();
    save_toml(&state_paths.history, &history).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_alula-mcp"))
        .env("ALULA_CONFIG", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let initialized = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "alula-test-agent", "version": "1" } }
        }),
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "alula");

    let tools = call(
        &mut stdin,
        &mut stdout,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    );
    let names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"get_theme_schema"));
    assert!(names.contains(&"preview_theme"));
    assert!(names.contains(&"save_theme"));
    assert!(names.contains(&"create_environment"));
    assert!(names.contains(&"delete_environment"));
    assert!(names.contains(&"assign_request_to_environment"));
    assert!(names.contains(&"list_history"));
    assert!(names.contains(&"delete_history_entry"));

    let schema = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "get_theme_schema", "arguments": {} }
        }),
    );
    assert_eq!(schema["result"]["isError"], false);
    assert!(
        schema["result"]["structuredContent"]["example"]
            .as_str()
            .unwrap()
            .contains("[syntax]")
    );

    let mut authored = AppConfig::default();
    authored.theme.name = "Agent Solarized Flight".into();
    authored.theme.colors.accent = "#268bd2".into();
    authored.syntax.keyword = "#859900".into();
    let theme_toml = authored.to_toml().unwrap();

    let preview = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "preview_theme", "arguments": { "theme_toml": theme_toml } }
        }),
    );
    assert_eq!(preview["result"]["isError"], false);
    assert!(!path.exists(), "preview must not persist the theme");

    let saved = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "save_theme", "arguments": { "theme_toml": authored.to_toml().unwrap() } }
        }),
    );
    assert_eq!(saved["result"]["isError"], false);
    assert_eq!(AppConfig::load(&path).unwrap(), authored);

    let read_back = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "name": "get_theme", "arguments": {} }
        }),
    );
    assert_eq!(
        read_back["result"]["structuredContent"]["theme"]["theme"]["name"],
        "Agent Solarized Flight"
    );

    let created = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": "create_environment", "arguments": { "name": "Agent staging" } }
        }),
    );
    assert_eq!(created["result"]["isError"], false);
    let environment_id = created["result"]["structuredContent"]["environment_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let assigned = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 8, "method": "tools/call",
            "params": {
                "name": "assign_request_to_environment",
                "arguments": { "environment_id": environment_id, "request_id": request_id }
            }
        }),
    );
    assert_eq!(assigned["result"]["isError"], false);

    let environments = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": { "name": "list_environments", "arguments": {} }
        }),
    );
    assert_eq!(
        environments["result"]["structuredContent"][0]["name"],
        "Agent staging"
    );
    assert_eq!(
        environments["result"]["structuredContent"][0]["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let history_over_mcp = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": { "name": "list_history", "arguments": { "limit": 10 } }
        }),
    );
    assert_eq!(history_over_mcp["result"]["isError"], false);
    assert_eq!(
        history_over_mcp["result"]["structuredContent"][0]["error"],
        "fixture failure"
    );

    let deleted_history = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 11, "method": "tools/call",
            "params": { "name": "delete_history_entry", "arguments": { "history_id": history_id } }
        }),
    );
    assert_eq!(deleted_history["result"]["isError"], false);

    let deleted_environment = call(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0", "id": 12, "method": "tools/call",
            "params": { "name": "delete_environment", "arguments": { "environment_id": environment_id } }
        }),
    );
    assert_eq!(deleted_environment["result"]["isError"], false);
    let saved_history: HistoryStore = load_or_default(&state_paths.history).unwrap();
    let saved_environments: EnvironmentStore = load_or_default(&state_paths.environments).unwrap();
    assert!(saved_history.entries.is_empty());
    assert!(saved_environments.environments.is_empty());
    assert!(state_paths.workspace.exists());
    assert!(state_paths.history.exists());
    assert!(state_paths.environments.exists());

    drop(stdin);
    assert!(child.wait().unwrap().success());
    std::fs::remove_dir_all(&directory).unwrap();
}
