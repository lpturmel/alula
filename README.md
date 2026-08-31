# Alula

Alula is an agent-ready native desktop client for building, sending, and
inspecting HTTP and WebSocket requests. It is written in Rust with GPUI.

## Features

- Build and run HTTP requests in tabs with flexible editing and response inspection
- Connect to WebSocket endpoints and inspect live messages
- Organize persistent requests and environments, and revisit request history
- Reuse variables and session cookies while keeping secrets in the OS credential
  store
- Control requests, environments, history, and themes through MCP integrations
- Customize shortcuts and live themes, including VS Code and Zed theme imports

## Run locally

```sh
cargo run -p alula
```

See [PERFORMANCE.md](PERFORMANCE.md) for benchmarks and profiling notes.
