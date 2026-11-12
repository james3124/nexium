# nexiumx

NEXIUM is an independent, Rust-powered headless browser engine. This package
ships the `nexium` CLI and MCP server launcher for Node.js users.

## Install

```bash
npm install --global nexiumx
# or run without installing
npx nexiumx https://example.com
```

The installer downloads a prebuilt binary for your platform from GitHub
Releases into `vendor/<platform>-<arch>/`. If your platform has no prebuilt
binary, set `NEXIUM_BINARY_PATH` to a locally built `nexium` executable.

Environment variables:

- `NEXIUM_BINARY_PATH` — use a specific binary.
- `NEXIUM_SKIP_DOWNLOAD=1` — skip the postinstall download.

## CLI

```bash
nexium https://example.com                     # summary
nexium https://example.com --dump-dom          # DOM tree
nexium https://example.com --json              # JSON summary
nexium https://example.com --screenshot page.png
nexium --api --bind 127.0.0.1:9222             # REST/WebSocket/MCP server
nexium --api --bind 127.0.0.1:9222 \
  --tls-cert cert.pem --tls-key key.pem        # HTTPS
nexium --mcp                                   # MCP over stdio
```

## MCP server

Stdio transport (recommended for local clients):

```json
{
  "mcpServers": {
    "nexium": {
      "command": "npx",
      "args": ["-y", "nexiumx", "--mcp"]
    }
  }
}
```

HTTP transports: `POST /mcp`, Streamable HTTP (`GET /mcp` SSE stream,
`DELETE /mcp`) and legacy `GET /mcp/sse` + `POST /mcp/message`. The server
exposes 19 `browser_*` tools.

## License

MIT
