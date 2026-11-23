# API reference

All application operations live under `/api/v1/`. This index is generated
from an exported schema of the actual server, preserving Swagger's groups.
Search by path, method or English operation summary.

- [Download OpenAPI JSON](/openapi.json) for request/response schemas,
  error responses, parameter types and code generation.
- Enable `docs.enabled` on an instance for interactive Swagger at `/docs`
  and its live schema at `/openapi.json`. They are absent when disabled.
- Read the [client guide](../clients/overview.md) for complete workflows.

The static schema represents the default **public catalog** policy.
An instance with `library.public_catalog = false` requires bearer auth for
catalog reads; its live schema reflects that setting. "Public by default"
does not bypass per-resource rules. Personal and administrator operations
still require the appropriate credentials.

<ApiReference />
