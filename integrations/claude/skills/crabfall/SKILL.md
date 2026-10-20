---
name: crabfall
description: Designs and verifies load tests with Crabfall (YAML scenarios in .crabfall/) and analyzes their results. Use when the person asks for a load, stress or performance test of an API, or asks about results in .crabfall/runs/.
---

# Load testing with Crabfall

You design, verify and analyze. **The person approves and runs the load.** Never run `crabfall approve` or `crabfall run` (a hook blocks them, and `crabfall run` refuses any scenario without a signed approval): when the scenario is ready, ask the person to run them.

You can freely use: `crabfall validate`, `crabfall check` (only against loopback), `crabfall schema`, `crabfall findings` and `crabfall compare`.

**Language.** Reply in the person's language. Crabfall prints in English by default, or in Spanish with `--lang es`, `CRABFALL_LANG=es` or `language: es` in `.crabfall/config.yaml`; the texts inside the results (finding titles, causes, warnings) are in the language of the run. The JSON keys and the finding ids are always the same.

## Designing a scenario

1. **Understand the API.** Read the routes and handlers, and the OpenAPI if there is one. For each endpoint, note: method, path, auth, body, valid IDs and where the code is.
2. **Check `.crabfall/config.yaml`** (environments, `base_url`, `allowed_hosts`, `limits`). If it does not exist, create it with a local environment:
   ```yaml
   version: 1
   default_environment: local
   environments:
     local:
       base_url: http://127.0.0.1:3000
       allowed_hosts: [127.0.0.1, localhost]
   ```
