---
name: Tandoor 2.7 Token Auth
overview: Tandoor 2.7.0 removed the /api-token-auth/ endpoint. The MCP server's username/password fallback (client + server layers), the helper scripts, several tests, and the docs still depend on it. Make TANDOOR_AUTH_TOKEN the only supported credential, remove the password path end to end, fix the scripts and docs, and check the hassio deployment before its Tandoor add-on updates.
todos:
  - id: deployment-check
    content: "Completed via /Users/ralph/dev/homeassistant-containers: production runs custom Recipes commit 1dc8e278f (2.6.15-51) and staging runs 498c49655 (2.6.15-47). Both ignored MCP env files are complete and mode 0600; stale/missing tokens were replaced with valid five-year API tokens. Staging was deployed and verified first, then production. Both tunnels are running, startup validates the token against Tandoor, and an isolated container completed search_recipes through each MCP server"
    status: completed
  - id: auth-client
    content: "Remove the password path from the client layer: delete TandoorAuth::authenticate() in src/client/auth.rs, TandoorClient::authenticate() in src/client/client.rs, and AuthRequest/AuthToken in src/client/types.rs (AuthToken is deserialised in tests/test_types.rs:213 — drop that test). Rewrite the rate-limit doc comments in auth.rs, client.rs:38, and the doc examples in src/client/mod.rs:21 and src/lib.rs:30"
    status: completed
  - id: auth-server
    content: "Remove the password path from src/server.rs: GLOBAL_CREDENTIALS (2441), new_with_credentials (2607), TandoorMcpServer::authenticate (2683), and the credential fallback in ensure_authenticated (2738-2764) so it only consults GLOBAL_AUTH and otherwise errors with 'TANDOOR_AUTH_TOKEN is not set'. Update the module/struct docs that explain the 10/day throttle (19, 2437-2451, 2666-2670). Keep new(base_url) + set_global_auth_token as the only construction path"
    status: completed
  - id: main-startup
    content: "src/main.rs: require a non-blank TANDOOR_AUTH_TOKEN; if TANDOOR_USERNAME/TANDOOR_PASSWORD are set, log a one-line warning that they are ignored since Tandoor 2.7 (keep accepting them for one release). Replace the failure hints at 102-110 (mention token, base URL, reachability; drop username/password/rate-limit lines) and the env-var header comment at 10-12"
    status: completed
  - id: scripts
    content: "scripts/test.sh: mint the token inside the existing create_admin_user() Django-shell heredoc (docker compose exec -T web_recipes /opt/recipes/venv/bin/python manage.py shell) using the same oauth2_provider AccessToken code as .github/workflows/ci.yml, capture it, and export TANDOOR_AUTH_TOKEN in run_tests(); make a missing token fatal instead of a warning. Rewrite scripts/get_token.sh as a thin docker-compose-exec wrapper around the same snippet (for manual use) or delete it; drop the 10/day throttle wording"
    status: completed
  - id: tests
    content: "tests/common/mod.rs: remove the authenticate() fallback and fail fast with a clear message when TANDOOR_AUTH_TOKEN is unset. tests/test_authentication.rs: delete test_authentication_failure or replace it with 'invalid Bearer token → API call returns 401'. tests/test_mcp_tools.rs and tests/test_protocol.rs: switch new_with_credentials → new + set_global_auth_token. Keep CI token minting as is"
    status: completed
  - id: docs
    content: "README.md: quick start (30-37), verification curl (166-174), env table (185-186), throttle note (203), 'Getting a Tandoor API token' (213-220) → Settings → API instructions, and the 429 troubleshooting entry (350-352). docs/mcp.md Configuration (32-41): describe token as required, drop the fallback sentence. docs/telemetry.md:47: drop the api-token-auth mention. .env.example: remove TANDOOR_USERNAME/PASSWORD. docker-compose.yml:16-17 and Dockerfile:42: remove the username/password env passthrough/default. Note the version boundary: Tandoor ≤ 2.6.15 still has the endpoint, 2.7.0+ does not"
    status: completed
  - id: verify
    content: cargo fmt, cargo clippy --all-targets -D warnings, unit tests (cargo test --lib --test test_types --test test_api_shapes --test test_protocol), doc tests, and scripts/test.sh against vabene1111/recipes:latest locally
    status: completed
  - id: pull-request
    content: Open a PR from branch feature/tandoor-27-token-auth with gh pr create --repo ralphzimmermann/tandoor-mcp against main when requested
    status: pending
  - id: deployment-docs
    content: "In the separate /Users/ralph/dev/homeassistant-containers repository: update docs/deployment.md:203-205 to say TANDOOR_AUTH_TOKEN is required (not 'preferably'); remove the obsolete TANDOOR_USERNAME/TANDOOR_PASSWORD entries created by scripts/bootstrap-staging-credentials.sh:138-139; make scripts/deploy-tandoor-mcp.sh fail before building when TANDOOR_AUTH_TOKEN is absent or blank"
    status: completed
