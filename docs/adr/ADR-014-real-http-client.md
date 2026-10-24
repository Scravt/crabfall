# ADR-014 — Cookies, redirects, compression and forms

Status: **accepted** · 19 Oct 2026

## Context

Crabfall's client sends exactly what the scenario says and returns the first response, as it arrives. With real APIs that clashes with four things any browser or HTTP client handles on its own:

- **Cookies:** APIs with cookie sessions (Rails, Django, Laravel, many BFFs) needed an `extract` of `set-cookie` and a hand-written `cookie` header on every step.
- **Redirects:** they weren't followed; a 302 after a login was left as a non-2xx response.
- **Compression:** the server didn't compress because nobody asked it to, so the traffic measured was one real clients don't generate. And if the scenario asked for it, checks failed on a compressed body.
- **Forms:** an `application/x-www-form-urlencoded` body had to be built by hand with `body` and the header.

## Decision

By default, Crabfall behaves like a real client. Each behavior can be turned off.

### Cookies

- **One jar per VU:** each VU stores the cookies the server sends it and sends them back, with a browser's rules (domain, path, expiry, `Secure` only over HTTPS). `cookie_store` (RFC 6265) implements it.
- **Lifetime:** the jar lives from the VU's `vu_setup` to its last iteration, and the renewal (`vu_refresh`) uses the same jar. Cookies set in one iteration are still there in the next one of the same VU, like a returning user. `setup` uses a jar of its own, which belongs to no VU.
- **The scenario's own header:** a `cookie` header from the scenario is sent along with the jar's, not instead of them.
- **Configuration:** `cookies: false` at the scenario's root turns it off. It's signed only if it's `false`.
- **A fix along the way:** without `vu_setup`, all VUs shared one empty session. Now each VU has its own, so cookies don't mix.

### Redirects

- **How many:** each step follows up to 10 redirects (301, 302, 303, 307 and 308). `redirects: N` on the request or in `defaults` changes the number; `0` follows none. The validation cap is 30.
- **Where to:** only to hosts in `allowed_hosts`. A redirect to another host is a failure of type `redirect` and isn't sent.
- **Method and body:** 303, and 301 or 302 after a POST, become GET without a body, as in browsers. 307 and 308 repeat the method and the body.
- **Credentials:** `authorization` and the scenario's own cookie don't travel to another host. The jar's follow their own domain rules.
- **Metrics:** the step's latency includes every hop, the timeout covers the whole chain and the status is that of the last response. Bytes add up over every hop. `crabfall check` shows each hop.
- **Signature:** `redirects` is signed only if it isn't 10.

### Compression

- **What is asked for:** each request sends `accept-encoding: gzip, br`, like real clients. An `accept-encoding` header from the scenario overrides it; `identity` asks for no compression.
- **What is decompressed:** the body is decompressed according to `content-encoding` (gzip, deflate or br) so checks and `extract` see the content, up to 64 MB decompressed. More than that is a failure of type `protocol`.
- **Bytes:** the measured bytes (ADR-013) are still those on the wire, that is, the compressed ones.
- **Signature:** the header goes in the step's header list, so it's signed like the `user-agent`. Approvals from before this change no longer hold.

### Forms

`form: { user: "{{users.email}}", remember: true }` builds an `application/x-www-form-urlencoded` body with the fields in the scenario's order. Values are text (accepting variables), numbers or booleans. It excludes `json`, `body` and `multipart`.

### `crabfall check` doesn't leave loopback

A redirect could take the smoke check to another host in `allowed_hosts`. Now `check`'s client refuses to connect to any address outside loopback, wherever the request comes from (ADR-004 §4).

### What whoever approves sees

`approve` and the `run` header show an `http` line, for example `cookies per VU · follows up to 10 redirects (only to allowed_hosts) · accepts gzip, br`.

## Consequences

- **For:** APIs with cookie sessions, logins that redirect and servers that compress work without extra configuration. Bytes per second reflect what a real client would send.
- **Against:** they change the traffic of existing scenarios (more headers, and redirects that weren't followed before), so they have to be approved again. There is no "clean" jar per iteration: a scenario that needs every iteration to start without cookies has to use `cookies: false` and handle them by hand.
