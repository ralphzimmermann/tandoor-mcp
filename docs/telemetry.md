# New Relic tracing

The server exports OpenTelemetry traces directly to New Relic over
OTLP/HTTP-protobuf. Set a non-empty `NEW_RELIC_LICENSE_KEY` to enable it:

```env
NEW_RELIC_LICENSE_KEY=replace-with-a-New-Relic-license-key
NEW_RELIC_APP_NAME=tandoor-mcp-production
NEW_RELIC_OTLP_ENDPOINT=https://otlp.nr-data.net:4318
```

Use `https://otlp.eu01.nr-data.net:4318` for an EU New Relic account. The
license key is sent in the required `api-key` header and must stay in an
untracked environment file.

When the key is blank or absent, the process keeps normal `tracing` output in
Docker logs but creates no exporter, background task, or telemetry traffic.

## Span model

Each MCP tool invocation creates one `mcp.tool_call` server span with the tool
name and outcome. Argument values are never attached because they can contain
recipe text or signed URLs. Requests made through the shared Tandoor HTTP
client create child spans with HTTP method, destination, status, and duration.

The HTTP client injects the W3C `traceparent` header. When the Recipes service
also runs the New Relic Python agent, New Relic can display this path as one
distributed trace:

```text
MCP tool call -> Tandoor API request -> Django transaction -> PostgreSQL query
```

The first iteration intentionally leaves S3 SDK operations and public image
downloads uninstrumented. Add those only if traces identify them as a
meaningful source of latency.
