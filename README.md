# Crabfall

A load testing tool in Rust, built to be used together with Claude Code:
Claude designs the scenario and analyzes the results; a person approves and runs the load.

Status: **slice 23** (help in the TUI), after v0.1.0. Already working: design with Claude, signed approval, scenarios with `setup`, `extract`, checks and thresholds, three executors, diagnosis, run comparison, datasets, pauses between steps, per-run tweaks by flag, an HTML report per run, login per virtual user (`vu_setup`), token renewal during the load (`vu_refresh`), file uploads (`multipart`), bytes sent and received per second, an HTTP client with cookies, redirects, compression and forms, and output in English or Spanish. See the [roadmap](docs/roadmap.md) (in Spanish).

## Usage

```bash
cargo install --path crates/crabfall-cli    # installs `crabfall` in ~/.cargo/bin
```

In the repo under test, create `.crabfall/config.yaml`:

```yaml
version: 1
default_environment: local
environments:
  local:
    base_url: http://localhost:3000
    allowed_hosts: [localhost]
    limits: { max_rps: 200 }      # optional: requests/s, max_vus, max_duration
```

and a scenario in `.crabfall/scenarios/smoke.yaml`:

```yaml
version: 1
name: smoke
flows:
  main:
    steps:
      - name: list_products
        request: { method: GET, url: /products }
load:
  executor: constant_rate
  rate: 50/s
  duration: 30s
  max_vus: 50
```

```bash
crabfall validate smoke              # --json for agents
crabfall check smoke                 # each flow once, only against loopback
crabfall approve smoke --env local   # risk summary; asks you to type the host
crabfall run smoke --env local       # results in .crabfall/runs/<id>/results.json
```

### Terminal UI

Run `crabfall` without a subcommand, in a terminal, to open the TUI ([ADR-015](docs/adr/ADR-015-tui.md)):

- **Pick** a scenario and the environment (`e`), with each scenario's approval state, its last run and its full description.
- **Tweak** rate, duration, VUs, pauses, renewal and thresholds; the plan is rebuilt as you type, and it shows the equivalent `crabfall` command.
- **Check** (`c`), **approve** (`a`, typing the host) and **run** (`r`). A line at the top shows these three steps and what to do next; below the tweaks, what the selected one means, with an example.
- **Watch it live**: the last second against what you asked for, p95 and errors per second, the steps, and warnings as they happen. `q` stops it, the same way Ctrl+C does.
- **Results** in tabs, and `o` opens the HTML report.
- **History** (`h`): the scenario's runs, newest first. Enter chooses a run and Enter on another compares them; `c` compares the selected one with the previous run; `b` sets it as the baseline.
- **Help**: `?` on any screen explains it; the bottom line always shows its keys.

The HTML report is still the place to analyze and share a run; the TUI is for operating it.

To stop a run, press Ctrl+C: it stops starting iterations, waits for the ones in progress and saves what ran, marked as stopped. A second Ctrl+C cuts them right away.

`crabfall run` only runs approved scenarios. The approval signs the scenario together with the resolved environment: any change in the traffic invalidates it, and `crabfall run` shows what changed. Approval happens in an interactive terminal; the signing key lives in the system keyring ([ADR-004](docs/adr/ADR-004-aprobacion.md)).

### Language

Crabfall prints in English by default. To use Spanish, any of these works, from highest to lowest priority ([ADR-016](docs/adr/ADR-016-idioma.md)):

```bash
crabfall run smoke --env local --lang es    # one command
export CRABFALL_LANG=es                     # this shell
```

```yaml
# .crabfall/config.yaml: this project
language: es
```

Everything follows it: the terminal output, validation errors, findings, the help and the HTML report. The scenario keys (`flows`, `load`, `checks`…) are always in English.

### Data per iteration

For each iteration to use different data (users, IDs), declare CSV or JSON files in `data:` and use them as `{{dataset.column}}`:

```yaml
data:
  users: { file: data/users.csv }    # .crabfall/data/users.csv, with header user,password
flows:
  main:
    steps:
      - name: login
        request:
          method: POST
          url: /auth/login
          json: { user: "{{users.user}}", password: "{{users.password}}" }
        think: 1s-3s                  # pause after the step, like a person
```

