# ADR-010 — Per-VU setup

Status: **accepted** · 17 Oct 2026

## Context

With what there was, an API with sessions could be tested in two ways, and both were wrong:

- **Login in `setup`:** only once, so all VUs share the same token. It measures a single session, with its cache and its locks, not a hundred users.
- **Login as the flow's first step:** every iteration logs in, so it mostly measures the login. In shop-api, the login with synchronous `scrypt` was exactly what broke.

Reality is in between: each user logs in once and then browses or buys many times with their session.

## Decision

### `vu_setup`

```yaml
data:
  users: { file: data/users.csv }
vu_setup:
  - name: login
    request:
      method: POST
      url: /auth/login
      json: { user: "{{users.user}}", password: "{{users.password}}" }
    extract:
      token: { json: $.access_token }
defaults:
  headers: { Authorization: "Bearer {{token}}" }
flows:
  buy:
    steps: [...]
```

- **What it is:** steps that **each VU** runs once, in order. What they extract becomes a variable of that VU in all its iterations.
- **What they see:** the global variables, what `setup` extracted, and the datasets. Each VU takes one row from every dataset its `vu_setup` uses. They don't inherit `defaults`, like `setup`, because they usually run before what `defaults` uses exists (`{{token}}`).
- **The dataset belongs to the VU:** if `vu_setup` uses `users`, the flows see their VU's row and don't take another. That way `{{users.user}}` in a flow check is the same user who logged in.
- **Restrictions:** no `think`. Step names are unique across the scenario, as always.

### When it runs: before the load

All VUs are prepared **before** the load starts:

- with `constant_vus`, the `vus`;
- with `constant_rate` and `ramping_rate`, the `max_vus`, because each slot of concurrency is a VU with its session.

Preparing them on demand, on each VU's first iteration, was ruled out. In the open model, that iteration would include the login in its latency, which is measured from the planned instant (ADR-003): the first seconds would show a degradation that doesn't belong to the system under load. Preparing them beforehand keeps the metrics clean, although with a high `max_vus` it means many logins. The `approve` summary says so: `login × 200 VUs = 200 requests before the load`.

The preparation runs with at most 50 VUs at a time and respects `limits.max_rps`. The load starts when it's done. The load's duration doesn't include the preparation.

### In the engine

In the open model, the concurrency semaphore becomes a **VU pool**: an iteration takes a free VU (with its variables) and returns it at the end; if there is none, it's dropped, as before. In the closed model, each VU is a task with its variables. Without `vu_setup`, VUs have no variables and everything behaves as before.

### Failures

- **If it fails for some VUs:** no response, status ≥ 400, a failed `extract` or check. Those VUs are left out, the load runs with the rest and a warning says how many.
- **If it fails for all of them:** the run is aborted with the first reason, as with `setup`.
- **`crabfall check`:** runs `vu_setup` once, for one VU, and then the flows with its variables.

### Signature and results

- **Signature:** the canonical plan includes `vu_setup` only if it exists, so earlier approvals still hold.
- **Results:** `results.json` has `vu_setup`, with the VUs prepared, the ones that failed, the duration and, per step, requests, failures, percentiles and reasons. It doesn't go into the steps' metrics or the diagnosis.
- **Where it's shown:** the `run` summary and the HTML report show a separate section.

## Consequences

- **For:** real per-user sessions, without polluting the load metrics. Checks can verify that each response belongs to the right user.
- **Against:** a generous `max_vus` multiplies the logins beforehand. A token that expires during the load is renewed with `vu_refresh` (ADR-011).
