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
  Blank values are treated as unset so Compose can fall back to
  username/password authentication.
- `BIND_ADDR`: listen address; default `127.0.0.1:3001`.
- `MCP_ALLOWED_HOSTS`: comma-separated hosts accepted by the transport's
  DNS-rebinding protection; defaults to loopback hosts. The Compose deployment
  also allows its `mcp-tandoor` service name. An empty value retains rmcp's
  secure loopback defaults rather than disabling host validation.
- `MCP_ALLOWED_ORIGINS`: comma-separated exact browser Origin values. With an
  empty value, requests that include `Origin` are rejected; non-browser MCP
  clients normally omit it. Allowlisted browser origins receive CORS preflight
  and response headers for the MCP transport methods and headers.
- `S3_BUCKET`: private object-storage bucket for generated recipe thumbnails.
  Unset disables inline galleries. `S3_REGION`, `S3_ENDPOINT`,
  `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `S3_FORCE_PATH_STYLE`,
  `S3_THUMBNAIL_PREFIX`, and `S3_PUBLIC_BASE_URL` configure AWS S3 or a
  compatible service. See [S3 thumbnails and inline gallery](thumbnails.md).
- `THUMBNAIL_URL_TTL_SECS`: presigned thumbnail lifetime; default 86400.
- `THUMBNAIL_MAX_EDGE`: generated JPEG longest edge; default 512.
- `RUST_LOG`: tracing filter; default `info`.

Keep the local default bind unless a reverse proxy or container network needs a
different address. A public deployment must terminate HTTPS before this
service.

## Connect ChatGPT

For a public deployment, open ChatGPT Plugins, add a custom MCP server, and
enter `https://your-host.example/mcp`.

For a private deployment, use the `chatgpt` Docker Compose profile. It runs
OpenAI's official `tunnel-client` image as a sidecar on the same Docker network
as this server:

```bash
docker compose --profile chatgpt up -d tunnel-client
docker compose --profile chatgpt logs -f mcp-tandoor tunnel-client
```

Compose reads `.env` automatically. The profile requires:

- `TANDOOR_BASE_URL` and either `TANDOOR_AUTH_TOKEN` or the Tandoor username
  and password;
- `CONTROL_PLANE_TUNNEL_ID`; and
- `OPENAI_API_KEY`, mapped to the tunnel client's required
  `CONTROL_PLANE_API_KEY`. Use a runtime API key whose principal has Tunnels
  **Read** and **Use** permissions, not an admin key.

The value of `MCP_SERVER_URL` is deliberately set by Compose to
`http://mcp-tandoor:3001/mcp`. Do not use `127.0.0.1` between these containers:
it would address the tunnel sidecar itself. No host port or inbound firewall
rule is needed; the tunnel makes an outbound HTTPS connection to OpenAI.

