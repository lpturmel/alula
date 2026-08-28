use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use alula::{
    AgentReply, LATEST_PROTOCOL_VERSION, McpHttpServer, install_tls_crypto_provider, reply_to_tool,
};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN},
};
use serde_json::{Value, json};

fn post(client: &Client, endpoint: &str, message: Value) -> Response {
    client
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", LATEST_PROTOCOL_VERSION)
        .json(&message)
        .send()
        .unwrap()
}

#[test]
fn embedded_streamable_http_obeys_transport_and_dispatches_tools() {
    install_tls_crypto_provider();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let config_path = std::env::temp_dir()
        .join(format!("alula-mcp-http-{nonce}"))
        .join("config.toml");
    let calls = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let captured = calls.clone();
    let server = McpHttpServer::start(
        0,
        config_path,
        Some(Arc::new(move |name, arguments| {
            captured
                .lock()
                .unwrap()
                .push((name.to_owned(), arguments.clone()));
            reply_to_tool(AgentReply::success(
                "live UI handled the call",
                json!({ "name": name, "arguments": arguments }),
            ))
        })),
    )
    .unwrap();
    let endpoint = server.endpoint();
    let health = endpoint.replace("/mcp", "/health");
    let client = Client::builder().build().unwrap();

    let health_response = client.get(&health).send().unwrap();
    assert_eq!(health_response.status(), StatusCode::OK);
    assert_eq!(
        health_response.json::<Value>().unwrap()["transport"],
        "streamable-http"
    );

    let get_response = client.get(&endpoint).send().unwrap();
    assert_eq!(get_response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(get_response.headers()["allow"], "POST");

    let initialized = post(
        &client,
        &endpoint,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "alula-http-test", "version": "1" }
            }
        }),
    );
    assert_eq!(initialized.status(), StatusCode::OK);
    let initialized = initialized.json::<Value>().unwrap();
    assert_eq!(
        initialized["result"]["protocolVersion"],
        LATEST_PROTOCOL_VERSION
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "alula");

    let notification = post(
        &client,
        &endpoint,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );
    assert_eq!(notification.status(), StatusCode::ACCEPTED);
    assert_eq!(notification.content_length(), Some(0));

    let tools = post(
        &client,
        &endpoint,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    )
    .json::<Value>()
    .unwrap();
    let tool_names = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"create_request"));
    assert!(tool_names.contains(&"send_request"));
    assert!(tool_names.contains(&"set_environment_variable"));
    assert!(tool_names.contains(&"create_environment_folder"));
    assert!(tool_names.contains(&"rename_environment_folder"));
    assert!(tool_names.contains(&"delete_environment_folder"));
    assert!(tool_names.contains(&"save_theme"));

    let call = post(
        &client,
        &endpoint,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "create_request",
                "arguments": { "name": "From agent", "url": "https://example.com" }
            }
        }),
    )
    .json::<Value>()
    .unwrap();
    assert_eq!(call["result"]["isError"], false);
    assert_eq!(
        call["result"]["structuredContent"]["name"],
        "create_request"
    );
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[(
            String::from("create_request"),
            json!({
                "name": "From agent",
                "url": "https://example.com"
            })
        )]
    );

    let bad_version = client
        .post(&endpoint)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "1900-01-01")
        .json(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": {}
        }))
        .send()
        .unwrap();
    assert_eq!(bad_version.status(), StatusCode::BAD_REQUEST);

    let mut hostile_headers = HeaderMap::new();
    hostile_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    hostile_headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    hostile_headers.insert(ORIGIN, HeaderValue::from_static("https://attacker.example"));
    let hostile_origin = client
        .post(&endpoint)
        .headers(hostile_headers)
        .json(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/list", "params": {}
        }))
        .send()
        .unwrap();
    assert_eq!(hostile_origin.status(), StatusCode::FORBIDDEN);
}
