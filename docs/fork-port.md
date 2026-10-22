# Fork port status

This document tracks the selective port from
[`ryanmac8/tandoor-mcp`](https://github.com/ryanmac8/tandoor-mcp) onto the
`feature-fixes` branch. The local `rmcp` 3.6 Streamable HTTP implementation,
MCP protocol support, origin validation, and reproducible lockfile remain the
foundation.

## Scope

| Fork work | Local treatment | Status |
| --- | --- | --- |
| PR 1: transport, token auth, API shapes | Kept the newer local implementation and regression coverage | Complete |
| PR 3: dependency advisories | Updated compatible dependencies and retained `Cargo.lock` | Complete |
| PRs 4–8, 11: recipe, pantry, CRUD | Ported behavior through local client and MCP conventions | Complete |
| PR 9: native image builds | Adapted publishing workflow and caching | Complete |
| PR 12: shopping workflows and CRUD | Ported with 100-page bounds and exact destructive resolution | Complete |
| PRs 13–14: recipe photos | Ported with SSRF-safe streaming downloads | Complete |
| PR 15: tool annotations | Applied local annotation conventions to all 54 tools | Complete |
| PR 16: Docker ABI fix | Pinned the Bookworm builder; startup verification is in CI | Complete |
| PR 2: Unraid template | Excluded by scope | Not planned |

## Implementation phases

1. Establish a passing local baseline and land security/correctness fixes.
2. Add tested parsing, pagination, matching, and patch-building helpers.
3. Improve existing recipe, shopping, pantry, and build behavior.
4. Add the selected recipe, recipe-book, shopping, food, and meal-plan tools.
5. Run protocol, integration, audit, release, and container verification.

## Required design deviations

- Do not copy the fork's `rmcp` 0.3 transport or public bind default.
- Keep `Cargo.lock`; dependency resolution must be reproducible.
- Image URLs must be HTTPS, resolve only to public addresses, revalidate every
  redirect, stream through a size limit, and enforce supported image media
  types.
- Destructive operations use exact identifiers and destructive MCP
  annotations. Composite operations validate before their first write and
  report partial-write context when Tandoor cannot provide a transaction.
- Collection scans use explicit page and date-range bounds.

## Verification log

- Baseline domain/API-shape tests passed; Docker-backed tests required a running
  local Tandoor instance.
- `cargo check --all-targets`: passed after integration.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- Domain, API-shape, MCP protocol, and fork type tests: passed.
- Locked release build: passed.
- `cargo audit`: passed with no vulnerable dependencies.
- Bookworm/distroless production image build and process startup smoke test:
  passed.
- Docker-backed Tandoor tests were attempted on an isolated port. The active
  remote Docker context did not forward the Compose port back to this host, so
  the live API suite could not connect. During that attempt the runner was
  updated for configurable host ports, current Tandoor's internal port 80,
  host-side readiness probes, and longer first-run migrations. All integration
  targets compile under strict Clippy and remain enabled as a blocking CI job.

