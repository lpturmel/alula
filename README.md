# Alula

Alula is an agent-ready desktop HTTP client written in Rust with GPUI. The
current MVP includes:

- Multiple request tabs with stable request IDs
- Editable methods, URLs, query parameters, headers, and JSON/text bodies
- Real HTTP execution with response status, timing, size, and formatted JSON
- A typed, vendor-neutral agent command layer and working MCP stdio server
- A modern native dark UI built with GPUI and `gpui-component`
- Live, TOML-backed interface and syntax themes with VS Code/Zed import

Run it on macOS or Linux with:

```sh
cargo run -p alula
```

The crate enables GPUI's `runtime_shaders` feature on macOS, so development
builds do not require the optional Xcode Metal Toolchain download.

## Settings and themes

Open **Settings** in the title bar. General settings can relocate the TOML
configuration, Agent settings configure the reserved loopback service port,
and Theme settings edit every interface and syntax color with the native
`gpui-component` color picker. Changes preview immediately; Cancel restores the
previous theme and Save atomically writes the configuration. Exact `#RRGGBB`
and `#RRGGBBAA` values are supported.

The file is created on first launch at:

```text
$ALULA_CONFIG                         when set
$XDG_CONFIG_HOME/alula/config.toml    on XDG systems
~/.config/alula/config.toml           otherwise
```

The default agent port is `37421`. The generated TOML exposes these values as
`application.config_path` and `agent.port`. When the configuration is moved,
Alula stores a small `location.toml` pointer beside the platform-default file;
`ALULA_CONFIG` remains the highest-priority override.

The import page accepts Alula TOML, VS Code JSON themes, and Zed JSON themes.
Imported interface colors and token scopes are converted into Alula's shared
UI/syntax model and previewed before saving.

## Agent integration

MCP is Alula's public agent protocol. `alula::agent` contains the
transport-independent command layer, while `alula-mcp` provides a working stdio
JSON-RPC transport. Theme tools include `get_theme`, `get_theme_schema`,
`preview_theme`, `save_theme`, and `import_theme`. This lets an agent design a
complete accessible interface and syntax theme, preview it without writing,
then persist it only with the user's approval.

Build and register the server executable with your MCP host:

```sh
cargo build -p alula --bin alula-mcp
# executable: target/debug/alula-mcp
```

The desktop app watches the TOML file and applies a theme saved by MCP while it
is running. Both the UI and MCP paths use the same parser, color validation,
import adapters, atomic writer, and syntax-highlighting theme builder.

The production architecture is deliberately layered:

```text
GPUI client ─┐
             ├─ Alula workspace + HTTP engine
MCP adapter ─┘          │
                 approval policy + audit log
```

The standalone stdio bridge is the first transport. A future loopback
Streamable HTTP endpoint can be embedded in the desktop app with a per-install
token. `send_request` and secret access must still require explicit user
approval by default.

This avoids coupling the core to OpenAI, Anthropic, or any single agent SDK. A
vendor-specific in-app assistant can still use the same typed command layer.
