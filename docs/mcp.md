# MCP transport and ChatGPT

The server implements MCP `2026-07-28` and retains `2025-11-25` compatibility
on one Streamable HTTP endpoint:

```text
POST http://127.0.0.1:3001/mcp
```

MCP `2026-07-28` is stateless. Clients may call `server/discover` before any
other method to select a supported version, read the tools capability, and load
the server instructions. The old `GET /sse` and `POST /message` endpoints were
removed because HTTP+SSE was superseded by Streamable HTTP.

## Discovery check

```bash
curl -sS -X POST "http://127.0.0.1:3001/mcp" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H 'MCP-Protocol-Version: 2026-07-28' \
  -H 'Mcp-Method: server/discover' \
  -d '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"manual-check","version":"1.0.0"},"io.modelcontextprotocol/clientCapabilities":{}}}}'
```

Modern requests must send matching protocol versions in the HTTP header and
request `_meta`. A tool call also sends `Mcp-Name` with the tool name.

## Configuration

- `TANDOOR_BASE_URL`: Tandoor URL; default `http://localhost:8080`.
- `TANDOOR_USERNAME`: shared Tandoor account; default `admin`.
- `TANDOOR_PASSWORD`: password for that account; default `admin`.
- `TANDOOR_AUTH_TOKEN`: reusable Tandoor token supported by the server API.
- `BIND_ADDR`: listen address; default `127.0.0.1:3001`.
- `MCP_ALLOWED_ORIGINS`: comma-separated exact browser Origin values. With an
  empty value, requests that include `Origin` are rejected; non-browser MCP
  clients normally omit it.
- `RUST_LOG`: tracing filter; default `info`.

Keep the local default bind unless a reverse proxy or container network needs a
different address. A public deployment must terminate HTTPS before this
service.

## Connect ChatGPT

In ChatGPT, open Plugins, add a custom MCP server, and enter
`https://your-host.example/mcp`. Alternatively, use Secure MCP Tunnel for a
private server. Select **No authentication**.

The URL must be reachable from OpenAI through public HTTPS or the secure tunnel.
`127.0.0.1` is not remotely reachable.

This process uses one shared Tandoor account. Anyone who can access `/mcp` can
read and change that kitchen. Put it behind Secure MCP Tunnel or an appropriate
private network boundary; the endpoint itself does not authenticate callers.

## Tool metadata conventions

Every tool needs an action-oriented description and annotations:

- Reads set `readOnlyHint: true`.
- Additive writes set `readOnlyHint: false` and `destructiveHint: false`.
- Deletes and bulk clearing set `destructiveHint: true`.
- Repeatable setters also set `idempotentHint: true`.

Update the server instructions when a new tool introduces a required lookup or
ordering relationship. Keep the first 512 characters self-contained.

## Tandoor API shape notes

Recent Tandoor responses diverge from older id-only fields. The client types
accept both shapes:

- `created_by` may be an integer or a nested user object.
- Shopping-list entries use `created_at` / `completed_at`.
- Meal plans use `from_date` / `to_date` (create requests must send those).
- Nested recipe payloads on meal plans / cook logs are summaries, not full
  recipe detail documents.

See `tests/test_api_shapes.rs` for fixtures.

## Follow-up work

These existing behavior issues are intentionally outside the protocol upgrade:

- `create_recipe` builds a step with an empty ingredient list in
  `src/server.rs`.
- `add_to_shopping_list` accepts a unit string but sends `unit: None` instead
  of resolving a Tandoor unit identifier.
- Tool handlers serialize JSON into text content rather than defining
  `structuredContent` and output schemas.
- `ensure_authenticated` returns the Tandoor-client mutex guard, so each tool
  holds the mutex while its HTTP request is in flight.