In ChatGPT, choose **Tunnel** as the custom MCP server connection, select the
configured tunnel or paste `CONTROL_PLANE_TUNNEL_ID`, and select **No
authentication**. Keep the tunnel associated with the target ChatGPT workspace.
The image is pinned to a reviewed release in `docker-compose.yml`; update that
tag deliberately from the
[latest OpenAI release](https://github.com/openai/tunnel-client/releases/latest).

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

The server currently exposes 57 tools. In addition to the original recipe,
shopping, pantry, meal-plan, metadata, and cooking-log operations, it supports:

- targeted recipe and ingredient edits, import, deletion, and photos;
- recipe-book membership;
- named shopping lists and recipe/meal-plan shopping groups;
- supermarkets, aisle ordering, and food categories;
- duplicate-food discovery/merge and unit conversions; and
- partial meal-plan updates.

Protocol tests compare the complete set of names rather than only a count.
Meal-plan shopping creates a `shopping-list-recipe` group for each plan and
uses Tandoor's bulk-entry action, preserving the meal-plan link for later
rescaling and grouped removal.

## Tandoor API shape notes

Recent Tandoor responses diverge from older id-only fields. The client types
accept both shapes:

- `created_by` may be an integer or a nested user object.
- Shopping-list entries use `created_at` / `completed_at`.
- Meal plans use `from_date` / `to_date`; write requests resolve recipe and
  meal-type IDs and send nested objects containing both fields, such as
  `{"id": 1284, "name": "Buckwheat Porridge GF"}`, as required by Tandoor's
  writable nested serializers.
- Nested recipe payloads on meal plans / cook logs are summaries, not full
  recipe detail documents.
- Shopping entries embed a slim food object and may omit creator/timestamp
  fields.
- Recipe ingredients may have no food when they represent a section header.
- Supermarket categories may be nested objects rather than integer ids.

See `tests/test_api_shapes.rs` for fixtures.

Meal-plan create/update failures include Tandoor's response body in the tool
error so validation failures identify the rejected field.

## Composite operations

Tandoor does not provide a transaction covering composite MCP tools. Targeted
recipe edits, food merges, recipe-to-shopping operations, and aisle-order
updates validate their inputs before the first write, but a later HTTP failure
can still leave earlier writes applied. Tool responses include item-level
errors or failure context so callers can inspect the resulting resource before
retrying.

Collection-wide operations stop after 100 pages. Meal-plan shopping ranges are
limited to 366 days. Destructive name-based operations require an exact name
or numeric id and do not use partial-name fallback.

Pagination URLs returned by Tandoor are rebased onto `TANDOOR_BASE_URL` before
the authorization token is attached. This supports reverse proxies while
preventing an API response from forwarding the token to another host. Typed
recipe-book, book-entry, and unit-conversion lists follow every page; malformed
collection responses fail instead of being interpreted as empty.

URL imports first parse Tandoor's source preview, then create the recipe and
attempt its image upload. If the image fails, the recipe remains created and
the tool reports `image_error` so retrying does not create a duplicate.

## Recipe image security

`set_recipe_image` accepts only public HTTPS URLs. The downloader rejects URL
credentials and private, loopback, link-local, documentation, and reserved
addresses; pins each request to a validated DNS result; disables automatic
redirects and revalidates each redirect; applies a 20-second timeout; verifies
both media type and file signature; and streams through a 20 MB limit.

`get_recipe_details` fetches the stored photo directly from the configured
Tandoor origin with API authentication and returns it as an MCP image content
block. Absolute media URLs from Tandoor are rebased onto the configured origin
when they identify that origin. Up to three same-origin media redirects are
followed with authentication. External HTTPS image URLs and redirect targets
(for example object storage or a CDN) use the SSRF-safe public downloader
without the Tandoor token. Amazon S3 presigned query strings are preserved, and
S3 objects with missing or generic `application/octet-stream` metadata are
accepted only when their bytes have a supported image signature. Display images
are signature checked and limited to 10 MB. If no image can be attached, the
text result tells the model not to substitute an unrelated image.

When S3 thumbnails are configured, `get_recipe_details` attaches a bounded
thumbnail by default; `image: original` explicitly requests the existing
full-size attachment and `image: none` omits it. `get_recipe_gallery` generates
or reuses thumbnails, returns short-lived private URLs in widget-only `_meta`,
and binds the result to `ui://tandoor/recipe-gallery-v7.html`. The server
advertises the MCP resources capability and serves that URI as
`text/html;profile=mcp-app`; ChatGPT renders the component inline.
`get_meal_plan_gallery` similarly binds a complete date-range result to
`ui://tandoor/meal-plan-gallery-v10.html`, keeping meal metadata, recipe summaries,
tags, servings, and photos in one authoritative card gallery instead of relying
on the client to reconstruct the layout from separate tool calls.
`get_recipe_image` returns one thumbnail or original image on demand.

Gallery results intentionally contain no inline image blocks. An earlier
version attached up to 12 base64 JPEG thumbnails (about 500 KB per result) as a
non-widget fallback; ChatGPT then never delivered the result to the component
iframe and the widget hung on its loading text. Keep gallery results small and
point clients without the component at `get_recipe_image`.

Widget resource URIs are immutable cache keys (`RECIPE_GALLERY_URI`,
`MEAL_PLAN_GALLERY_URI` in `server.rs`). Increment the URI when changing widget
HTML, CSS, JavaScript, or its MCP Apps protocol handshake, then update the
protocol tests. `resources/read` additionally serves the current HTML for any
`ui://tandoor/recipe-gallery*.html` or `ui://tandoor/meal-plan-gallery*.html`
URI, so a client still holding a stale cached tool descriptor loads a working
widget instead of an "unknown resource" error.

Widgets consume the standard `ui/notifications/tool-result` message and also
subscribe to ChatGPT's `openai:set_globals` compatibility event. The latter is
required because `window.openai.toolOutput` may be populated after the widget
script's initial execution. Initialization is scheduled after the initial
document execution and retried briefly because the host bridge may attach
after an inline widget script starts. As a last resort, widgets record the
tool arguments (`ui/notifications/tool-input` or `window.openai.toolInput`) and,
if no result has arrived a few seconds after initialization, call the gallery
tool themselves through the host (`window.openai.callTool` or an MCP Apps
`tools/call` request), with one retry and a 20 s timeout. The loading copy
names the current phase ("Connecting to ChatGPT…", "Waiting for your meal
plan…", "Loading your meal plan…"), so a stuck widget tells you where the
hand-off failed; a failed self-fetch ends in an explicit error instead of an
indefinite loading state.

The meal-plan component groups cards by day (locale-formatted heading via
`Intl.DateTimeFormat` and `window.openai.locale`), hides a plan title that
merely repeats the recipe name, renders keywords as hue-hashed tinted pills,
humanizes minutes into hours, and clamps long plan notes behind a "Show more"
toggle that reports the new height to the host.

The widget resource CSP permits only the S3 origin derived from
`S3_PUBLIC_BASE_URL`, `S3_ENDPOINT`, or the AWS bucket/region. For compatible
storage, the internal operation endpoint and browser-visible signing endpoint
may differ, but both must address the same service. Presigned URLs must not be
rewritten after signing.

## Recipe links

Recipe-bearing results include a canonical `recipe_url` built from
`TANDOOR_BASE_URL`, for example
`https://recipes.example/recipe/1281`. Server and tool instructions tell clients
to render the recipe name as a Markdown link using that field. Configure
`TANDOOR_BASE_URL` with a URL that is both reachable by the MCP server and
openable in the user's browser.

## Remaining protocol enhancement

Tool handlers currently serialize JSON into text content. Typed
`structuredContent` and per-tool output schemas remain a separate MCP response
format enhancement.
