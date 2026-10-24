# ADR-011 — Renewing each VU's session

Status: **accepted** · 18 Oct 2026

## Context

With `vu_setup` (ADR-010), each VU logs in once and uses that token for the whole run. But in real APIs the access token expires within minutes. In `market-api` it lasts 5 minutes: a longer run starts getting 401s, and the report shows a drop that doesn't belong to the system under load but to the client.

## Decision

### `vu_refresh`

```yaml
vu_setup:
  - name: login
    request: { method: POST, url: /auth/login, json: { ... } }
    extract:
      token: { json: $.access_token }
      refresh_token: { json: $.refresh_token }
vu_refresh:
  every: 4m
  steps:
    - name: refresh
      request:
        method: POST
        url: /auth/refresh
        json: { refresh_token: "{{refresh_token}}" }
      extract:
        token: { json: $.access_token }
        refresh_token: { json: $.refresh_token }
```

- **`every`:** how often each VU renews, counted from the end of its `vu_setup` or its last renewal.
- **`steps`:** optional; they see the VU's variables, including its row of each dataset. What they extract overrides those variables. Without `steps`, `vu_setup` is repeated, that is, logging in again: useful for APIs without a refresh token.
- **Restrictions:** it requires `vu_setup`. Its steps take no `think` and their names are unique across the scenario.
- **New variables:** what the renewal extracts isn't added to what the flows see. A variable that only existed after renewing would be missing in the first iterations, so it is rejected when building the plan.
- **By flag:** `--refresh-every 4m` changes `every`. If the scenario has no `vu_refresh`, it creates one that repeats `vu_setup`. It is signed like any tweak.

### When it runs: in the background

A separate task renews each session when its time comes, whether the VU is idle or in an iteration:

- **The iteration in progress isn't touched:** it took its variables when it started and goes on with the previous token, which is still valid. The next one takes the new one.
- **It takes no capacity away from the load:** the VU stays available while renewing. In the open model this matters: since all VUs are prepared together, they expire together.
- **Concurrency:** at most 50 renewals at a time, as in `vu_setup`.
- **End of the load:** after it ends, there are no more renewals.

Two alternatives were ruled out:

- **Renewing between iterations, taking the VU out of the pool:** that was the first version. In the open model, all VUs left at once when they expired and the load dropped iterations. A test showed it.
- **Renewing on a 401 and retrying the step:** it measures a 401 and a retry that a well-built client wouldn't have, and it doesn't work for APIs that don't answer 401 on expiry.

The rule for choosing `every`: less than the token's lifetime, minus how long an iteration takes, because the iteration uses the token it had when it started.

### Failures

- **If a renewal fails** (no response, status ≥ 400, a failed `extract` or check), that VU stops iterating. In the open model it leaves the pool. Going on with an expired token would only add 401s that don't belong to the system.
- **Warning:** at the end, a warning says how many VUs were lost.
- **`crabfall check`:** runs the renewal once, after the flows.

### Signature and results

- **Signature:** the canonical plan includes `vu_refresh` (`every` and steps) only if it exists, so earlier approvals still hold.
- **Results:** `results.json` has `vu_refresh` with `every_s`, the renewals, the lost VUs and, per step, requests, failures, percentiles and reasons. It doesn't go into the steps' metrics or the diagnosis.
- **Where it's shown:**
  - `approve`: the request bound, VUs × possible renewals × steps;
  - the `run` header and summary;
  - the HTML report, which shows it separately.

## Consequences

- **For:** long runs against APIs with short tokens, without client-side 401s and with clean metrics. With a refresh token or by logging in again.
- **Against:** the token's lifetime isn't read from the response (`expires_in`); it has to be set by hand in `every`. If the server shortens the TTL, 401s show up again, although that is exactly what the report should show.

## Diagnosing expired sessions

The first run on `market-api` without `vu_refresh`, with 60 s tokens, showed that the diagnosis of 401s was wrong. It said "fails from the start (200%)" about a step that had answered fine for a minute. There were two bugs in `error_threshold`:

- **Errors counted twice:** a 401 without the field the `extract` was looking for was counted twice in the per-second series. Now it's one error.
- **"From the start" by mistake:** with pauses, the step had 2 or 3 requests per second and the rule waited for the first second with 5 or more. The 401s of the earlier seconds were counted as "errors from before", and it looked like it failed from the start. Now the window starts at the first second that was already failing, and the rate is that of the whole window.

Also, if 401s dominate and start in the middle of the run, the finding says "the session expired". The first cause depends on how the token is obtained: with `setup`, move the login to `vu_setup`; with `vu_setup`, add `vu_refresh`; with `vu_refresh`, lower `every`.
