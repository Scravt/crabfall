# ADR-002 — Scenario format

Status: **accepted** · 10 Oct 2026 · revised 12 Oct 2026 (ADR-001: Claude-first)

## Context

The scenario is the tool's central contract. Claude writes it (ADR-001), or a person by hand; later, the importers too. The approval signs it, the engine runs it and the diagnosis hangs its findings on it. It also lives versioned in Git, in the users' repos. Changing it after v0.1 breaks everyone, so it is decided now.

Requirements, by priority:

1. **Statically analyzable.** Before running, we must be able to compute the target hosts, the maximum request rate and the duration, for the approval summary and the hard limits.
2. **Easy for an LLM to write correctly.** Validated by JSON Schema, with errors that point at the exact field, and no unknown fields silently ignored.
3. **Readable and editable by humans**, with clean diffs in Git.
4. **A natural target for future importers**, without losing what matters.

## Decision

### 1. Declarative data in YAML, not code

The scenario is a YAML document (JSON is accepted too) validated against a JSON Schema generated from the Rust types with `schemars`.

**Rejected alternatives:**

- **JavaScript scripts, like k6:** they can't be analyzed without running them, so there is no reliable approval summary. Also, an agent writing code that the tool runs breaks principle 1.
- **A custom format, like Bruno's `.bru`:** it needs its own parser, highlighting and validation, and LLMs don't know it.
- **TOML:** deep nesting (flows → steps → request → body) becomes unreadable.

The cost of being declarative is that there is no arbitrary logic (ifs, loops, computations). `extract`, interpolation and dynamic placeholders cover that. If a real case demands it, a small expression language will be considered; never arbitrary code.

### 2. The approval does not live in the file

The scenario **has no `status` field**. A scenario is approved if and only if `.crabfall/approvals/` holds a valid signature for its current hash. Any edit changes the hash and the scenario goes back to draft automatically, without anyone having to remember to mark it.

**What is signed** (details in ADR-004): the scenario's hash **together with the resolved environment** it will run against (base URL, allowed hosts, variables and limits). Without the environment, an agent could leave an approved scenario untouched and change the `base_url` of `local` to point at production.

**How the hash is computed:** the YAML is parsed into the typed model, serialized to canonical JSON (sorted keys, defaults filled in) and hashed with SHA-256. YAML comments disappear when parsing. **`description` and `meta` are left out**: they don't change the traffic. If a new version of the tool changes a default value, approvals are invalidated, and that is correct: the traffic changed.

### 3. Structure: setup, weighted flows and load

- **`setup`**: steps that run **once per run**, before the load (for example, a login). Their `extract`s become read-only global variables.
- **`flows`**: each iteration picks a flow by its `weight` and runs its `steps` in order. This models a realistic traffic mix (70% browse, 30% buy) in a single scenario. `weight` is optional and defaults to `1`; weights are relative and don't have to add up to 100.
- **`defaults`**: headers and `timeout` inherited **only by the steps in `flows`**. `setup` inherits nothing, because it usually runs before the variables that `defaults` uses exist (for example, `{{token}}`).
- **`load`**: a single executor per scenario. The rate is expressed in **iterations** per second, not requests.

Since a flow can have several steps, the request rate is not the iteration rate. The approval summary shows the maximum request rate computed as `rate × the most steps among the flows`, which is the safe bound. **Hard limits (`limits.max_rps`) are measured in requests per second** and compared against that bound.

### 4. Every step has a stable name, and that name is the metrics key

`name` is required and unique within the scenario (`[A-Za-z0-9_.-]+`, compatible with OpenAPI `operationId`s). Metrics, thresholds and findings are grouped by `name`, **never by the interpolated URL**: `/orders/123` and `/orders/456` are the same endpoint. This avoids a series explosion and lets the diagnosis talk about "`create_order`".

### 5. `source` links the step to the code

An optional, free-form field: `file`, `symbol` and `operation_id`. The engine doesn't use it; it only copies it into the findings. Claude fills it in when designing from the code (or from an OpenAPI spec, with `operation_id`). It is what lets the analysis go from a finding straight to the responsible handler.

### 6. Interpolation and types

- `{{var}}`: variables from the scenario, the environment, `--var` on the CLI or an `extract`. Priority, lowest to highest: scenario → environment → CLI → extract.
- `{{env.NAME}}`: system environment variables or `.env` entries. Only those declared under `secrets` in `config.yaml` are allowed. They are redacted in every output (terminal, JSON, reports).
- `{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt(1,100)}}`: dynamic values, a new one on each use.
- **Type preservation:** if a JSON value is *exactly* `"{{var}}"`, it keeps the variable's type (number, boolean, object). Mixed with text, the result is a string. This avoids sending `"42"` where the API expects `42`.

### 7. Strict with the unknown

