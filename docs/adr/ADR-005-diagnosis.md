# ADR-005 — Diagnosis: deterministic findings + the agent's hypotheses

Status: **accepted** · 12 Oct 2026

## Context

A load tester says "p99 = 2.2 s". What's needed is "`login` saturates at ~36/s; the other steps degrade with it; the code is in `src/auth.ts`". The engine sees the metrics but not the code; Claude sees the code but must not make up metrics.

## Decision

Two layers, always kept apart:

1. **The engine detects, with deterministic rules**, over what was measured: per-second series (global and per step), per-step metrics and totals. Each finding has an `id`, a severity, a title with numbers, the step and its `source`, a time window, `evidence`, and likely causes picked according to the signal (a 429 doesn't suggest the same as a timeout). Same data, same findings.
2. **Claude explains**, going from `source` to the code, and presents what it proposes as **hypotheses**, separate from what was measured (skill, section "Analyzing results").

### Initial rules (`crates/crabfall-core/src/diagnose.rs`)

| id | When | Signal |
| --- | --- | --- |
| `invalid_result` | always | scheduler lag p99 > 5 ms |
| `breaking_point` | `ramping_rate` | the first sustained window (2 of 3) with dropped iterations, p95 ≥ max(3× baseline, baseline + 50 ms) or errors ≥ 5% |
| `capacity_exceeded` | constant rate | > 1% of iterations dropped |
| `degradation_over_time` | constant rate | least-squares line of the p95: end ≥ 2× start, +20 ms, R² ≥ 0.6 |
| `error_threshold` | per step | goes from < 1% to ≥ 5% errors, sustained. The window starts at the first second that was already failing, even if it had few requests. If it's from the start, it's flagged as a likely functional error; if it's 401s in the middle of the run, as an expired session, with the way out depending on how the token is obtained (ADR-011) |
| `long_tail` | per step, without saturation | p99 ≥ 10× p50 and +50 ms, with ≥ 200 requests |
| `new_connections` | per step | new connections above `max_vus` (or `vus`) > 10% of the requests: the pool grows up to the concurrency, so one connection per VU is normal |

The "baseline" is the median p95 of the first windows with data. The "culprit" step is the one that worsens the most relative to its own baseline; if several degrade together, the suggested cause is a shared resource.

### Output

- `results.json` includes `series` and `findings`.
- `crabfall run` prints the findings at the end.
- `crabfall findings [id] [--json]`: the summary for agents (totals, thresholds, findings and per-step metrics, without series or bodies).

## Validation

`crates/crabfall-core/tests/diagnose.rs` starts a server with each pathology and checks that the right finding fires: a small pool (breaking at ~200 it/s), a rate limiter at 150/s (429 from ~150 it/s), work that grows with every request, one request in 25 being slow, `Connection: close` on every response, and a constant rate above capacity. A healthy ramp fires nothing serious. A slow server with keep-alive doesn't fire `new_connections` even if the pool opens dozens of connections. The thresholds are constants at the top of the module, so they can be tuned with the lab.

## Consequences

- The diagnosis only sees the client side. Correlating with server metrics (CPU, pool, GC) is Phase 6 / D4.
- The rules have fixed thresholds: they can give false negatives on very noisy systems. They are tuned with real cases, adding a pathology to the lab for every diagnosis mistake.
- Rules from the original catalog are missing: "server vs payload" (TTFB vs receiving) and contagion between endpoints as a finding of its own (today it shows up inside `breaking_point`).

## Test with shop-api (15 Oct 2026)

- `stress-ramp` (20 → 400 it/s): flat latency across the whole ramp (p95 ~33 ms, the fixed cost of `create_order`) and no findings. Correct: the system didn't saturate.
- `login-ramp` (10 → 80 it/s): `breaking_point` at ~40 it/s (36 req/s) on `login`, with 219 iterations dropped. The cause in the code is `scryptSync` (~28 ms of CPU per call blocking the event loop: 1000 / 28 ≈ 36/s).
- That same run gave a false positive for `new_connections`: 200 new connections with `max_vus: 200`, all reused. The rule was fixed and the pathology was added to the lab (`slow_server_with_keep_alive_has_no_new_connections`).

## The limit the server declares (19 Oct 2026)

A 429 almost always comes with information: `Retry-After` and, more and more, the limit's policy (`RateLimit-Policy` from the IETF drafts, as express-rate-limit sends it; or the classic `X-RateLimit-Limit`). The engine stores that from each 429 in `steps[].rate_limit` (count, minimum and maximum `Retry-After`, quota and window). When `error_threshold` finds that 429s dominate, the cause stops being generic and states the limit with numbers ("declares 30 requests every 60 s (`RateLimit-Policy`) and asks to wait with `Retry-After 23 s`"), and the evidence includes it. `crabfall run` shows it on a `limit` line. Only the numeric form of `Retry-After` is read; an HTTP date is ignored.

## Bandwidth (20 Oct 2026)

`bandwidth_limit` (medium): the traffic (bytes sent + received per second, ADR-013) reaches a ceiling and stays there while the load asks for much more and the p95 shoots up.

- **When it looks:** only if the peak goes over 1 MB/s; below that, bandwidth is almost never the limit.
- **The ceiling:** the first second with at least 85% of the peak; from there, 5 seconds or more, and 70% of them, at that level.
- **What the load asked for:** the offered iterations (started + dropped) after the ceiling, times the bytes per iteration from before. It has to be 25% above what was moved.
- **The latency:** the median p95 after the ceiling, at least 3 times (and 50 ms more than) the one before, as in `breaking_point`.
- **Evidence:** peak, bytes/s moved and asked for, bytes per iteration and the p95s. The causes start with the link (Wi-Fi, VPN, network card, plan), continue with a proxy or CDN that throttles, and end with large responses.
- **Lab:** 100 KB responses through a simulated 2 MB/s "link", under a ramp from 5 to 60 it/s (up to 6 MB/s asked for), give the finding; the same responses without the link don't.