isProject: false
---

# Tandoor 2.7 removed `/api-token-auth/`

## Background

Tandoor 2.7.0 (2026-10-23) migrated its auth views to the new frontend and
removed the `api-token-auth/` endpoint. Verified against upstream
`cookbook/urls.py`: tag 2.6.15 has `path('api-token-auth/', CustomAuthToken.as_view())`,
tag 2.7.0 does not. Latest release at time of review is 2.7.2 (2026-10-27).

CI broke the same day because it pulls `vabene1111/recipes:latest`; the fix
(mint the token from the Django shell) landed as part of PR #6, now merged to
`main` as `8e36186`. Everything else that uses the endpoint is still in place
and is the scope of this plan.

## Implementation status (2026-10-29)

Implemented on `feature/tandoor-27-token-auth`: the runtime now requires a
non-blank `TANDOOR_AUTH_TOKEN`; client/server password authentication and its
public API have been removed; local tests mint a short-lived token through the
Tandoor container's Django shell; tests, deployment configuration, README, and
`docs/` have been updated. The staging and production deployments are complete:
both use database-validated API tokens, both tunnel sidecars are running, and
`search_recipes` succeeds through each private MCP endpoint. The separate
deployment repository now restores ignored MCP env files without exposing
secrets, rejects incomplete credentials before building, and documents the
token-only requirement. Formatting and `cargo check --all-targets` pass; the remaining
verification also passed: Clippy with warnings denied, unit/protocol/doc tests,
shell syntax checks, and the full Docker integration suite against
`vabene1111/recipes:latest` (including token minting and cleanup).

## What still depends on the endpoint (reviewed 2026-10-29)

### Code

| Location | Use | Effect on Tandoor ≥ 2.7 |
| --- | --- | --- |
| `src/client/auth.rs:44-113` | `TandoorAuth::authenticate()` POSTs username/password | 404 → "Tandoor API endpoint not found. Check your base URL" (misleading) |
| `src/client/client.rs:81-83` | `TandoorClient::authenticate()` passthrough; docs at 38, 49 | dead path |
| `src/client/types.rs:89-100` | `AuthRequest` / `AuthToken` | only used by the removed endpoint |
| `src/server.rs:2441` | `GLOBAL_CREDENTIALS` static | holds credentials that can never be used |
| `src/server.rs:2607-2609` | `new_with_credentials()` stores them | public API used by `main.rs`, `tests/test_protocol.rs`, `tests/test_mcp_tools.rs`, `lib.rs` doc example |
| `src/server.rs:2683-2698` | `TandoorMcpServer::authenticate()` | dead path |
| `src/server.rs:2738-2764` | `ensure_authenticated()` falls back to credentials when `GLOBAL_AUTH` is empty | hits the 404 at first tool call if the token was never set |
| `src/main.rs:90-111` | falls back to `authenticate()` when token blank; failure hints talk about username, password, rate limits | startup fails with misleading text |
| `src/client/mod.rs:21`, `src/lib.rs:30` | doc examples use `authenticate()` / `new_with_credentials()` | stale |

### Scripts and tests