An unknown field is an **error**, not a warning, because it catches human typos and LLM hallucinations. Fields prefixed with `x-` will be allowed for third-party extensions and ignored by the engine (**not implemented yet**: `serde` can't accept them without losing error locations; it needs a pre-pass over the YAML).

### 8. An explicit `todo` for what is missing

Claude (and future importers) mark the values they don't know with `{ todo: "what is needed" }`. `crabfall validate` fails while any remain and lists each one with its path. That shows exactly what the person has to fill in, instead of the test failing with a 404 in the middle of the load.

### 9. Format versioning

`version: 1` is required. Adding optional fields doesn't bump the version; an incompatible change does, and comes with `crabfall migrate`. The tool rejects versions it doesn't know, with a message saying which version of `crabfall` is needed.

### 10. Parser: `serde-saphyr` + `serde_path_to_error`

Evaluated on 12 Oct 2026 against `serde_yaml_ng` and `serde_norway` (forks of `serde_yaml`, which is archived; `serde_yml` is deprecated). It is the only one that gives line and column in every case tested:

| Case | serde_yaml forks | serde-saphyr + path_to_error |
| --- | --- | --- |
| misspelled field | path + line | path + line |
| wrong type | path + line | path + line |
| unknown variant (`executor`) | path + line | path + line |
| unknown field inside a tagged enum | **nothing** | path + line of the block |
| duplicate key | **no line** | line |

It is also pure Rust, actively maintained, and offers `Spanned<T>`, which locates semantic errors (for example, a repeated step `name`). `validate --json` reports `path`, `line`, `column` and `message`.

## Examples

### Project configuration: `.crabfall/config.yaml`

```yaml
version: 1
default_environment: local

environments:
  local:
    base_url: http://localhost:3000
    allowed_hosts: [localhost, 127.0.0.1]
  staging:
    base_url: https://staging.api.example.com
    allowed_hosts: [staging.api.example.com]
    vars:
      product_id: 1001
    limits:                 # hard limits: no approval goes past them
      max_rps: 200          # requests per second, not iterations
      max_vus: 300
      max_duration: 15m

secrets:                    # names only; the values come from the environment or .env (gitignored)
  - LT_USER
  - LT_PASSWORD
```

### A scenario written by hand or by the agent: `.crabfall/scenarios/checkout.yaml`

```yaml
version: 1
name: checkout
description: Users browsing the catalog; some of them buy.

meta:
  created_by: agent         # agent | human | import:openapi | import:curl ...

vars:
  product_id: 42

setup:
  - name: login
    request:
      method: POST
      url: /auth/login
      json:
        user: "{{env.LT_USER}}"
        password: "{{env.LT_PASSWORD}}"
    checks:
      - status: 200
    extract:
      token: { json: $.access_token }

defaults:                   # applies to every step in the flows
  headers:
    Authorization: "Bearer {{token}}"
  timeout: 5s

flows:
  browse:
    weight: 70
    steps:
      - name: list_products
        request:
          method: GET
          url: /products
          query:
            page: "{{$randomInt(1,20)}}"
        checks:
          - status: 200
        source: { file: src/products/routes.ts, symbol: listProducts }

  buy:
    weight: 30
    steps:
      - name: create_order
        request:
          method: POST
          url: /orders
          json:
            product_id: "{{product_id}}"     # sent as a number
            quantity: 1
            idempotency_key: "{{$uuid}}"
        checks:
          - status: 201
          - json: $.id
            exists: true
        extract:
          order_id: { json: $.id }
        tags: [write]
        source: { file: src/orders/handler.ts, symbol: createOrder }

      - name: get_order
        request:
          method: GET
          url: /orders/{{order_id}}
        checks:
          - status: 200
          - header: content-type
            contains: application/json

load:
  executor: ramping_rate
  start_rate: 10/s
  stages:
    - { target: 100/s, duration: 2m }
    - { target: 100/s, duration: 5m }
    - { target: 0/s, duration: 30s }
  max_vus: 200
  graceful_stop: 30s

thresholds:
  - metric: http.latency.p95
    lt: 300ms
  - metric: http.latency.p99
    step: create_order
    lt: 800ms
  - metric: http.error_rate
    lt: 1%
  - metric: checks.pass_rate
    gt: 99%
```

The approval summary computed from this file with `--env staging`:

```
Scenario    checkout  (changed since the last approval: +1 step in flow "buy")
Environment staging → staging.api.example.com
Load        ramping_rate up to 100 it/s ≈ 200 req/s max (2 steps per iteration)
Duration    7m30s + 30s graceful stop
Limits      within max_rps 200 ✓  max_vus 300 ✓  max_duration 15m ✓
Writes      yes: create_order (POST /orders), tagged [write]
```

### A scenario generated by Claude from an `openapi.yaml`

```yaml
version: 1
name: smoke
description: Generated from openapi.yaml — 12 operations included, 2 skipped (see meta).

meta:
  created_by: agent
  generated_from: openapi.yaml
  skipped:
    - { operation_id: liveFeed, reason: "WebSocket not supported yet" }
    - { operation_id: streamEvents, reason: "text/event-stream not supported" }

vars:
  petId: { todo: "ID of an existing pet in the environment" }

flows:
  main:
    steps:
      - name: listPets
        request:
          method: GET
          url: /pets
          query: { limit: 10 }
        checks: [{ status: 200 }]
        source: { operation_id: listPets }

      - name: createPet
        request:
          method: POST
          url: /pets
          json: { name: "string", tag: "string" }   # example derived from the schema
        checks: [{ status: 201 }]
        source: { operation_id: createPet }

      - name: showPetById
        request:
          method: GET
          url: /pets/{{petId}}
        checks: [{ status: 200 }]
        source: { operation_id: showPetById }

load:
  executor: constant_vus
  vus: 1
  duration: 30s
```

`crabfall validate` answers:

```
✗ vars.petId: pending value (todo) — "ID of an existing pet in the environment"
1 pending. Fill it in by hand, pass it with --var petId=..., or ask the agent to resolve it.
```

## Model quick reference (v1)

| Block | Fields | Notes |
| --- | --- | --- |
| root | `version`, `name`, `description`, `meta`, `vars`, `data`, `cookies`, `setup`, `vu_setup`, `vu_refresh`, `defaults`, `flows`, `load`, `thresholds` | Required: `version`, `name`, `flows`, `load` |
| vu_refresh | `every`, `steps` | `steps` optional: without steps, it repeats `vu_setup` |
| flow | `weight`, `steps` | `weight` optional, defaults to `1` |
| defaults | `headers`, `timeout`, `think`, `redirects` | Only for steps in `flows`, not in `setup` |
| step | `name`, `request`, `checks`, `extract`, `tags`, `source`, `think` | `name` unique across the scenario. `think`: a pause after the step (`2s` or `1s-3s`), not in `setup` |
| request | `method`, `url`, `headers`, `query`, `timeout`, `redirects` + one of `json` / `body` / `multipart` / `form` | `timeout` defaults to 30s and covers every redirect. `redirects` defaults to 10, only to `allowed_hosts` (ADR-014). `url` relative to `base_url` or absolute; hosts are checked against `allowed_hosts` |
| multipart | per field: a string (text) or `{ file, filename, content_type }` | `file` relative to `.crabfall/`; Crabfall sets the request's `content-type` (ADR-012) |
| form | per field: text (with variables), number or boolean | `application/x-www-form-urlencoded`; Crabfall sets the `content-type` (ADR-014) |
| cookies | `true` (default) or `false` | One jar per VU, from `vu_setup` to the last iteration; `setup` uses its own (ADR-014) |
| check | `status`, `status_in`, `json` + (`exists` / `equals` / `matches`), `header` + (`exists` / `equals` / `contains` / `matches`), `body_contains`, `body_matches` | Non-blocking: they count toward `checks.pass_rate`. `equals` accepts `{{variables}}`. The body has its own keys (`body_contains`, `body_matches`) because it has no selector |
| extract | `{ json: <path> }`, `{ header: <name> }`, `{ regex: <pattern> }` | In `setup`: read-only global. In flows: per iteration |
| load | `executor` + its fields: `constant_rate` (`rate`, `duration`, `max_vus`); `ramping_rate` (`start_rate` optional, defaults to `0/s`; `stages` with `target` and `duration`; `max_vus`); `constant_vus` (`vus`, `duration`); later `breakpoint` | All accept `graceful_stop` (default 30s). It is a flat block: `crabfall validate` rejects fields that don't apply to the executor. With `constant_vus` the rate isn't bounded in advance: if the environment sets `limits.max_rps`, the engine spaces out iterations to stay under it |
| threshold | `metric`, optional `step` or `tag`, one of `lt` / `lte` / `gt` / `gte` | Metrics: `http.latency.p50`…`p99.9`, `avg`, `max` (duration); `http.error_rate` (no response or status >= 400) and `checks.pass_rate` (percentage); `http.rps` (number). With no data, the threshold fails |

Units: durations `500ms`, `30s`, `2m`; rates `50/s`, `3000/m`; percentages `1%`.

## Smoke check

Any scenario can be checked with `crabfall check <scenario>`. It runs `setup` and each flow **once**, serially with 1 VU, and **ignores the `load` block**. That's why it isn't an executor: it is a run mode. The rules that make it safe for an agent are in ADR-004.

## Consequences

- **For:** an exact approval summary, strong validation for LLMs, readable diffs and a clear target for future importers. Findings speak in the user's terms (`create_order`), not URLs.
- **Against:** there is no arbitrary logic, so some complex flows will need several scenarios or wait for the expression language. Strict mode means the JSON Schema has to stay up to date (it is generated from the code, so the risk is low).

## Open points

- **Per-VU setup** (each virtual user with its own login): resolved in ADR-010, with `vu_setup` at the root.
- **Tokens that expire during the load:** resolved in ADR-011, with `vu_refresh` at the root.
- **Datasets** (CSV/JSON): resolved in ADR-007, with the `data` field at the root.
- **Think time** between steps: resolved in ADR-008, with `think` on the step and in `defaults`.