Each iteration takes a row, in order, at random (`pick: random`) or each row only once (`pick: unique`, for example to register users; when the rows run out, the load ends there). Editing the file invalidates the approval ([ADR-007](docs/adr/ADR-007-datasets.md)). `think` also goes in `defaults` for every step; it does not count in the latency and `crabfall check` ignores it.

### Sessions

For APIs with sessions, `vu_setup` runs once per virtual user, before the load: each VU logs in with its dataset row and uses its token in all its iterations, without measuring the login in each one:

```yaml
vu_setup:
  - name: login
    request: { method: POST, url: /auth/login, json: { user: "{{users.user}}", password: "{{users.password}}" } }
    extract: { token: { json: $.access_token } }
defaults:
  headers: { Authorization: "Bearer {{token}}" }
```

One VU is prepared per `vus` (or `max_vus`); the `crabfall approve` summary shows how many logins that is ([ADR-010](docs/adr/ADR-010-setup-por-vu.md)).

If the token expires during the load, `vu_refresh` renews it in the background, without cutting iterations or adding to their metrics. With a refresh token:

```yaml
vu_refresh:
  every: 4m             # less than the token lifetime
  steps:
    - name: refresh
      request: { method: POST, url: /auth/refresh, json: { refresh_token: "{{refresh_token}}" } }
      extract: { token: { json: $.access_token }, refresh_token: { json: $.refresh_token } }
```

Without `steps`, it repeats `vu_setup` (logs in again). `--refresh-every 4m` sets it by flag. A VU that cannot renew stops iterating ([ADR-011](docs/adr/ADR-011-renovacion-de-sesion.md)).

### Uploads and traffic

To upload files, `multipart` builds a `multipart/form-data` body with files from `.crabfall/` and text fields:

```yaml
request:
  method: POST
  url: /files
  multipart:
    file: { file: files/attachment.bin, filename: "attachment-{{$uuid}}.bin" }
    description: "uploaded by {{users.email}}"
```

The file is read once and sent whole in each request (up to 100 MB). Its type comes from the extension, or from `content_type`. The approval signs its content ([ADR-012](docs/adr/ADR-012-multipart.md)).

With large files, watch the traffic: `crabfall run` shows the bytes sent and received on the socket (headers, body and TLS), with the average and the peak per second, and the report charts them ([ADR-013](docs/adr/ADR-013-trafico.md)):

```
traffic      sent 137.4 MB (2.1 MB/s, peak 4.3 MB/s = 36 Mbit/s)  received 137.6 MB (2.1 MB/s, peak 4.1 MB/s = 34 Mbit/s)
```

### Cookies, redirects and compression

Crabfall behaves like a real client ([ADR-014](docs/adr/ADR-014-cliente-http-real.md)):

- **Cookies:** each VU keeps the cookies the server sends and sends them back, from its `vu_setup` to its last iteration. An API with cookie sessions works without `extract`. `cookies: false` at the root turns them off.
- **Redirects:** each step follows up to 10, only to hosts in `allowed_hosts`; the latency includes every hop. `redirects: 0` in the request (or in `defaults`) follows none.
- **Compression:** each request asks for `gzip, br`; checks and `extract` see the decompressed body and the bytes count the compressed one. `accept-encoding: identity` in the headers asks for no compression.

For a classic form, `form` builds an `application/x-www-form-urlencoded` body:

```yaml
request:
  method: POST
  url: /login
  form: { username: "{{users.email}}", password: "{{users.password}}", remember: true }
```

`crabfall approve` shows how the client will behave in an `HTTP` line. When a server answers 429, Crabfall reads its `Retry-After` and declared policy, and the finding states the limit with numbers.

### Tweaks without editing the YAML

`validate`, `check`, `approve` and `run` accept flags that override the scenario for that run:

```bash
crabfall run checkout --env local --rate 100/s --duration 2m --think 1s-3s
crabfall run checkout --executor constant_vus --vus 20 --duration 5m
crabfall run checkout --executor ramping_rate --stages 50/s:1m,200/s:3m,0/s:30s
crabfall run login-users --data users=data/others.csv --pick users=random
crabfall run shop-per-user --duration 10m --refresh-every 4m
crabfall run checkout --threshold "http.latency.p95<300ms" --threshold "http.error_rate<1%"
```

Full list in `crabfall run --help`. The approval signs the ones that change the traffic: pass the same ones to `crabfall approve` and `crabfall run`. `--threshold` replaces the YAML ones and needs no new approval ([ADR-008](docs/adr/ADR-008-think-time-y-ajustes.md)).

