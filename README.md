# Tandoor Recipes MCP Server

A Model Context Protocol (MCP) server for [Tandoor Recipes](https://tandoor.dev) that provides comprehensive recipe management, shopping lists, meal planning, and inventory tracking capabilities.

## Features

- **Recipe Management**: Search, create, and manage recipes
- **Shopping Lists**: Add items, mark as purchased, and manage shopping workflows
- **Meal Planning**: Plan meals and manage meal schedules
- **Inventory Tracking**: Monitor pantry items and get recipe suggestions
- **Keywords & Tags**: Organize recipes with keywords and categories
- **Cooking Logs**: Track cooking history and ratings

## Transport

This server uses the **Streamable HTTP** MCP transport. The MCP endpoint is `/mcp` (not `/sse`). Streamable HTTP is required for Claude custom connectors — legacy SSE triggers an OAuth discovery probe that Claude cannot satisfy.

## Quick Start

1. **Clone the repository**:

```bash
git clone https://github.com/ralphzimmermann/tandoor-mcp
cd tandoor-mcp
```

2. **Configure environment variables**:

```bash
# Create an API token in Tandoor under Settings → API.
export TANDOOR_BASE_URL="http://localhost:8080"
export TANDOOR_AUTH_TOKEN="tda_your_token_here"
```

3. **Run the server**:

```bash
cargo run
# Listens at 127.0.0.1:3001 by default; MCP endpoint at /mcp
```

The MCP Streamable HTTP endpoint is `http://127.0.0.1:3001/mcp`. See
[MCP transport and ChatGPT](docs/mcp.md) for discovery checks, deployment
security, and ChatGPT setup.

## Docker

```bash
docker run -d \
  -e TANDOOR_BASE_URL="http://your-tandoor:8080" \
  -e TANDOOR_AUTH_TOKEN="tda_your_token_here" \
  -e BIND_ADDR="0.0.0.0:3001" \
  -p 3001:3001 \
  ghcr.io/ralphzimmermann/tandoor-mcp:latest
```

To connect the private server to ChatGPT through OpenAI Secure MCP Tunnel, set
the Tandoor and tunnel credentials in `.env`, then run:

```bash
docker compose --profile chatgpt up -d tunnel-client
```

This starts the MCP server and the pinned official tunnel sidecar without
publishing the MCP port. See [MCP transport and ChatGPT](docs/mcp.md) for the
required variables, permissions, logs, and ChatGPT setup.

The Compose build pulls the Rust build image from the Public ECR mirror of the
official image because deployment hosts have hit Docker Hub failures; set
`RUST_IMAGE` in `.env` to use another registry. Plain `docker build` and CI use
the Dockerfile's Docker Hub default, since Public ECR throttles anonymous pulls
from shared runner IPs.

## Adding to Claude as a Custom Connector

1. In Claude settings, go to **Integrations → Custom connectors → Add custom connector**
2. Set the URL to your server's `/mcp` endpoint:
   ```
   https://tandoor-mcp.example.com/mcp
   ```
3. No OAuth Client ID is needed — Streamable HTTP doesn't trigger OAuth probes.

The MCP endpoint does not authenticate callers. Keep it on a private network or
put it behind an authenticated HTTPS proxy; a query parameter alone is not an
adequate access-control boundary.

## Tandoor Configuration

⚠️ **IMPORTANT**: Tandoor uses a multi-tenant permission system that requires specific setup for API access to work properly. Without proper space and group configuration, you'll get permission errors even with valid authentication.

### Method 1: Web Interface Setup (Recommended)

1. **Access Tandoor admin interface**:

   - Go to `http://your-tandoor-url/admin/`
   - Login with superuser credentials

2. **Create/Verify Groups**:

   - Navigate to **Authentication and Authorization** → **Groups**
   - Ensure these groups exist: `admin`, `user`, `guest`
   - If missing, create them (names must match exactly)

3. **Create a Space**:

   - Navigate to **Cookbook** → **Spaces**
   - Click **Add Space**
   - Fill in:
     - **Name**: Your organization/space name (e.g., "Production", "Family")
     - **Max recipes**: 0 (unlimited)
     - **Max users**: 0 (unlimited)
     - **Max file storage mb**: 0 (unlimited)
     - **Allow sharing**: Checked
   - Click **Save**

4. **Create User Space Association**:
   - Navigate to **Cookbook** → **User spaces**
   - Click **Add User space**
   - Select:
     - **User**: Your admin user
     - **Space**: The space you just created
     - **Active**: Must be checked
   - Click **Save**
   - After saving, click on the created User space entry
   - In **Groups**, select `admin` and add it
   - Click **Save**

### Method 2: Command Line Setup

```bash
docker exec -it your-tandoor-container /opt/recipes/venv/bin/python manage.py shell
```

```python
from cookbook.models import Space, UserSpace
from django.contrib.auth.models import User, Group

admin_user = User.objects.get(username='admin')

space, created = Space.objects.get_or_create(
    name='Production',
    defaults={
        'created_by': admin_user,
        'max_recipes': 0,
        'max_users': 0,
        'max_file_storage_mb': 0,
        'allow_sharing': True
    }
)

user_space, created = UserSpace.objects.get_or_create(
    user=admin_user,
    space=space,
    defaults={'active': True}
)

if not created and not user_space.active:
    user_space.active = True
    user_space.save()

admin_group = Group.objects.get(name='admin')
user_space.groups.add(admin_group)
```

### Verification

```bash
# Test the token created under Tandoor Settings → API
curl -X GET http://your-tandoor-url/api/keyword/ \
  -H "Authorization: Bearer <YOUR_TOKEN>"
```

## Configuration

### Environment Variables

| Variable             | Description                                               | Default                  |
| -------------------- | --------------------------------------------------------- | ------------------------ |
| `TANDOOR_BASE_URL`   | Full browser-accessible URL to your Tandoor instance      | `http://localhost:8080`  |
| `TANDOOR_AUTH_TOKEN` | Tandoor API token (**required**)                          | —                        |
| `BIND_ADDR`          | Address and port for the MCP server                       | `127.0.0.1:3001`         |
| `MCP_ALLOWED_HOSTS`   | Hosts accepted by DNS-rebinding protection                | Loopback hosts           |
| `MCP_ALLOWED_ORIGINS` | Exact browser origins allowed to call `/mcp`              | —                        |
| `S3_BUCKET`           | Private bucket for generated recipe thumbnails             | Disabled                 |
| `S3_REGION`           | S3 bucket region                                            | `us-east-1`              |
| `S3_ENDPOINT`         | Optional S3-compatible internal endpoint                   | AWS                      |
| `S3_ACCESS_KEY_ID` / `S3_SECRET_ACCESS_KEY` | Thumbnail-only credentials                 | —                        |
| `S3_PUBLIC_BASE_URL`  | Optional browser-visible endpoint used for signed URLs/CSP | Derived                  |
| `THUMBNAIL_URL_TTL_SECS` | Presigned thumbnail URL lifetime                       | `86400`                  |
| `THUMBNAIL_MAX_EDGE`  | Generated JPEG longest edge                                | `512`                    |
| `NEW_RELIC_LICENSE_KEY` | Enables New Relic OTLP tracing when non-empty            | Disabled                 |
| `NEW_RELIC_APP_NAME`  | New Relic/OpenTelemetry service name                        | `tandoor-mcp`             |
| `NEW_RELIC_OTLP_ENDPOINT` | New Relic OTLP/HTTP base endpoint                      | US endpoint              |
| `NEW_RELIC_ENVIRONMENT_NAME` | Optional `deployment.environment.name` trace attribute | Unset                 |
| `RUST_LOG`           | Log level (`info`, `debug`, `trace`, …)                   | `info`                   |

`TANDOOR_AUTH_TOKEN` is required. Create it in Tandoor under **Settings → API**.
Tandoor 2.7.0 removed the username/password `/api-token-auth/` endpoint (it is
still present in Tandoor 2.6.15 and earlier), so this server no longer accepts
username/password credentials.

See [New Relic tracing](docs/telemetry.md) for the span model and production
configuration.

The Compose deployment adds `mcp-tandoor` to `MCP_ALLOWED_HOSTS` so the tunnel
sidecar can use the private service name without disabling host validation.
See [S3 thumbnails and inline recipe gallery](docs/thumbnails.md) for AWS IAM,
lifecycle, security, and local MinIO setup.

### Getting a Tandoor API token

Open your Tandoor instance, go to **Settings → API**, create an access token,
and set its `tda_...` value as `TANDOOR_AUTH_TOKEN`. Treat the token as a
password and choose an expiry appropriate for the deployment.

## Available Tools

### Recipe Management

- `search_recipes` — Search for recipes with flexible querying (keywords, foods, cooking time, rating, random). Each result includes a clickable `recipe_url` and shows `has_photo`; `has_photo: false` lists recipes missing a photo
- `get_recipe_details` — Get comprehensive recipe information, including `servings_text`, `source_url`, nutrition, recipe-level properties, computed food-property totals, metadata, step display flags, and attached-file metadata. Includes an authoritative thumbnail by default; use `image: "original"` only for full resolution or `image: "none"` to omit it. Ingredient amounts, nutrition, and food-property totals scale with `servings` (`base_servings` keeps the stored value); per-serving properties do not
- `get_recipe_gallery` — Render 1–24 authoritative recipe thumbnails in an inline MCP Apps gallery (requires the S3 thumbnail cache)
- `get_recipe_image` — Return one authoritative thumbnail or explicitly requested original MCP image
- `create_recipe` — Create a new, internally managed recipe, including `servings_text`, `source_url`, ingredient-overview visibility, nutrition, and recipe-level properties. Use `steps` to add structured steps, each with its own ingredients (food, unit, amount, note; omit `amount` for to-taste ingredients; `{"header": "For the sauce"}` adds a section heading) — the legacy `instructions` field creates a single step with no ingredients
- `update_recipe` — Update an existing recipe and mark it as internally managed; only requested fields change. Supports partial nutrition updates/removal and property upserts/removals in addition to top-level fields, targeted keyword changes, and targeted or full step changes. Returns the resulting recipe and steps
- `get_property_types` — List valid recipe property type IDs, names, units, and descriptions; call this before setting recipe properties
- `set_recipe_image` — Set a recipe's photo from a public HTTPS JPEG, PNG, WebP, or GIF URL; private/reserved hosts are rejected and downloads are streamed through a 20 MB limit
- `set_recipe_image_from_file` — Set a recipe's photo from an image attached to the ChatGPT conversation (user upload or generated image); ChatGPT passes a signed download URL via `openai/fileParams`. Web/desktop ChatGPT only; the mobile apps do not hand over attachments yet
- `remove_recipe_image` — Remove a recipe's photo
- `delete_recipe` — Delete a recipe permanently
- `import_recipe_from_url` — Parse an external URL, persist the resulting recipe, and import its image when available

### Recipe Relations and Ingredient Structure

- `find_unlinked_recipe_ingredients` — Find ingredients whose food name exactly matches an unlinked recipe; duplicate recipe names are returned as ambiguous
- `link_step_recipe` / `unlink_step_recipe` — Embed or remove a related recipe on one specific parent step
- `link_food_recipe` / `unlink_food_recipe` — Set or clear a global recipe link on a Food record; this affects every ingredient using that Food
- `add_ingredient_header` — Insert a section heading such as “For the sauce” before a stable ingredient ID, or append it
- `set_related_recipe_mealplan_inclusion` — Control whether the authenticated account includes related recipes in meal-plan shopping lists

Call `find_unlinked_recipe_ingredients` before writing and let the user choose
the scope: a step embed is local to one parent recipe, while a Food link is
global. `get_recipe_details` returns the stable step, ingredient, and food IDs
required for these tools. See [Recipe relations through MCP](docs/mcp.md).

### Food Management and Sourcing

- `search_foods` — Search reusable Foods and return their descriptions, pantry/category state, and raw Markdown `sourcing_info`
- `get_food` — Retrieve one Food by ID, including both `sourcing_info` and the sanitized `sourcing_info_markdown`
- `create_food` — Create a Food with optional plural name, description, pantry state, and Markdown sourcing information
- `update_food` — Partially update those fields; pass `sourcing_info: ""` to clear the sourcing notes
- `get_shopping_list` items carry `sourcing_info` (Markdown) when the Food has notes, so purchasing advice is available while shopping

`sourcing_info` is the editable source of truth. `sourcing_info_markdown` is
rendered and sanitized by Tandoor and is never accepted on writes; search and
shopping results omit it because it duplicates the Markdown. These fields
require a Tandoor release that includes Food sourcing information. Older
instances remain readable but return neither field, and because Django REST
Framework silently ignores unknown fields, a sourcing write would otherwise
succeed without storing anything. The MCP detects this from the response:
`update_food` returns an error and `create_food` returns the new Food with a
`warning`.

### Recipe Books

- `get_recipe_books` — List all recipe books/collections, optionally filtered by name
- `create_recipe_book` — Create a new recipe book/collection
- `update_recipe_book` — Rename or update the description of a recipe book/collection
- `delete_recipe_book` — Delete a recipe book/collection by ID
- `add_to_recipe_book` — Add a recipe to a recipe book
- `remove_from_recipe_book` — Remove a recipe from a recipe book by entry ID
- `get_recipe_book_entries` — List which recipes are in which recipe books

### Shopping Lists

Items are referred to by food name (e.g. `"lemons"`, plurals match) or entry ID; lists, stores, and categories by name or ID.

**Items**
- `add_to_shopping_list` — Add items in plain words: `["3 lemons", "2 lb chicken thighs", "milk", "a dozen eggs"]`. Units map to existing ones, new foods are created, and an existing line for the same food/unit is increased instead of duplicated. Optional `shopping_list` puts them on a named list
- `get_shopping_list` — The whole list with each item's category, named lists, and whether it came from a recipe; optional `shopping_list` filter
- `update_shopping_list_item` — Change one item's amount, unit, food, checked status, or named lists
- `check_shopping_items` — Check off items by name or ID (`checked: false` unchecks)
- `remove_from_shopping_list` — Remove items without checking them off
- `clear_shopping_list` — Clear checked items and mark them on hand in the pantry

**Recipes on the list**
- `add_recipe_to_shopping_list` — Add a recipe's ingredients, scaled to `servings` and linked to the recipe, skipping foods on hand
- `add_meal_plan_to_shopping_list` — Add ingredients for meal plans in a date range as linked groups, skipping foods on hand
- `get_shopping_list_recipes` — Recipes/meal plans on the list with servings and their items
- `update_shopping_list_recipe` — Change a recipe's servings; its items are rescaled
- `remove_recipe_from_shopping_list` — Remove a recipe and the items it added (manually added items stay)

**Named lists** (e.g. "Costco", "Party")
- `get_shopping_lists`, `create_shopping_list`, `update_shopping_list` (name/description/color), `delete_shopping_list` (items stay on the main list)

**Stores and aisles**
- `get_supermarkets` — Stores with their aisle order (optional `query`)
- `create_supermarket` / `update_supermarket` — Name, description, and `category_order` (the full walking order of aisles; missing categories are created) / `delete_supermarket`
- `get_supermarket_categories`, `create_supermarket_category`, `update_supermarket_category`, `delete_supermarket_category` — Aisles/sections such as Produce or Dairy
- `set_food_category` — Put foods in an aisle so they group together on the list
### Food & Inventory

- `search_foods` — Search for foods/ingredients with fuzzy name matching
- `update_pantry` — Mark foods on hand / not on hand (Tandoor tracks on-hand status, not quantities). Matches existing foods by exact name or plural (case-insensitive) and creates foods that don't exist yet when marked available (optional `supermarket_category`; disable with `create_missing: false`)
- `find_duplicate_foods` — List foods that look like duplicates (same name apart from case, spacing, or a simple plural)
- `merge_foods` — Merge one food into another by ID; recipes and shopping entries move to the target and the source food is **deleted**
- `suggest_from_inventory` — Get recipe suggestions based on current inventory
- `get_unit_conversions` — List unit conversions, optionally filtered by food ID

### Meal Planning

- `get_meal_plans` — Get meal plans for a date range
- `get_meal_plan_gallery` — Render up to 31 meal-plan entries as polished cards with authoritative recipe photos, tags, descriptions, notes, timing, and servings
- `create_meal_plan` — Create a new meal plan
- `update_meal_plan` — Update fields on an existing meal plan (recipe, title, servings, date, meal_type, note) without deleting and recreating it
- `delete_meal_plan` — Delete a meal plan
- `get_meal_types` — Get available meal types

### Metadata

- `get_keywords` — Get all available recipe keywords/tags across every API page
  (see [MCP tool behavior](docs/mcp.md#keyword-listing))
- `get_units` — Get available measurement units

### Cooking History

- `get_cook_log` — Get cooking history
- `log_cooked_recipe` — Log a cooked recipe

## Troubleshooting

### "Couldn't register with Tandoor's sign-in service" (Claude connector)

This happens when the connector URL points to a legacy SSE endpoint (`/sse`). Claude probes for OAuth and fails. Fix: use the Streamable HTTP endpoint (`/mcp`).

### "Authentication credentials were not provided"

The `Authorization: Bearer` header is missing (401) or the token is unknown or expired (Tandoor answers 403 with this message). Verify `TANDOOR_AUTH_TOKEN` is set and correct. The server checks the token against Tandoor at startup and exits with `Tandoor rejected TANDOOR_AUTH_TOKEN: 403 ...` rather than serving requests that would all fail.

### "You do not have permission to perform this action"

Tandoor permissions issue. Check that the user has an active `UserSpace` with an `admin` group assignment (see [Tandoor Configuration](#tandoor-configuration) above). Since the startup check requests `/api/keyword/`, a token whose user lacks space access stops the server at startup with this `403` in the log instead of starting with limited functionality.

### "Failed to connect to Tandoor API" at startup

The server validates the token before it opens the MCP listener, so it exits when `TANDOOR_BASE_URL` is unreachable. In Docker Compose the `restart: unless-stopped` policy retries until Tandoor is up; otherwise fix the URL or start Tandoor first.

### Django Scopes Error

```
ScopeError: A scope on dimension(s) space needs to be active for this query
```

The user has no active space. Follow the Tandoor Configuration section to set one up.

### Connection Refused / Network Errors

- Verify `TANDOOR_BASE_URL` is reachable from where the MCP server runs
- Check if Tandoor is running: `curl http://your-tandoor-url/`

## Development

### Building from Source

```bash
cargo build
RUST_LOG=debug cargo run
```

### Running Tests

```bash
# Unit tests — no network or Docker required
cargo test --lib --test test_types --test test_api_shapes --test test_protocol

# Integration tests — requires a live Tandoor instance
TANDOOR_BASE_URL=http://localhost:8080 \
TANDOOR_AUTH_TOKEN=tda_your_token \
cargo test -- --test-threads=1
```

The integration test suite can also be driven through `./scripts/test.sh` which manages a Docker-based Tandoor instance:

```bash
./scripts/test.sh test           # full run: up, test, down
./scripts/test.sh test --keep-running
./scripts/test.sh up / down / logs
```

Set `TANDOOR_TEST_PORT` when port 8080 is already in use:

```bash
TANDOOR_TEST_PORT=18080 ./scripts/test.sh test
```

The stack publishes Tandoor on the Docker *host*. When the active Docker
context points at a remote engine (`ssh://` or `tcp://`), the script derives the
host from that context automatically; set `TANDOOR_TEST_HOST` to override it:

```bash
TANDOOR_TEST_HOST=192.168.1.50 ./scripts/test.sh test
```

**Testing requirements**: Docker + Docker Compose installed and the selected
Tandoor test host and port reachable from this machine.

### CI

GitHub Actions runs on every push and pull request:

| Job               | What it does                                                         |
| ----------------- | -------------------------------------------------------------------- |
| `unit-test`       | fmt, strict clippy, domain/API-shape/protocol tests (no Docker)       |
| `integration-test`| spins up Postgres + Tandoor, creates a test user, runs all tests     |
| `security`        | blocking `cargo audit` for known vulnerabilities                    |
| `docker-build`    | builds the image, checks it exits without Tandoor, then serves MCP against a stub |

The `publish` workflow builds natively for `linux/amd64` and `linux/arm64`,
combines the image manifests, and pushes to the repository's GHCR package on a
`main` push or version tag.

## Fork integration

The expanded API coverage was adapted from
[`ryanmac8/tandoor-mcp`](https://github.com/ryanmac8/tandoor-mcp) while retaining
this repository's rmcp 3.6 transport, protocol discovery, origin validation,
typed API compatibility, lockfile, and local-bind security defaults. See
[`docs/fork-port.md`](docs/fork-port.md) for the source mapping and deliberate
security deviations.
