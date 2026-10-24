# Roadmap — Crabfall

Revised 12 Oct 2026. The previous version (with phases, dates and OKRs) is in the git history.

## Goal

Go from "I have an API" to "I know where it breaks and why" without setting up a project for every test. Claude designs the scenario and analyzes the results; a person approves and runs the load.

## Principles

1. **Claude designs and analyzes; the person authorizes the load.** Claude can only run the smoke check against loopback (ADR-004).
2. **Trustworthy numbers.** No coordinated omission, percentiles with HdrHistogram, and a warning when the generator is the bottleneck.
3. **Findings with evidence.** The engine detects patterns deterministically; Claude's hypotheses about the code are marked as hypotheses.
4. **One core, thin interfaces.** CLI first; then a TUI in the terminal and MCP. The graphical GUI (Tauri) comes later (ADR-001).
5. **Local-first.** YAML scenarios versioned in the project's repo. No accounts, no cloud.
6. **Own use first.** Something is done when it's used in a real case.
7. **Every tweak of a run goes in as a flag.** Anything that would make sense in a customization panel (load, pauses, data, environment, variables) can be passed as `--something` to `validate`, `check`, `approve` and `run`, with the same logic in `crabfall-core`. That way the TUI (and later a GUI) is a thin layer (ADR-008).

## How it's used

```
You:    "build me a load test of the checkout with 200 users"
Claude: reads the code → writes .crabfall/scenarios/checkout.yaml
        → crabfall validate --json → fixes → crabfall check checkout --json → fixes
You:    crabfall approve checkout --env staging
        crabfall run checkout --env staging
Claude: crabfall findings latest --json → reads the code behind each finding → proposes hypotheses
```

## Architecture

```
crates/
  crabfall-core      scenario, validation, hash, engine, metrics, diagnosis
  crabfall-cli       the `crabfall` binary
tools/
  lab-server   axum server with configurable pathologies, for tests
integrations/claude/  Claude Code skill (installed by `crabfall claude install`)
```

`crabfall-core` gets split into smaller crates when a module justifies it, not before.

In the user's repo:

```
.crabfall/
  config.yaml        environments, allowed_hosts, limits, secrets
  scenarios/*.yaml   versioned
  approvals/         local signatures (gitignored)
  runs/<id>/         results (gitignored)
```

## Slices