## With Claude Code

```bash
crabfall claude install              # in the repo under test: skill, hook and permissions in .claude/
```

Then, in a Claude Code session: *"build me a load test of the checkout flow"*. Claude reads the code, writes the scenario, checks it with `crabfall validate` and `crabfall check` (only against loopback) and hands you the `crabfall approve` and `crabfall run` commands to run yourself. It cannot approve or run on its own ([ADR-001](docs/adr/ADR-001-claude-first.md), ADR-004).

`crabfall findings [--json]` shows the findings of the last run (or of one by id). `crabfall compare [base] [new] [--json]` compares two runs (by default, the last one with the previous one of the same scenario): what improved, what got worse and whether the change is consistent or may be noise ([ADR-006](docs/adr/ADR-006-comparacion.md)). To compare against an accepted reference instead of the previous run, set a baseline with `crabfall baseline [run]` (it goes in `.crabfall/baselines/`; commit it to share it) and use `crabfall compare --baseline`. In the TUI history, `b` sets it. `crabfall schema` prints the JSON Schema of the scenario.

Each `crabfall run` also leaves `report.html` next to `results.json`: a file that opens offline, with per-second charts (iterations, latency, p95 per step, traffic), the findings with their window shaded, the comparison with the previous run and the tables. `crabfall report [run] --open` regenerates it and opens it in the browser ([ADR-009](docs/adr/ADR-009-reporte-html.md)).

Exit codes: `0` ok · `1` `check` with failed steps, thresholds not met, `setup` failed or `compare` with regressions · `2` invalid scenario or configuration · `3` refused (no valid approval, host not allowed, limits, no terminal) · `4` error saving or accessing the keyring.

Full scenario format: [ADR-002](docs/adr/ADR-002-formato-escenario.md).

## Development

```bash
cargo test --workspace
cargo run --release -p lab-server -- --port 3999   # lab server
```

The tests check the Spanish messages, so `.cargo/config.toml` sets `CRABFALL_LANG=es` for processes started by cargo in this workspace; English has its own tests (`tests/english.rs` in both crates). An installed `crabfall` still defaults to English.

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check` and `clippy` on Linux, and the tests on Linux and Windows, on every push to `main` or `dev` and on every pull request. Warnings count as errors. On Windows the diagnosis pathologies are skipped (`CRABFALL_SKIP_TIMING_TESTS=1`): they measure real time and that 2-core runner cannot keep the load schedule.

To test against more realistic APIs: [shop-api](../shop-api) (local, with planted performance problems) and [market-api](../market-api) (Postgres, JWT, rate limit and uploads), separate repos with scenarios in their `.crabfall/`.

## Documentation

The architecture decision records and the roadmap are in Spanish.

- [Roadmap](docs/roadmap.md)
- [ADR-001 — Claude-first](docs/adr/ADR-001-claude-first.md)
- [ADR-002 — Scenario format](docs/adr/ADR-002-formato-escenario.md)
- [ADR-003 — hyper and the scheduler](docs/adr/ADR-003-hyper-y-scheduler.md)
- [ADR-004 — Approval and threat model](docs/adr/ADR-004-aprobacion.md)
- [ADR-005 — Diagnosis](docs/adr/ADR-005-diagnostico.md)
- [ADR-006 — Comparing runs](docs/adr/ADR-006-comparacion.md)
- [ADR-007 — Datasets](docs/adr/ADR-007-datasets.md)
- [ADR-008 — Think time and tweaks by flag](docs/adr/ADR-008-think-time-y-ajustes.md)
- [ADR-009 — HTML report](docs/adr/ADR-009-reporte-html.md)
- [ADR-010 — Setup per VU](docs/adr/ADR-010-setup-por-vu.md)
- [ADR-011 — Session renewal](docs/adr/ADR-011-renovacion-de-sesion.md)
- [ADR-012 — File uploads (multipart)](docs/adr/ADR-012-multipart.md)
- [ADR-013 — Bytes sent and received](docs/adr/ADR-013-trafico.md)
- [ADR-014 — Cookies, redirects, compression and forms](docs/adr/ADR-014-cliente-http-real.md)
- [ADR-015 — Terminal TUI](docs/adr/ADR-015-tui.md)
- [ADR-016 — Output language](docs/adr/ADR-016-idioma.md)
