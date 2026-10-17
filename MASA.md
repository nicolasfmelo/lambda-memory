# MASA in this codebase

The code follows the [MASA framework](https://www.masa-framework.org/) ([specification](https://github.com/nicolasfmelo/masa-framework/blob/main/specification/SKILL.md), [rulesets](https://github.com/nicolasfmelo/masa-framework/blob/main/specification/references/rulesets.md), [validation](https://github.com/nicolasfmelo/masa-framework/blob/main/specification/references/validation.md)). This page describes how it maps to Rust; MASA has no official Rust guide.

## Layers

| Layer | Module | Owns |
|---|---|---|
| Domain | `src/domain_models` | Typed identities, sources, revisions, timeline entries, diagnostic snapshots, domain errors. Depends on nothing else. |
| Engines | `src/engines` | Pure rules: capture validity, scope, temporal ordering, fusion, selection, context budget, timeline policy. Depend only on domain; no I/O, no clock. |
| Integrations | `src/integrations` | The embedded store (transactions, migrations, keyset pages), the OpenRouter HTTP client and its wire DTOs, configuration. |
| Services | `src/services` | Orchestration: capture, idle dreaming, recall, timeline, diagnostics. They call engines for decisions and integrations for effects. |
| Delivery | `src/delivery` | Authenticated MCP framing over stdio, argument validation, dispatch to services. Thin by design. |

`src/main.rs` composes the runtime for `--serve-mcp` and the maintenance commands.

## Rules applied

- Data is dressed at the boundaries: wire and SQL types stay in integrations and delivery; business layers only see domain types.
- Dependencies are constructed explicitly and statically; no service locator.
- Identities are distinct value types. Database record IDs and bare strings are never used as owner, project or source identity, and actor provenance is kept separate from the owner boundary.
- Errors are semantic domain errors, mapped to JSON-RPC codes only in delivery.
- Query planning and Luna selection are integrations, because they make network calls; the service bounds their output with the authoritative request. The model never writes database queries.
- Read-only diagnostics (`memory_status`, `memory_activity`) use domain snapshots, never call providers and never postpone dreaming.

## Checking the boundaries

`scripts/check_layers.py` scans `crate::` references for forbidden edges, flags I/O or clock use in pure layers, rejects wire and SQL types in business layers and catches delivery bypasses. It runs in CI. It does not see through every alias or macro, so reviews still matter.