3. **Write `.crabfall/scenarios/<name>.yaml`:**
   ```yaml
   version: 1
   name: checkout
   description: 70% browse the catalog, 30% buy.
   meta: { created_by: agent }
   flows:
     browse:
       weight: 70
       steps:
         - name: list_products
           request: { method: GET, url: /products, query: { page: 1 } }
           source: { file: src/products.ts, symbol: listProducts, operation_id: listProducts }
     buy:
       weight: 30
       steps:
         - name: create_order
           request:
             method: POST
             url: /orders
             headers: { Authorization: Bearer dev-token }
             json: { product_id: 42, quantity: 1 }
           tags: [write]
           source: { file: src/orders.ts, symbol: createOrder }
   load:
     executor: constant_rate
     rate: 50/s          # iterations per second, not requests
     duration: 1m
     max_vus: 100
   ```
   Rules:
   - Every step has a stable, unique `name` (`snake_case`). Metrics are grouped by that name.
   - Always fill in `source`: diagnosis uses it to get back to the code.
   - Tag with `tags: [write]` every step that creates, changes or deletes data.
   - The `weight`s reflect the requested traffic mix; they are relative.
   - The request rate is `rate × steps of the longest flow`. It must stay within `limits.max_rps`.
   - `max_vus` ≈ `rate × (expected latency + pauses of the iteration) (s) × 5`, with a minimum of 10. If it is low, `crabfall` drops iterations.
   - **Pauses** (`think`): if they ask to simulate people, put `think: 1s-3s` in `defaults` (or `think` in a step: `2s` fixed, `1s-3s` random; `0s` removes it). It goes after the step, does not count in the latency and occupies the VU. With `constant_vus` it is almost mandatory: without pauses, 10 VUs are not 10 people. Not allowed in `setup`, and `crabfall check` ignores it.
   - If you do not know a value, write `{ todo: "what is needed" }`. Do not invent IDs or credentials.
   - **Variables:** `vars:` at the root and `{{name}}` in `url`, `query`, `headers` and bodies. If a JSON value is exactly `"{{x}}"`, it keeps its type (number, boolean). Dynamic: `{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt(1,100)}}`. Credentials: `{{env.NAME}}`, declaring `NAME` in `secrets:` of `config.yaml` (the value comes from the environment or `.env`; never write it in the scenario). The host of a URL cannot have variables.
   - **`defaults:`** `headers`, `timeout`, `think` and `redirects` common to every step of `flows`.
   - **Chaining requests:** `setup:` (a list of steps at the root) runs once before the load; use it for a shared login. `extract:` in a step takes values from the response for the following steps:
     ```yaml
     setup:
       - name: login
         request: { method: POST, url: /auth/login, json: { user: "{{env.API_USER}}", password: "{{env.API_PASSWORD}}" } }
         extract:
           token: { json: $.access_token }      # also { header: location } or { regex: "id=(\d+)" }
     defaults:
       headers: { Authorization: "Bearer {{token}}" }
     ```
     What `setup` extracts is valid for every flow; what a flow extracts, only for the following steps of that iteration. If `setup` fails, the run aborts; if an `extract` fails, that iteration stops.
   - **Login per user** (`vu_setup:`, a list of steps at the root): each VU runs them once before the load and what they extract becomes a variable of that VU. It is the right choice for APIs with sessions: with `setup` everyone shares one token, and with the login as the first step of the flow you mostly measure the login. Use `vu_setup` with a users dataset (`{{users.user}}`): each VU takes a row and the flows see that same row. `vus` (or `max_vus`) VUs are prepared, one login each: size `max_vus` knowing it is the number of logins beforehand. It takes no `think`. If it fails in some VUs, they are left out; if it fails in all of them, the run aborts.
   - **Expiring tokens** (`vu_refresh:` at the root, requires `vu_setup`): if the API issues short tokens (look at the TTL in the code: `expiresIn`, `exp`, `ACCESS_TTL`…) and the run lasts longer than that, add `vu_refresh` with `every` shorter than the token lifetime minus the length of an iteration. If there is a refresh endpoint, put it in `steps` and extract the `refresh_token` in `vu_setup`; otherwise, without `steps` it repeats the login. It runs in the background and is not part of the metrics. Without it, a long run shows 401s that are not the system's fault: if you see 401s starting in the middle of the run, suspect this before the server.
   - **Cookie sessions** (Rails, Django, Laravel, express-session, a BFF): no `extract` or `cookie` header needed. Each VU keeps and sends back its cookies on its own, from its `vu_setup` to its last iteration; put the login in `vu_setup` and that is it. If the API uses tokens, cookies do no harm. `cookies: false` at the root only if the scenario needs to handle them by hand.
   - **Redirects:** they are followed automatically (up to 10, only to `allowed_hosts`) and the status is the final one. To check the redirect itself (for example, that the login answers 302 to `/dashboard`), put `redirects: 0` in that request and check `status: 302` and the `location` header. A redirect to a host outside `allowed_hosts` fails with type `redirect`: add the host to the environment or cut the redirect.
   - **Forms** (`application/x-www-form-urlencoded`, typical of web logins and OAuth): `form: { username: "{{users.email}}", password: "{{users.password}}" }` instead of `json`. Text, number or boolean values.
   - **Compression:** every request asks for `gzip, br` and checks see the decompressed body. Do not add `accept-encoding` unless you want to measure without compression (`identity`).
   - **File uploads** (`multipart:` in the `request`, instead of `json` or `body`): each key is a field; a string is text (accepts variables) and `{ file: files/photo.jpg, filename: "photo-{{$uuid}}.jpg", content_type: image/jpeg }` is a file, relative to `.crabfall/` (`filename` and `content_type` are optional). Do not set the `content-type` header: Crabfall sets it with the boundary. Use test files of a realistic size for the API (look at the limit in the code: `limits.fileSize`, `MAX_UPLOAD`…) and keep them in `.crabfall/files/`; if there are none, ask the person for them or generate one without real data. If the API returns a hash or the size, check it. If it uploads and never deletes, the server's disk grows with every iteration: add the step that deletes if it exists, or say so.
   - **Data per iteration** (users, IDs, different searches): `data:` at the root, with files relative to `.crabfall/`. Each iteration takes a row of each dataset its flow uses, and sees it as `{{dataset.column}}` in all its steps:
     ```yaml
     data:
       users: { file: data/users.csv }                     # CSV with a header row: all strings
       products: { file: data/products.json, pick: random } # array of objects: keeps types
     ```
     `pick: sequential` (default) goes through the rows in order and starts over; `random` picks one at random. `unique` uses each row only once in the whole run: use it when a row can only be used once (registering users, consuming coupons). When the rows run out, the load ends there (`data_exhausted` in the results); size the file for the planned iterations, and in `vu_setup` it needs at least one row per VU. Columns cannot be used in `setup`, but they can in `vu_setup` (one row per VU) and in the flows. Do not invent users or credentials: if there is no test data, leave a `todo` and ask the person. Editing the file invalidates the approval.
   - **Checks** (in each step, non-blocking): `- status: 201`, `- status_in: [200, 201]`, `- { json: $.id, exists: true }`, `- { json: $.status, equals: paid }` (accepts `"{{variable}}"`), `- { json: $.name, matches: "^a" }`, `- { header: content-type, contains: json }`, `- body_contains: ok`, `- body_matches: "regex"`. If a step has status checks, `crabfall check` does not require 2xx.
   - **Thresholds** (at the root; if any fails, `crabfall run` exits with code 1):
     ```yaml
     thresholds:
       - { metric: http.latency.p95, lt: 300ms }            # p50…p99.9, avg, max
       - { metric: http.latency.p99, step: create_order, lt: 800ms }   # or tag: write
       - { metric: http.error_rate, lt: 1% }                # no response or status >= 400
       - { metric: checks.pass_rate, gt: 99% }
       - { metric: http.rps, gte: 40 }
     ```
     Checks and thresholds do not change the traffic: editing them does not invalidate the approval.
   - **Executors:**
     - `constant_rate` (`rate`, `duration`, `max_vus`): a fixed rate; the default for measuring latency.
     - `ramping_rate` (`start_rate`, `stages: [{ target: 200/s, duration: 2m }, …]`, `max_vus`): the rate changes linearly; use it to find the breaking point.
     - `constant_vus` (`vus`, `duration`): N users in a loop; the rate depends on the latency. Use it only if they ask for "N concurrent users"; to measure latency prefer the rate executors.
