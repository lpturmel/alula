# Alula

Alula is an agent-ready desktop HTTP client written in Rust with GPUI. The
current MVP includes:

- Multiple request tabs with stable request IDs
- Editable methods, URLs, query parameters, headers, and JSON/text bodies
- Real HTTP execution with response status, timing, size, and formatted JSON
- In-memory session cookies shared across matching HTTP and WebSocket requests
- WebSocket streaming with per-message inspection, Stop, and Reconnect controls
- A typed, vendor-neutral agent command layer and working MCP stdio server
- A modern native dark UI built with GPUI and `gpui-component`
- Live, TOML-backed interface and syntax themes with VS Code/Zed import

Run it on macOS or Linux with:

```sh
cargo run -p alula
```

## Releases

Pushing a version tag such as `v0.1.1` runs the desktop release workflow. The
tag must match the workspace version in `Cargo.toml`. GitHub Releases receives
a native ARM64 macOS `.app` archive and a Windows x64 archive, together with a
SHA-256 file for each. The macOS bundle carries Alula's `.icns`; the Windows
executable embeds the multi-resolution `.ico` and application manifest.

The macOS build is ad-hoc signed so the bundle is internally consistent, but
it is not notarized with an Apple Developer identity. The workflow can also be
run manually for an existing tag from the Actions page.

The crate enables GPUI's `runtime_shaders` feature on macOS, so development
builds do not require the optional Xcode Metal Toolchain download.

See [PERFORMANCE.md](PERFORMANCE.md) for the reproducible benchmark suite,
flamegraph locations, measured gains, and further startup recommendations.

## WebSocket requests

Enter a `ws://` or `wss://` URL and select **Connect**. Alula also recognizes
an `http://` or `https://` endpoint with an enabled `Upgrade: websocket`
header and converts the scheme for the handshake. Query parameters,
environment variables, authentication headers, cookies, and WebSocket
subprotocol headers use the same request editor as HTTP requests.

Responses that set cookies automatically update the current application
session. Later HTTP requests and WebSocket handshakes receive matching cookies
according to their domain, path, secure, and expiry attributes. An explicit
`Cookie` request header takes precedence. The session jar is memory-only and is
cleared when Alula exits.

A non-empty request body is sent once as the initial text message. Incoming
text, binary, ping, pong, and close messages appear individually in the
response inspector with their direction, arrival time, and size. Binary data
that is not UTF-8 is displayed as a bounded hexadecimal preview. Long-lived
transcripts retain the newest 500 messages within a 16 MiB display budget.
Use **Stop** to close the live connection and **Reconnect** to start a fresh
session with the current request values.

## Settings and themes

Open **Settings** in the title bar. General settings can relocate the TOML
configuration, Agent settings configure the reserved loopback service port,
Keybindings records customizable application shortcuts with native keycap
previews, and Theme settings edit every interface and syntax color with the
native `gpui-component` color picker. Changes preview immediately; Cancel
restores the previous theme and Save atomically writes the configuration.
Exact `#RRGGBB` and `#RRGGBBAA` values are supported.

The file is created on first launch at:

```text
$ALULA_CONFIG                         when set
$XDG_CONFIG_HOME/alula/config.toml    on XDG systems
~/.config/alula/config.toml           otherwise
```

The default agent port is `37421`. The generated TOML exposes these values as
`application.config_path`, `agent.port`, and a `[keybindings]` table. Empty
shortcut values disable a command. When the configuration is moved, Alula
stores a small `location.toml` pointer beside the platform-default file;
`ALULA_CONFIG` remains the highest-priority override.

The import page accepts Alula TOML, VS Code JSON themes, and Zed JSON themes.
Imported interface colors and token scopes are converted into Alula's shared
UI/syntax model and previewed before saving.

## Agent integration

MCP is Alula's public agent protocol. The desktop app starts an embedded
Streamable HTTP server on `http://127.0.0.1:37421/mcp` by default. Its port is
configurable on the Agent settings page and in `[agent].port`; saving a new
port restarts the service. The green header badge displays the port only when
the server is actually listening, and displays the bind error otherwise.

Point an MCP client at the running app:

```json
{
  "mcpServers": {
    "alula": { "url": "http://127.0.0.1:37421/mcp" }
  }
}
```

The endpoint is loopback-only, validates browser origins, supports JSON MCP
responses over POST, and exposes `GET /health` for readiness checks. A GET on
`/mcp` returns `405 Method Not Allowed` because Alula does not need a standalone
server-to-client SSE stream.

`alula::agent` contains the transport-independent command layer. Tool calls on
the embedded endpoint are dispatched to the live GPUI application state, so an
agent-created or updated request appears immediately and `send_request` uses
the same streaming request path as the Send button. Theme tools include
`get_theme`, `get_theme_schema`,
`preview_theme`, `save_theme`, and `import_theme`. This lets an agent design a
complete accessible interface and syntax theme, preview it without writing,
then persist it only with the user's approval.

For MCP hosts that only support subprocess transports, the existing stdio
bridge remains available:

```sh
cargo build -p alula --bin alula-mcp
# executable: target/debug/alula-mcp
```

Both the UI and MCP paths use the same parser, color validation, import
adapters, atomic writer, and syntax-highlighting theme builder.

The production architecture is deliberately layered:

```text
GPUI client ─┐
             ├─ Alula workspace + HTTP engine
MCP adapter ─┘          │
                 approval policy + audit log
```

The standalone bridge operates on persistent state. The embedded transport is
preferred when the UI is running because it works directly with the open tabs.
Secret access should still require explicit user approval by default.

This avoids coupling the core to OpenAI, Anthropic, or any single agent SDK. A
vendor-specific in-app assistant can still use the same typed command layer.
