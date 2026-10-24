# ADR-003 — An HTTP client on hyper and an open-model scheduler

Status: **accepted** · 12 Oct 2026 · based on `spikes/load-spike`

## Context

Diagnosing requires knowing where each request's time goes (DNS, connect, TLS, waiting on the server, download), and for the numbers to be trustworthy the generator can't slow down when the server slows down (coordinated omission). `reqwest` doesn't expose per-phase timings.

## Decision

### 1. Client: `hyper` directly, with its own connections

The engine opens its connections (`tokio::net::lookup_host` → `TcpStream::connect` → `hyper::client::conn::http1::handshake`) and reuses them with its own pool. That way it measures each phase separately:

| Phase | How it is measured |
| --- | --- |
| DNS | `lookup_host`, only on new connections |
| connect | `TcpStream::connect`, only on new connections |
| TLS | the `tokio-rustls` handshake, only on new connections (**pending**: the spike is HTTP only) |
| TTFB | from `send_request` until the headers arrive |
| receiving | reading the whole body |

`sending` is not split from TTFB: with small bodies it adds nothing, and hyper doesn't expose it easily.

### 2. Scheduler: open model with dropped iterations

- Each iteration has a **planned instant** (`start + i / rate`); the scheduler sleeps until that instant and dispatches it **without waiting for responses**.
- **Latency is measured from the planned instant**, not from the actual send. Service time (from the send) is reported too.
- `max_vus` bounds the requests in flight. If no VU is free, the iteration **is dropped and counted**, never delayed. Dropped iterations are the "the server can't keep up" signal.
- The **scheduler lag** (actual − planned dispatch) is measured. If its p99 goes over 5 ms, a warning says the generator may be the bottleneck.

### 3. Windows: timer resolution at 1 ms

On Windows the timer has ~15.6 ms resolution by default and every `sleep` is rounded up. Without fixing it, the scheduler's p99 lag was 27 ms. The engine (and `lab-server`) call `timeBeginPeriod(1)` at startup.

## Evidence (Windows 10, loopback, release)

**10,000 requests at 1,000/s against `/delay/5`:** the send rate was 1000.1/s and the scheduler's p99 lag, 2.2 ms.

| ms | p50 | p90 | p95 | p99 | p99.9 |
| --- | --- | --- | --- | --- | --- |
| spike (service) | 6.02 | 6.88 | 7.04 | 7.11 | 7.72 |
| spike (from planned) | 7.02 | 7.93 | 8.20 | 8.82 | 9.27 |
| oha `--latency-correction` | 6.03 | 7.01 | 7.05 | 7.55 | 8.16 |

**200/s against `/pool/50` with pool=4 (capacity: 80/s), `max_vus` 100:** the scheduler kept to the plan (2 ms p99 lag). 880 requests were sent, with a p50 latency of 1.27 s, and 1,120 iterations were dropped, with a warning. The actual throughput (~79/s) matches the server's capacity.

## Consequences

- There is more of our own code than with `reqwest`: the connection pool, keep-alive handling and, later, TLS and redirects.
- Latency "from planned" includes ~1 ms of scheduler jitter. If more precision is needed, the last millisecond can be spin-waited.
- Pending: measure TLS with `tokio-rustls` and test HTTP/1.1 against real servers (connection closes, `Connection: close`).

## Extension (12 Oct 2026): `ramping_rate` and `constant_vus`

- **`ramping_rate`** uses the same open model with an exact schedule: iteration *i* starts when the integral of the rate reaches *i*. On a linear segment that means solving `from·t + (to−from)/(2d)·t² = k`, with no accumulated rounding error (`crates/crabfall-core/src/load.rs`). `constant_rate` is a single flat segment.
- **`constant_vus`** is a closed model: N tasks looping until `duration`. It has no scheduler lag and no dropped iterations, and its latency **does** suffer from coordinated omission: it is reported, but rate executors are preferred for measuring latency. If the environment sets `limits.max_rps`, a pacer spaces out the starts to stay under it, with a warning when it had to slow down.