4. **Validate:** `crabfall validate <name> --json`. Fix until `valid` is `true`. Each error has `path`, `line` and `message`.
5. **Verify:** `crabfall check <name> --json` runs each flow once, in series. It only works if the environment points to the local machine; otherwise skip it and say so. Look at the `status` and `body` of each step: a 404 is usually a nonexistent ID, a 401 the auth and a 400 the body.
6. **Hand over** a short summary (flows and weights, rate, duration, maximum requests/s, steps that write) and the exact commands for the person:
   ```
   crabfall approve <name> --env <environment>   # shows the risk summary and asks to type the host
   crabfall run <name> --env <environment>
   ```
   Any later change to the scenario or the environment invalidates the approval: if you fix something, say that it has to be approved again.

   The person can also do it from the terminal UI: `crabfall` without a subcommand opens it (pick the scenario, approve, run and watch it live). You cannot use it: a hook blocks it.

   **Tweaks by flag.** To vary a run without touching the YAML ("the same at twice the rate", "try it with 20 users", "with other users"), use flags instead of editing the scenario: `--rate`, `--duration`, `--vus`, `--max-vus`, `--start-rate`, `--stages 100/s:1m,0/s:30s`, `--graceful-stop`, `--executor`, `--think 1s-3s`, `--refresh-every 4m`, `--data users=data/others.csv`, `--pick users=random` and `--threshold "http.latency.p95<300ms"` (list in `crabfall run --help`; `--threshold` replaces the YAML ones and needs no new approval). Validate them with `crabfall validate <name> <flags> --json`: the error says which flag is missing or extra. The same flags go in `crabfall approve` and in `crabfall run`, because the approval signs them. Hand over both commands with the full flags. If a tweak will always be used, it belongs in the YAML.

## Analyzing results

Always start with `crabfall findings --json` (the most recent run; `crabfall findings <id> --json` for another one). It is a compact summary: totals, thresholds, findings and metrics per step. Open `.crabfall/runs/<id>/results.json` only if you need the per-second series (`series`). For the person there is a graphical report in `.crabfall/runs/<id>/report.html` (`crabfall run` generates it; `crabfall report <id>` regenerates it): mention it when you wrap up the analysis. You read the JSON, not the HTML.

