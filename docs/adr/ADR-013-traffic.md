# ADR-013 — Bytes sent and received

Status: **accepted** · 19 Oct 2026

## Context

With `multipart` (ADR-012) Crabfall sends large files. If latency rises, it may be the server or it may be the bandwidth between the generator and the server, and until now the results didn't show how many bytes moved.

## Decision

### What is measured

- **The socket's bytes**, not the body's: a wrapper around the `TcpStream` counts what is read and written. That includes headers, the multipart envelope, chunked encoding and, with HTTPS, encryption and the TLS handshake. It's what occupies the link, which is what matters when talking about bandwidth.
- **Per request:** each request carries its meter and hooks it to the connection while using it. HTTP/1.1 sends one request at a time per connection, so everything going through the socket at that moment belongs to it. A new connection adds its handshake to the first request.
- **Even without a response:** the meter belongs to the request, not the response. If a request times out after sending the file, those bytes count. That's exactly the case that matters when the link is saturated.
- **Only the load:** requests from `setup`, `vu_setup` and `vu_refresh` don't count, as in the other metrics.

### Where it shows up

- `results.json`: `bytes_sent` and `bytes_received` per step and per second in the series, and in `totals` along with `send_rate` and `receive_rate` (bytes per second, the run's average). Old runs don't have them and read as 0.
- `crabfall run`: a `traffic` line with the total, the average and the per-second peak, also in Mbit/s because that's how links are measured.
- The report: a tile, a per-second chart and the bytes per request of each step.

### What was ruled out

- **Counting the body:** it's exact and easy to test, but it leaves out headers and TLS and says nothing about a timeout in the middle of a send.
- **A sampled global counter:** it doesn't separate by step and mixes the `vu_setup` and `vu_refresh` traffic with the load's.

## Consequences

- **For:** you can see whether the traffic hits a ceiling while latency rises, and how many bytes each step moves.
- **Against:** Crabfall doesn't know the link's capacity, so at first there was no diagnosis rule that said "bandwidth", and `crabfall compare` didn't compare bytes per request. Both were added later: `bandwidth_limit` (ADR-005, 20 Oct 2026) and the size warning in `compare` (ADR-006).