Each slice works end to end before moving on to the next. Closing it updates the README (status, commands and whatever new changes how it's used); `crates/crabfall-cli/tests/readme.rs` fails if a command or an ADR is missing.

**0. Foundations**
- [x] Cargo workspace with `crabfall-core`, `crabfall-cli` and `lab-server`
- [x] Spike: a client on `hyper` with per-phase timings (DNS, connect, TTFB, receiving) → ADR-003 (TLS pending)
- [x] Spike: a constant-rate scheduler that measures from the planned instant → ADR-003
- [x] Pick a YAML crate with line and column in errors → `serde-saphyr` (ADR-002 §10)
- Done when: 10,000 requests against `lab-server` give percentiles similar to `oha`, and the rate holds even if the server slows down.

**1. First run**
- [x] Minimal model: weighted flows, steps without `extract`, `constant_rate` executor, `json`/`body` bodies, HTTPS
- [x] `crabfall validate [--json]` with the exact error path and pending `todo`s
- [x] `crabfall run` → a terminal summary and `results.json` with p50/p90/p95/p99 per step
- [x] An interim guard (replaced in slice 3 by the signed approval)
- Done: tested against `shop-api` (a local example API, separate repo). **Pending:** TLS against a real server, with `market-api` (Postgres, JWT, rate limit; separate repo, 18 Oct 2026) deployed to a server.

**2. Claude designs**
- [x] `crabfall check [--json]` per ADR-004
- [x] Generated JSON Schema (`schemars`) and `crabfall schema`
- [x] The `crabfall` skill + `crabfall claude install` (ADR-001)
- Done when: Claude builds a scenario from the code that passes `validate` and `check` without me touching the YAML. Tested with `shop-api` (scenario `buscar-y-comprar`) following the skill in this session. **Pending:** repeat it in a fresh Claude Code session, which loads the skill on its own.

**3. Approval**
- [x] `crabfall approve` and the check in `crabfall run`, with signature, keyring and TTY (ADR-004)
- [x] `limits` and `allowed_hosts` in `config.yaml` (done in slice 1)
- [x] `PreToolUse` hook (`crabfall hook pre-tool-use`), brought forward to slice 2
- [x] Guard tests (list in ADR-004; `--var` added 24 Oct 2026: another value invalidates the approval with the diff, the scenario's same value doesn't, and neither does a secret's value)
- Done: in `market-api`, with `smoke` approved, raising the `rate` from 5/s to 6/s made `crabfall run` in the user's terminal refuse with code 3 and show the diff (24 Oct 2026). The test uncovered two details, already fixed: the diff header came out in Spanish in English output, and the default `user-agent` carried the version, so upgrading Crabfall invalidated every approval; now it's signed as `crabfall/<version>` (ADR-004).

**4. Full scenario**
- [x] `setup`, `extract` (json, header, regex), `defaults`, several flows with `weight`
- [x] Interpolation with type preservation and dynamic placeholders, `{{env.X}}` for `secrets`, `--var`, `defaults`
- [x] Checks and thresholds
- [x] `ramping_rate` (exact per-segment schedule) and `constant_vus` (with a pacer if there's `limits.max_rps`) executors
- [x] Exit codes: pass, failed thresholds (1), configuration error (2)
- Done: `shop-api` does a real login in `setup`, chains `token` and `order_id`, and checks with checks and thresholds.

**5. Diagnosis**
- [x] Per-second series (started, dropped, requests, errors, p50/p95/p99), global and per step, in `results.json`
- [x] Rules: breaking point, capacity exceeded, error threshold, degradation over time, long tail, new connections, invalid result (ADR-005)
- [x] `crabfall findings [--json]` with evidence and the step's `source`; findings at the end of `crabfall run`
- [x] Skill: from the finding to the code, separating what was measured from hypotheses
- [x] Validate each rule with a pathology (test servers in `tests/diagnose.rs`)
- Done: in `shop-api`, `login-ramp` gives a `breaking_point` at ~36 req/s and Claude gets to `scryptSync` in `src/auth.ts` (15 Oct 2026). `stress-ramp` doesn't saturate `shop-api` and gives no findings, as it should. The run uncovered a `new_connections` false positive, already fixed (ADR-005).

**6. Comparing runs**
- [x] `crabfall compare [base] [new] [--json]`: by default, the latest against the previous one of the same scenario and environment (ADR-006)
- [x] A per-step verdict on the p95: magnitude (≥ 10% and 2 ms) and a sign test over the paired seconds, to tell change from noise
- [x] Differences in totals, findings (new, resolved, a breaking point that moves) and thresholds; exits with `1` if there are regressions
- [x] Skill: a "Comparing runs" section; `crabfall compare` allowed in `crabfall claude install`
- [x] Tests: unit tests, the engine against a fast and a slow server, and the binary
- Done: in `shop-api`, with `login` using async `scrypt` (branch `fix/async-scrypt`), `login-ramp` holds 80 req/s without drops or findings, and `crabfall compare` against the previous run says "better": `login` p95 7213 → 32 ms in 25 of 29 seconds, the breaking point and `new_connections` resolved (16 Oct 2026).

**7. Datasets**
- [x] `data:` at the root: CSV with a header (strings) or JSON (an array of objects, keeps types), relative to `.crabfall/` (ADR-007)
- [x] `{{dataset.column}}`: one row per iteration for all its steps, `pick: sequential` (default) or `random`; only flows that use the dataset consume rows
- [x] The approval signs each file's SHA-256; `approve` and `run` show the datasets
- [x] Errors with the column or row: an empty or unreadable file, a missing column (with the ones there are), use in `setup`
- [x] Skill: a "Data per iteration" section
- [x] Tests: loading CSV and JSON, row order, signature, errors and the engine against the echo server
- Done: in `shop-api`, `login-users` logs in with a different user per iteration (`data/users.csv`) and asks for random products from `data/products.json`. The approved run (50 iterations, 100 requests) gives 150 checks passed and 0 failed, including `$.id == "{{products.id}}"`, which confirms the row arrives with its type (17 Oct 2026).

**8. Think time and tweaks by flag**
- [x] `think` on the step and in `defaults`: fixed (`2s`) or random (`1s-3s`), after the step; it takes up the VU, doesn't count toward latency; `check` ignores it and `setup` doesn't allow it (ADR-008)
- [x] Tweak flags on `validate`, `check`, `approve` and `run`: `--executor`, `--rate`, `--duration`, `--vus`, `--max-vus`, `--start-rate`, `--stages`, `--graceful-stop`, `--think`, `--data`, `--pick`
- [x] Tweaks are validated again and the error says which flag is missing or extra; the approval signs them; `approve`, `run` and `results.json` show them
- [x] `Overrides` in `crabfall-core`, so the GUI and MCP don't depend on the CLI
- [x] `--threshold "http.latency.p95<300ms"`: replaces the YAML's thresholds; doesn't change the signature (added after closing, 17 Oct 2026)
- [x] Skill: pauses and tweaks by flag; README with examples
- [x] Tests: units, applying tweaks, signature, the engine with pauses and the binary
- Done: in `shop-api`, `login-users --executor constant_vus --vus 5 --duration 20s --think 1s-2s` gives 1.8 iterations per second (36 in 20 s; ~1.7 expected), 108 checks passed and 0 failed, and `login`'s p50 at 34 ms, the same as without pauses: the pause doesn't enter the latency. `results.json` stores the tweaks in `tweaks` and the pauses in `load.think` (17 Oct 2026).

**9. HTML report**
- [x] A self-contained `report.html` per run (no CDN; opens offline), generated by `crabfall run`; `crabfall report [run] [--open]` regenerates it (ADR-009)
- [x] Header with verdict, load, pauses and tweaks; indicators; findings with evidence and a shaded window; comparison with the previous run and p95 before/after with the same load
- [x] Per-second charts with a single axis, crosshair and tooltip (also by keyboard), an optional log scale for latency, p95 per step, light and dark mode, tables as the chart-free view
- [x] Embedded data that can't close the `<script>`; text always with `textContent`
- [x] Checked with headless Edge on the real `shop-api` runs (screenshots in light and dark, mouse simulated through DevTools)
- Done: I reviewed the reports of the `shop-api` runs (the `login-ramp` breaking point and the before and after of the fix) and what happened is clear without looking at the terminal (17 Oct 2026). It will be adjusted when it's integrated with the GUI.

**10. Per-VU setup**
- [x] `vu_setup:` at the root: each VU runs those steps once before the load; what's extracted becomes the VU's variable (ADR-010)
- [x] One row of each dataset per VU; if `vu_setup` uses a dataset, the flows see their VU's row
- [x] Preparation before the load for `vus` or `max_vus`, up to 50 at a time and respecting `limits.max_rps`; the open model hands out VUs from a pool
- [x] VUs that fail are left out with a warning; if all fail, it aborts; `crabfall check` runs it once
- [x] Signed only if it exists; `results.json`, the `run` summary, `approve` and the report show it apart from the load metrics
- [x] Tests: a login per VU with `constant_vus` and with the open model, the right token on each iteration, total failure, validation and signature
- Done: in `shop-api`, `orders-per-user` (3 VUs, one per user in `data/users.csv`) did 3 logins before the load (p50 34 ms) and then 22 buying iterations with 1-2 s pauses: 44 requests, 88 checks passed and 0 failed, so each order belonged to its VU's user (18 Oct 2026).

**11. Session renewal**
- [x] `vu_refresh:` at the root (`every` and optional `steps`): renews each VU's session before the token expires; without `steps`, it repeats `vu_setup` (ADR-011)
- [x] In the background, whether the VU is idle or in an iteration: the iteration in progress keeps the old token and the next one takes the new one, so renewing takes no capacity from the load
- [x] A VU that can't renew stops iterating, with a warning; `crabfall check` runs the renewal once
- [x] `--refresh-every` by flag; signed only if it exists; `results.json`, `approve`, the `run` summary and the report show it separately
- [x] Tests: renewal in both models (also the idle VUs in the pool), the new token in the following iterations, a failed renewal, validation, signature and flag
- [x] Diagnosis: 401s in the middle of a run are flagged as an expired session, with the way out depending on the scenario; fixed the double counting of errors with `extract` and the false "from the start" with few requests per second
- [x] `approve` suggests the `crabfall run` with the same tweaks
- Done: in `market-api`, with 60 s tokens and 20 VUs for 2 minutes, `shop-per-user` got 608 401s from second 59 and the finding said "the session expired" with the `vu_refresh` suggestion. `session-refresh`, the same with renewal through `/auth/refresh` every 45 s, did 40 renewals (p50 5.5 ms), 0 401s and 1,860 of 1,860 checks passed (18 Oct 2026).

**12. File uploads (multipart)**
- [x] `multipart:` on the `request`: text fields with variables and files from `.crabfall/` with optional `filename` and `content_type`; excludes `json` and `body` (ADR-012)
- [x] Files are read once and sent whole on each request (up to 100 MB); a random boundary per step that appears in no file; Crabfall sets the `content-type` with the boundary
- [x] Signs each file's SHA-256, not the boundary; `approve` and `run` show each file with its size and the step that uploads it
- [x] Tests: a server with axum's multipart parser gets the file intact (with CRLFs and something that looks like a boundary) and the text fields; a signature that's stable and sensitive to the content; validation
- Done: in `market-api`, `upload` (10 VUs for 1 minute, each uploading a 1 MB attachment, checking the `sha256` the server returns, downloading it and deleting it) did 137 iterations and 411 requests without failures, with 685 of 685 checks passed: the 137 files arrived intact. The upload had p50 9 ms and p95 134 ms; the download, p50 5 ms (19 Oct 2026).

**13. Bytes per second**
- [x] The socket's bytes per request (headers, body and TLS), with a per-request meter hooked to the connection; they count even if the request times out (ADR-013)
- [x] `bytes_sent` and `bytes_received` per step, per second in the series and in `totals`, with `send_rate` and `receive_rate`; `crabfall findings --json` includes them
- [x] `crabfall run` shows the total, the average and the per-second peak (also in Mbit/s); the report adds a tile, a per-second chart and each step's bytes per request
- [x] Tests: a 300 KB upload and a 256 KB download measured per step (the body plus a few hundred bytes of headers), the series adds up like the totals, and the bytes of a request that times out count
- Done: in `market-api`, `upload` (10 VUs for 1 minute, already as `crabfall`) moved 137.1 MB sent and 137.2 MB received, 2.1 MB/s each way. Per request, `upload_file` sent the file plus 559 bytes of headers and multipart, and `download_file` received the file plus 487. The peak, 10 MB/s (84 Mbit/s), was second 0, with the 10 VUs uploading at once. 685 of 685 checks passed (19 Oct 2026).

**14. A real HTTP client**
- [x] Cookies: one jar per VU (RFC 6265, with `cookie_store`) from `vu_setup` to the last iteration, also in `vu_refresh`; `setup` with its own; a custom `cookie` header is added; `cookies: false` turns them off (ADR-014)
- [x] Redirects: up to 10 per step (`redirects` on the request or in `defaults`), only to `allowed_hosts`; 303, and POST with 301/302, become GET; `authorization` and the custom cookie don't travel to another host; latency, timeout and bytes cover the chain
- [x] Compression: `accept-encoding: gzip, br` by default; checks and `extract` see the decompressed body (gzip, deflate, br; 64 MB cap) and the bytes are those on the wire
- [x] `form:` builds `application/x-www-form-urlencoded` bodies
- [x] `crabfall check` doesn't leave loopback, not even through a redirect: the client refuses to connect outside
- [x] Each VU has its own session even without `vu_setup` (before, they shared an empty one)
- [x] `approve` and `run` show an `http` line; `check` shows each hop of a redirect
- [x] CI on GitHub Actions: fmt and clippy on Linux, tests on Linux and Windows
- [x] Tests: against axum with `tower-http` really compressing (gzip and br), session per VU, cookies off, redirects (chain, limit, loop, POST 302 and 307, another host without `authorization`), check without leaving loopback, forms, signature and validation
- Done: in `market-api`, `browse` approved again (20 it/s for 30 s) gave 1,800 of 1,800 checks, without failures, with `search` at p50 10 ms as before. It received about 2 KB per request: Express doesn't compress, so asking for `gzip, br` changed nothing, as it should (19 Oct 2026). **Pending:** see compression behind Caddy (the `tls` profile, `encode gzip`), together with slice 1's TLS test.

**15. What the server says**
- [x] From each 429, `Retry-After` and the declared policy (`RateLimit-Policy` draft-8 and drafts 6-7, `X-RateLimit-Limit`) are stored in `steps[].rate_limit`; `error_threshold` states the limit with numbers; `crabfall run` shows it on a `limit` line (ADR-005)
- [x] `crabfall compare` compares bytes per request and warns if the response or the request changed size a lot (ADR-006)
- [x] Tests: the parser with express-rate-limit's real headers and the old formats, the rate-limit pathology with headers, and the size warning (also with runs without bytes)
- Done: in `market-api`, `rate-limit` (20 it/s against `/me` for 45 s) got 300 429s from second 30 (20/s × 30 s = 600). The finding says "declares 600 requests every 60 s (`RateLimit-Policy`) and asks to wait with `Retry-After from 16 to 30 s`", and `crabfall run` shows it on the `limit` line (19 Oct 2026). The run uncovered a formatting detail, already fixed: with little traffic the peak came out as "0.0 Mbit/s"; now it comes out in kbit/s.

**16. Language** (ADR-016)
- [x] English by default; `--lang` > `CRABFALL_LANG` > `language:` in config.yaml; without taking the system's language
- [x] All the output in both languages: terminal, validation, plan, findings, warnings, comparison, clap's help and the HTML report (with numbers and dates according to the language)
- [x] Each message with its two versions together (`tr!`, `pick`); the help is translated with a table; the report with `L(en, es)`
- [x] README, skill and schema documentation in English; ADRs and roadmap in Spanish (in English since 24 Oct 2026)
- [x] Tests: the existing ones in Spanish with `CRABFALL_LANG=es` in `.cargo/config.toml`; the English ones apart (flag > variable > config priority, help, unknown language, core messages)
- Done: in `market-api`, `register-users` without configuration came out in English; with `language: es` in the config, `smoke` came out in Spanish in `approve`, `run` and the report (22 Oct 2026).

**17. Stopping and watching live** (engine, for the TUI; ADR-015)
- [x] `Progress` carries a two-level stop request: the first stops starting iterations and waits for those in progress (`graceful_stop`); the second cuts now
- [x] Stopping before the load (in `setup` or `vu_setup`) aborts the run; during `vu_setup` it cuts the logins in progress
- [x] A stopped run saves `results.json` and the report with `stopped_at_s`, a warning at the top and `passed: false`; the diagnosis runs over what there was; `crabfall compare` warns that it covers less
- [x] Ctrl+C in `crabfall run`: the first stops in order, the second cuts
- [x] Live: phase (setup, vu_setup, load, draining, done), busy VUs, failed renewals and every closed second of the series (global and per step), published half a second after closing; the final series and the published one come from the same function
- [x] Tests: stopping in both models, twice (cuts what's in progress), during `vu_setup`, and the live series equal to the final one
- Done: in `market-api`, `browse --duration 2m` stopped with Ctrl+C at 20 s ended at 20.8 s (no long iterations to wait for), with 416 of 416 iterations and 1,248 checks passed, and saved results and report with the warning "Stopped by the person at second 20.8". The run uncovered two details, already fixed: the `no-2xx` column untranslated and two blank lines after the warning (20 Oct 2026).

**18. TUI** (ADR-015)
- [x] `crabfall` with no arguments opens the TUI if there's a terminal (otherwise, the help); the hook blocks it for Claude (`crabfall` without a subcommand, without blocking `--help` or `which crabfall`)
- [x] Home: scenarios with their description, the approval status in the chosen environment (also if approved with tweaks) and the last run; `e` changes the environment
- [x] Detail: `approve`'s risk summary and a tweaks form with the same parsers as the flags; the plan is rebuilt on apply and the error stays on the field; the equivalent command
- [x] Background check, approving by typing the host (same summary and diff) and running with `run::execute`, which saves results and report like `crabfall run`
- [x] Live view: phase and progress, last-second indicators, p95 and error sparklines, steps (last-second p50 and worst p95 of the last 5), warnings; `q` or Ctrl+C stop after confirming
- [x] Results in tabs (summary with the comparison against the previous run, findings, steps, checks); `o` opens the HTML, `r` repeats
- [x] The CLI was reorganized to share pieces: `prepare_in`, `approve::{summary, verify, sign}`, `run::execute`
- [x] Tests with `TestBackend`: list, valid and invalid tweaks, running without approval, wrong host, check, run screen; and two ignored tests that print the screens (one with a real run)
- Done: tested by the user in `market-api` (20 Oct 2026): "it's fantastic… good design". There are details to polish, to be defined with the user.

**19. History and comparison in the TUI** (ADR-015)
- [x] `h` (from the detail or the results) lists the scenario's runs in the environment, newest first: result, duration, requests, req/s, worst p95 and findings
- [x] Enter compares the chosen one with the previous; Space marks a baseline and Enter compares against it; `o` opens the chosen one's report
- [x] The comparison uses `compare::compare`, like `crabfall compare`: verdict, warnings, regressions, improvements, steps table and new, resolved and persisting findings
- [x] Tests: the history's order, the comparison with the previous (worse) and against a baseline (better), and a scenario without runs
- Done: in `market-api`, `search-ramp`'s history compared the run without the index (18 Oct) with the one after and said "better": `search` p95 3203 → 10.8 ms in 60 of 60 seconds, and the breaking point resolved (22 Oct 2026).

**20. Bandwidth diagnosis** (ADR-005)
- [x] `bandwidth_limit`: the traffic stalls at a ceiling (85% of the peak, 5 seconds or more) while the load asks for 25% more and the p95 triples; only above 1 MB/s
- [x] Evidence (peak, moved, asked for, bytes per iteration, p95) with readable names in the report, in both languages; the skill knows how to read it
- [x] Lab: a simulated 2 MB/s link gives the finding; the same responses without the limit don't
- Done when: against `market-api` behind a real link (the TLS test), `upload` with more VUs shows it if the link fills up. **Pending:** depends on the remote server.

**21. `pick: unique`** (ADR-007)
- [x] Each row only once in the whole run; the row is taken at dispatch, before the iteration counts
- [x] When they run out, the load ends in order with `data_exhausted` (dataset and second) and a warning; it doesn't count as a failure or as a stop by the person
- [x] In `vu_setup`, the plan requires one row per VU; `approve` warns if the rows don't cover the planned iterations
- [x] `--pick users=unique` by flag; schema, skill and README
- [x] Tests: both models use each row once and end when they run out; the `vu_setup` validation
- Done: in `market-api`, `register-users` (25 rows from `data/new-users.csv`, 1/s, 60 iterations asked) registered 25 users with 25 different emails in the database, 50 of 50 checks passed and all 201; the load ended on its own at second 25 with the warning and `data_exhausted` in `results.json` (22 Oct 2026).

**22. A fixed baseline run** (ADR-006)
- [x] `crabfall baseline [run]` stores it in `.crabfall/baselines/<scenario>.<environment>.json`, versioned; it doesn't accept an aborted run
- [x] `crabfall compare --baseline [run]` compares against it; without a baseline, it exits with 2 and says how to set one
- [x] In the TUI, `b` in the history sets the baseline and the row marks it
- [x] Help in both languages, README, skill (Claude prefers it to ask whether something got worse, but doesn't move it)
- [x] Tests: setting it, comparing against the baseline even with runs in between, without a baseline, extra arguments, and the `b` key
- Done: in `market-api`, `search-ramp`'s baseline was set with `b` in the history (`20261022-202949`, with the index, versioned in `market-api`). Against it, a run with the index gave "no change" (`search` p95 10.7 → 10.6 ms) and another one without the trigram indexes gave "worse": p95 10.7 → 3322 ms in 60 of 60 seconds, 8.1% of iterations dropped and a new breaking point at ~102 it/s (24 Oct 2026).

**23. Help in the TUI** (ADR-015)
- [x] `?` on any screen opens its help (what it is, how it's used, each key); any key closes it; while editing a value, `?` is typed
- [x] In the scenario, a line with the three steps (check → approve → run), which one is done and what's next, depending on the state: errors, invalid tweak, check pending or failed, changed since the approval, approved
- [x] Under the tweaks, what the selected one means and an example
- [x] In the list, the selected scenario's whole description (and the whole error if it can't be built), instead of a cut-off column
- [x] In the history, Enter chooses a run and Enter on another compares them; `c` compares with the previous; Esc releases the chosen one
- [x] The footer shows `? help` and `j/k`
- [x] A lighter gray for secondary text (the dark one was barely readable)
- [x] The footer wraps between keys when it doesn't fit the width (before, it was cut and `b` didn't show in the history); the history fits in 90 columns without cutting the id
- [x] At the end, the summary says "What now?": run again, change tweaks, back to the menu (`m`, from any screen), history, report
- Done when: someone opening the TUI for the first time gets to run and compare without being told. First test with the user (22 Oct 2026): they got to run, see the history, compare and set the baseline, but didn't find `b`: the history's footer didn't fit the width and was cut right at "· b". Fixed: the footer wraps between keys to fit the width, and the history's columns no longer cut the run id (`20261022-2029`). **Pending:** the test with someone who doesn't know it.

**24. The agent asks for the run, the person confirms in a window** (ADR-017)
- [x] `crabfall run --ask`: without a terminal, it relaunches in a new console, waits and returns the code and where the results are; in a terminal it asks there
- [x] The window shows the risk summary; without a valid approval it asks for the host and signs, with an approval `y` is enough; the load runs in the window and Ctrl+C stops it; it closes on its own after 30 s
- [x] A time-limited allowance (1 to 120 min) signed with the same key and tied to the plan's hash; revoked by deleting the file
- [x] 5 min without an answer cancels; closing the window too
- [x] `--window` refuses if the console isn't new and visible (the only process in it)
- [x] Hook: lets `run --ask` through, blocks `run` without `--ask` and `run --window`; skill, help in both languages and README
- [x] Tests: the allowance (expires, doesn't survive another plan, an edit or another key), the hook and `--window` outside its window
- [x] Research on malicious agents: what holds and what doesn't, in ADR-017
- Done: in `market-api`, Claude ran `search-ramp --ask`; the user typed the host in the window (the plan had changed because of `X-Lab-Key`) and gave a few minutes of allowance. Claude compared against the baseline ("no change"), dropped the trigram indexes and asked for the run again: it ran without asking within the allowance, because the plan was the same, and gave "worse". Then the indexes were restored (24 Oct 2026). The test uncovered a detail, already fixed: after typing the host, the minutes question looked like a second step to approve; now it says it's optional and that Enter runs the load. Linux in slice 25; macOS, later.

**25. The `--ask` window on Linux** (ADR-017)
- [x] With a desktop (`DISPLAY` or `WAYLAND_DISPLAY`), the window opens in `$TERMINAL` or the first known terminal (`x-terminal-emulator`, gnome-terminal, konsole, kitty, xterm…), each with its own way of receiving the command; without a desktop it refuses as before
- [x] The agent doesn't wait for the terminal (gnome-terminal returns at once): the window leaves its pid next to the answer, and the agent waits for the answer or for that process to stop existing; the answer is written aside and renamed
- [x] `--window` on Linux: input and output on a terminal, leader of its own session and with a known terminal as parent (not `script`, `tmux` or a shell)
- [x] Tests: each terminal's arguments, `/proc/<pid>/stat` with odd names, which parents are valid; CI already runs on Linux
- Done: in WSL2 (Ubuntu 24.04 with WSLg and GNOME Keyring), `smoke --ask` against `market-api` opened the window in gnome-terminal (through `x-terminal-emulator`); the user typed the host, gave a few minutes of allowance, and the next run in xterm went ahead without asking. With the allowance revoked, it asked again; closing the xterm without answering returned code 3 and "the window closed without an answer", with no new run (24 Oct 2026). In WSLg the terminals look gray on black: those are their default colors.

## Versions

**v0.1.0** (19 Oct 2026): slices 0 to 13 under the name Crabfall (`crabfall` binary, `.crabfall/` folder), on `main` and on GitHub (private at the time). Slice 13 was closed afterwards, with the `upload` run.

**v0.2.0** (24 Oct 2026): slices 14 to 25. A real HTTP client (cookies, redirects, compression, forms), what the server says in a 429, English and Spanish, stopping and watching live, the TUI with history, comparison and help, the bandwidth diagnosis, `pick: unique`, the fixed baseline run, and the agent asking for the run with `--ask` while a person confirms it in a window, on Windows and Linux.

**Still open** (real tests; the code is done):
- Against a remote server (`market-api` on an hourly VPS; Vultr in Santiago or São Paulo chosen, with `sslip.io` instead of a domain; on hold): real TLS (1), compression behind Caddy (14) and the bandwidth diagnosis over a real link (20).
- The skill in a fresh Claude Code session, which loads it on its own (2).
- The TUI with someone who doesn't know it (23), for later.

## Next version (v0.3)

- **MCP server.**
- **Importers:** OpenAPI, curl, Postman.
- **Diagnosis rules:** the [comparative study](studies/diagnosis-comparison.md) (k6, Gatling, NeoLoad, LoadRunner, Azure, Locust, JMeter, Artillery, Dynatrace) is done (24 Oct 2026), and its recommendations were accepted: deterministic rules; saturation with fixed VUs and server vs payload first, contagion between steps if there's time, and best practices as `validate` warnings; rules can be turned off and a few named margins tuned, recorded in the results; no custom rules. To be written into an ADR, with its lab.

## Backlog (unordered)

**Last: new load models** (`ramping_vus`, fixed iterations, per-VU arrival). `constant_rate`, `ramping_rate` and `constant_vus` are enough today.

- The `--ask` window on macOS (Terminal through `osascript`); there's nowhere to test it today.
- Defenses against a malicious agent (ADR-017): a guide to run the agent with Claude Code's sandbox and restricted egress, and a short-lived token required by the target (FIDO2 or a phone, verified off the machine).
- Approving in CI without a person, so the fixed baseline works in a pipeline, and a GitHub Action.
- Correlation with server metrics (OTLP, Prometheus); see D4.
- HTTP/2, WebSockets, gRPC, Kafka.
- Reading `expires_in` to choose when to renew on its own.
- A Tauri GUI (after the TUI).
- Run phases (NeoLoad style) and the trend across many runs (diagnosis study, points 5 and 6).

## Open decisions

| # | Question | Current recommendation |
| --- | --- | --- |
| D4 | Does the diagnosis use server metrics? | Not for now: client side only |

## Decision log

| Date | Decision | Where |
| --- | --- | --- |
| 7 Oct 2026 | Claude designs and analyzes; it never triggers load | Principle 1 |
| 7 Oct 2026 | `hyper` directly instead of `reqwest` (confirmed with the spike on 12 Oct) | ADR-003 |
| 7 Oct 2026 | Two-layer diagnosis: rules in the engine, hypotheses in Claude | Principle 3 |
| 10 Oct 2026 | Claude can run smoke checks only against loopback | ADR-004 |
| 10 Oct 2026 | A declarative YAML scenario format, derived approval | ADR-002 |
| 12 Oct 2026 | Claude-first: a CLI with JSON + a Claude Code skill and hook (`crabfall claude install`); MCP and importers later | ADR-001 |
| 12 Oct 2026 | Work in vertical slices, without dates | This document |
| 12 Oct 2026 | Rust MSVC toolchain on Windows | — |
| 12 Oct 2026 | Open-model scheduler: latency from the planned instant, iterations dropped when there's no VU | ADR-003 |
| 12 Oct 2026 | On Windows, the timer resolution is raised to 1 ms | ADR-003 |
| 12 Oct 2026 | YAML with `serde-saphyr` + `serde_path_to_error` (D5) | ADR-002 §10 |
| 12 Oct 2026 | The approval signs the resolved canonical plan; `approve` always requires a terminal; `run` only verifies the signature; no expiry | ADR-004 |
| 12 Oct 2026 | Each piece of work on its own branch, `--no-ff` merge to `dev`; `main` stays untouched until decided | — |
| 12 Oct 2026 | Checks and thresholds aren't signed (they don't change the traffic); `setup`, `extract` and variables are | ADR-004 |
| 12 Oct 2026 | `ramping_rate` with an exact schedule; `constant_vus` with a pacer if there's `max_rps` | ADR-003 |
| 12 Oct 2026 | Two-layer diagnosis: deterministic rules with evidence in the engine, hypotheses about the code in Claude | ADR-005 |
| 15 Oct 2026 | `new_connections` discounts one connection per VU (the pool grows up to the concurrency) | ADR-005 |
| 15 Oct 2026 | `crabfall compare`: a per-step verdict with magnitude + a sign test over paired seconds; regressions exit with code 1 | ADR-006 |
| 16 Oct 2026 | Datasets: `{{dataset.column}}`, one row per iteration, CSV as strings and JSON with types; each file's SHA-256 is signed | ADR-007 |
| 17 Oct 2026 | Think time after each step; every tweak of a run goes in as a flag and is signed | ADR-008 |
| 17 Oct 2026 | A self-contained HTML report per run, generated by `crabfall run` | ADR-009 |
| 17 Oct 2026 | `vu_setup`: each VU logs in before the load; the open model hands out VUs from a pool | ADR-010 |
| 18 Oct 2026 | `vu_refresh`: renewal in the background, without taking the VU out of the pool; if it fails, the VU stops iterating | ADR-011 |
| 18 Oct 2026 | `multipart`: files read once and signed by content; no streaming or paths with variables | ADR-012 |
| 19 Oct 2026 | Bytes measured on the socket per request (headers, body and TLS), not on the body; only the load | ADR-013 |
| 19 Oct 2026 | Name: Crabfall; `crabfall` binary, `.crabfall/` folder, `crabfall-core` and `crabfall-cli` crates (D2) | — |
| 19 Oct 2026 | A TUI in the terminal instead of the Tauri GUI, for now; new load models go last | This document |
| 19 Oct 2026 | First version v0.1.0: `dev` is merged into `main`; private repo on GitHub | — |
| 19 Oct 2026 | A real HTTP client by default: cookies per VU, redirects to `allowed_hosts`, gzip and br; `check` never leaves loopback | ADR-014 |
| 19 Oct 2026 | TUI: `crabfall` with no arguments; approves with the typed host; stops in order and saves; the HTML stays for analyzing and sharing | ADR-015 |
| 19 Oct 2026 | Output in English by default and Spanish on request; README and skill in English, ADRs and roadmap in Spanish | ADR-016 |
| 23 Oct 2026 | The agent can run load with `run --ask`: a person confirms each run in a separate window, where the load runs; a signed time-limited allowance; the threat model is still ADR-004's | ADR-017 |
| 24 Oct 2026 | The default `user-agent` is signed without the version: upgrading Crabfall doesn't invalidate approvals | ADR-004 |
| 24 Oct 2026 | v0.2.0: slices 14 to 25 to `main`; for v0.3, MCP, importers and diagnosis rules after comparing them with other tools | Versions |
| 24 Oct 2026 | Diagnosis for v0.3: deterministic rules; they can be turned off and a few margins tuned, recorded in the results; no custom rules | [Study](studies/diagnosis-comparison.md) |
| 24 Oct 2026 | All the documentation in English (ADRs, roadmap, studies); the test repos (`market-api`, `shop-api`) public on GitHub; MIT OR Apache-2.0 license files | ADR-016 |