1. **Is the result valid?** If there is an `invalid_result` finding, say so first and draw no conclusions from that run. If `stopped_at_s` is set, the person stopped the run at that second: say it is incomplete and that its numbers only cover what ran.
2. **Findings (`findings[]`)**, from most to least severe. Each one has `title`, `step`, `source`, `window`, `evidence` (the numbers) and `causes` (probable causes, not conclusions):
   - `breaking_point`: with `ramping_rate`, from which rate it degrades and which step worsens most.
   - `capacity_exceeded`: the requested rate exceeds what the system sustains.
   - `error_threshold`: a step starts failing from a certain rate (or from the start: probably a functional error). With 429, if the server declared its limit, the cause and `evidence.rate_limit` carry the quota, the window and the `Retry-After`: look in the code for where that number is configured and whether it is per IP, per user or per token.
   - `degradation_over_time`: with constant load, latency rises steadily.
   - `long_tail`: p99 far from p50 under steady load.
   - `new_connections`: connections are not reused.
   - `bandwidth_limit`: traffic (bytes per second) levels off while the load asks for more and latency rises: the limit may be the link, not the server. Before blaming the code, suggest measuring the link or running the generator next to the server.
3. **Go to the code.** For each finding with a `source`, open that file and symbol and look for what would explain the evidence: synchronous CPU work, small pools, locks, queries without an index, growing collections, external calls without a concurrency limit.
4. **Report in two parts, always separate:**
   - **Measured**: what the finding says, with its numbers (`evidence`).
   - **Hypothesis**: your explanation from the code, with file and line, marked as a hypothesis, and how to confirm it (for example, "if it is the pool, raising it from 5 to 20 should move the break from ~170 to ~650 it/s; run the same scenario again").
5. If there are no findings, look at the thresholds and the p95/p99 per step, and say that no known pattern was detected; do not invent problems.

**Bytes.** Each step has `bytes_sent` and `bytes_received` (bytes on the socket: headers, body and TLS), and `totals` has `send_rate` and `receive_rate` in bytes per second; the series has them per second. With large files or responses, if latency rises while bytes per second stop growing even though the load keeps growing, the limit may be the bandwidth and not the server: say so as a hypothesis (multiply by 8 to compare with a link in Mbit/s) and suggest confirming it by running the generator closer to the server or with smaller files.

## Comparing runs

When the person changed something (code, configuration, infrastructure) and ran the same scenario again, or asks "did it improve?":

1. `crabfall compare --json` compares the most recent run with the previous one of the same scenario and environment (`crabfall compare <base> <new> --json` to choose them). If the project has a baseline (`.crabfall/baselines/<scenario>.<env>.json`), `crabfall compare --baseline --json` compares against that accepted reference instead; prefer it when the person asks whether something regressed. Only the person decides to move the baseline (`crabfall baseline <run>`).
2. **Read `warnings` first.** If the load is different (`same_load: false`) or the scenario changed, say so: the differences may come from there and not from the change.
3. **Verdict per step (`steps[].change`)**, on p95: `better` and `worse` are large changes, consistent second by second (`sign_test`: in how many seconds it moved in that direction, and the p-value). `uncertain` is a large but inconsistent change: say it may be noise and suggest repeating the run; do not present it as an improvement or a regression. `same` is a small change.
   If `warnings` says a step's response or request changed size, mention it next to that step's latency change: it may be the explanation (more data, other pagination, a larger file) rather than a slower server.
4. **Report** `regressions` and `improvements` with their numbers, and the `resolved` and `new` findings. If a `breaking_point` moved, that is the headline ("the break moved from ~36 to ~140 it/s").
5. If a finding shows as `resolved` but the diagnosis rule changed between `crabfall` versions, point it out: it is not thanks to the change.

## Reference

- `crabfall schema`: the full JSON Schema of the scenario, with a description of each field.
- Units: durations `500ms`, `30s`, `2m`, `1h`; rates `50/s`, `3000/m`.
- Exit codes: `0` ok · `1` failed (`check` with failed steps, thresholds not met, `setup` failed or `compare` with regressions) · `2` invalid scenario or configuration · `3` refused (host not allowed, limits, not loopback, no valid approval).