| Location | Use | Effect |
| --- | --- | --- |
| `scripts/get_token.sh` | curls the endpoint | always fails |
| `scripts/test.sh:217-226` | calls `get_token.sh`, warns and continues | integration tests fall back to password login and fail |
| `tests/common/mod.rs:41-64` | password fallback when `TANDOOR_AUTH_TOKEN` unset | never works; error text talks about rate limits |
| `tests/test_authentication.rs:31-52` | `test_authentication_failure` expects bad credentials to fail | passes for the wrong reason (404) |
| `tests/test_mcp_tools.rs:16-30` | `new_with_credentials` + `server.authenticate` fallback | breaks when the API is removed |
| `tests/test_protocol.rs:20` | `new_with_credentials` | breaks when the API is removed (runs in the unit-test CI job) |
| `tests/test_types.rs:213` | deserialises `AuthToken` | breaks when the type is removed |

### Docs and deployment files

| Location | Content |
| --- | --- |
| `README.md:30-37` | quick start offers username/password alternative |
| `README.md:166-174` | verification curl against the endpoint |
| `README.md:185-186, 203` | env table rows and 10/day throttle note |
| `README.md:213-220` | "Getting a Tandoor API token" curls the endpoint |
| `README.md:350-352` | "Request was throttled (429)" troubleshooting |
| `docs/mcp.md:32-41` | describes the fallback; already mentions the 2.7 removal but needs a rewrite once the fallback is gone |
| `docs/telemetry.md:47` | lists the `/api-token-auth/` login as an uninstrumented span |
| `.env.example:4-5` | `TANDOOR_USERNAME` / `TANDOOR_PASSWORD` |
| `docker-compose.yml:16-17` | passes `TANDOOR_USERNAME` / `TANDOOR_PASSWORD` into the container |
| `Dockerfile:42` | `ENV TANDOOR_USERNAME=admin` |

`TANDOOR_AUTH_TOKEN` itself keeps working: tokens are `oauth2_provider.AccessToken`
rows, `OAuth2Authentication` is still the first DRF auth class, and 2.7.0 still
registers `access-token` (`AccessTokenViewSet`) so users can create tokens in
Tandoor under Settings → API.

## Approach

Make the token the only credential. Password login was already discouraged
(10 requests/day throttle) and no longer exists upstream, so carrying a
fallback that only works on ≤ 2.6 adds a confusing failure mode. Remove the
path in both layers (`client` and `server`) rather than only in `main.rs`;
otherwise `ensure_authenticated()` still reaches the 404 on the first tool
call if the token was never set.

Compatibility: keep reading `TANDOOR_USERNAME` / `TANDOOR_PASSWORD` in
`main.rs` for one release only to emit a startup warning that they are
ignored, so existing Compose files do not break silently. Remove the
variables from `.env.example`, `docker-compose.yml`, and `Dockerfile` now.

Public API change: `TandoorMcpServer::new_with_credentials` goes away; the
construction path becomes `TandoorMcpServer::new(base_url)` followed by
`set_global_auth_token(token)`. Update the two tests and the `lib.rs` doc
example accordingly.

Alternative, only if 2.6 support turns out to matter: keep `authenticate()`
but detect the 404 and print the token instruction instead of the rate-limit
hint. Not planned.

## Reference: minting a token without the endpoint

The CI step (`.github/workflows/ci.yml`, "Create test user and get auth token")
does exactly what Tandoor's removed view did:

```python
import uuid
from django.utils import timezone
from oauth2_provider.models import AccessToken
token = AccessToken.objects.create(
    user=u,
    token='tda_' + str(uuid.uuid4()).replace('-', '_'),
    expires=timezone.now() + timezone.timedelta(days=1),
    scope='read write app',
)
print('CI_TOKEN=' + token.token)
```

`scripts/test.sh` already runs a Django-shell heredoc in `create_admin_user()`
via `docker compose -f "$COMPOSE_FILE" exec -T web_recipes /opt/recipes/venv/bin/python manage.py shell`.
Append the snippet there, print a marker line, and capture it the same way CI
does (`sed -n 's/^CI_TOKEN=//p'`). That removes the need for a separate curl
step; `get_token.sh` can either wrap the same `docker compose exec` call for
manual use or be deleted.

## Out of scope

- Tandoor's new frontend auth flows (allauth headless); the MCP server is a
  server-to-server client and only needs the Bearer token.
- Token rotation/expiry handling. Tokens created in Settings → API have a
  user-chosen expiry; the server treats a 401 as a configuration error.
