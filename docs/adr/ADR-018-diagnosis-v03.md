# ADR-018 — Diagnosis for v0.3: where the time goes, fixed concurrency, and turning rules off

Status: **accepted** · 24 Oct 2026 · extends [ADR-005](ADR-005-diagnosis.md) · from the [diagnosis study](../studies/diagnosis-comparison.md)

## Context

The [study](../studies/diagnosis-comparison.md) compared Crabfall's rules with nine tools and recommended, for v0.3: keeping the rules deterministic; adding "saturation with fixed VUs" and "server vs payload"; and letting rules be turned off and a few margins tuned, without custom rules. The person accepted it.

Looking at the engine before writing the first rule showed a mistake in the study. With `constant_vus`, all VUs start together and the concurrency is N from the first second to the last. k6's *throughput limit* ("VUs rise, throughput stays flat") needs the concurrency to **change** during the run. With N fixed, the throughput is flat whether the server is healthy or saturated, and the latency is always N ÷ throughput (Little's law): there is no "before" with low concurrency to compare against. That rule belongs with `ramping_vus`, which is still in the backlog.

## Decision

### 1. `time_split`: where a slow step's time goes

Per step, the medians of what is already measured split the latency in three:

- **server**: time to first byte (TTFB), from sending the request until its headers arrive. It includes sending the body: with uploads, it says so.
- **download**: reading the body (`receive_ms`, new in `results.json`).
- **before sending**: the rest of the latency: getting a connection (DNS, connect, TLS) and, in the open model, the wait from the planned instant.

Medians aren't additive, so this is an approximation; it's only used to see which part **dominates**.

| Condition | Value |
| --- | --- |
| responses | ≥ 100 |
| latency p50 | ≥ 50 ms (fast steps aren't worth splitting) |
| the dominant part | ≥ 60% of the latency p50 |

- **Download dominates** → `medium`: the response size or the link, not the server's processing. Causes: large responses (pagination, unneeded fields, no compression), the link, a server that writes the body slowly (streaming as it generates it).
- **Before sending dominates** → `medium`: the client side or the network: new connections (with `new_connections` if it's keep-alive), slow DNS or TLS, or a generator that can't keep up.
- **Server dominates** → `info`, and only without saturation (like `long_tail`): it tells Claude to look at the handler and what it calls (database, other services). With saturation, `breaking_point` or `capacity_exceeded` already says it.
- **Evidence:** the three medians, the shares, and bytes sent and received per request.

### 2. `fixed_concurrency`: what a closed model can't show

With `constant_vus` and a slow step (latency p50 ≥ 100 ms), an `info` finding says that the run doesn't show whether that's the limit, and suggests repeating it with `--executor ramping_rate` to find the breaking point. The evidence has the VUs, iterations per second, the step and its p50. It isn't a detection: it's the honest version of rule 1 until `ramping_vus` exists.

### 3. Turning rules off and tuning margins (next slice)

- `diagnosis:` in the scenario or in the environment of `config.yaml`, and by flag (principle 7):
  ```yaml
  diagnosis:
    off: [long_tail]
    latency_margin: 100ms   # instead of the 50 ms added to "3× the baseline" and in long_tail
    min_requests: 50        # instead of 100 or 200, for low-traffic steps
  ```
- Only named settings with bounds, not the internal constants.
- Not signed (it doesn't change the traffic), like `thresholds`. It goes into `results.json` and the report; `compare` warns if two runs used different settings.
- The skill tells Claude not to turn off or loosen rules so a run "passes"; if it thinks something is noise, it proposes it to the person.

### 4. What stays out

- ML or an LLM inside the engine: the rules stay deterministic (study, §1).
- Saturation in the closed model: with `ramping_vus`.
- Run phases and the trend across runs: later (study, rules 5 and 6).

## Validation

Lab servers in `crates/crabfall-core/tests/diagnose.rs`:

- a body written slowly (headers at once, the body in pieces) → `time_split` with download dominant;
- a handler that waits before answering → `time_split` with server dominant (`info`);
- a fast server → no `time_split`;
- `constant_vus` against a slow handler → `fixed_concurrency`; against a fast one, nothing.

## Consequences

- **For:** a slow step now says which side to look at, with data that was already measured. The closed model stops coming out "no findings" when it simply can't see.
- **Against:** the split uses medians, so a step with two very different paths (a cache hit and a miss) can hide one of them; `long_tail` covers part of that. `receive_ms` exists only in new runs.
