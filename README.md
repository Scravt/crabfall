# Crabfall

A load testing tool in Rust, built to be used together with Claude Code:
Claude designs the scenario and analyzes the results; a person approves and runs the load.

Status: **v0.2.0** (slices 0 to 25), working on v0.3: slices 26 (where a slow step's time goes), 27 (saturation with fixed VUs, by comparing runs) and 28 (turning diagnosis rules off and tuning margins) are done ([ADR-018](docs/adr/ADR-018-diagnosis-v03.md)); see [What's next](#whats-next). Already working: design with Claude, signed approval (HMAC-SHA256 with the key in the system keyring; see [Approval](#approval-no-load-without-a-person)), runs requested by the agent and confirmed by a person in a window ([`--ask`](#letting-the-agent-ask-for-a-run---ask)), scenarios with `setup`, `extract`, checks and thresholds, three executors, diagnosis, run comparison, datasets, pauses between steps, per-run tweaks by flag, an HTML report per run, login per virtual user (`vu_setup`), token renewal during the load (`vu_refresh`), file uploads (`multipart`), bytes sent and received per second, an HTTP client with cookies, redirects, compression and forms, and output in English or Spanish. See the [roadmap](docs/roadmap.md).

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
crabfall run smoke --env local --ask # asks first: what Claude uses, in a window
```

### Approval: no load without a person

`crabfall run` only runs a scenario that a person approved, exactly as it is now. The guard lives in the `crabfall` binary, not in Claude Code's permissions, because Claude can edit any file in the repo ([ADR-004](docs/adr/ADR-004-approval.md)).

1. **What gets signed: the canonical plan.** `crabfall approve` resolves the scenario against the environment and the flags, and builds a canonical JSON of the traffic it will send:
   - each request with its URL already resolved against `base_url`, method, headers, body and timeout; the flow weights; `setup`, `extract`, `vu_setup` and `vu_refresh`;
   - the resolved load (executor, rate, duration, VUs, stages, pauses), including every tweak by flag;
   - the environment's `base_url`, `allowed_hosts`, `vars` and `limits`;
   - the SHA-256 of every dataset and every uploaded file (their content, not just their path);
   - the `user-agent`; the default one is signed as `crabfall/<version>`, so upgrading Crabfall keeps your approvals.

   Secret values never enter, only their names. `description`, `checks`, `thresholds` and `source` are left out: they don't change the traffic. The SHA-256 of that JSON is the plan's **fingerprint**.
2. **The key.** The first approval generates 32 random bytes and stores them in the system keyring (Windows Credential Manager, macOS Keychain, Secret Service on Linux) as `crabfall` / `approval-key`. The key is never written in the repo.
3. **The signature.** HMAC-SHA256 with that key over the format version, scenario, environment, fingerprint, date and approver. It goes in `.crabfall/approvals/<scenario>.<env>.json`, together with the approved plan (used for the diff). Approvals are not committed: the key belongs to one machine.
4. **The person.** `crabfall approve` refuses unless stdin and stderr are an interactive terminal. It shows the risk summary (hosts, rate, VUs, duration, steps that write, data, the diff against the last approval), refuses plans over the environment's `limits`, and asks you to **type the target host**: a `y` is not enough.
5. **The verification.** Every `crabfall run` recomputes the HMAC with the key from the keyring, checks that the stored plan matches its fingerprint, rebuilds the plan from what is on disk now and compares fingerprints. If anything differs, it refuses with exit code `3` and shows the diff between what was approved and what there is now.

What that stops:

| Attempt | Result |
| --- | --- |
| Editing the scenario after approving it (rate, a URL, a body) | The fingerprint changes: refused, with the diff |
| Leaving the scenario alone and pointing the environment elsewhere (`base_url`, `allowed_hosts`, `limits`, `vars`) | The environment is in the fingerprint: refused |
| Changing a dataset row or the uploaded file | Their SHA-256 is in the fingerprint: refused |
| Running with other flags (`--rate 500/s` approved as `--rate 50/s`) | The tweaks are in the fingerprint: refused |
| Writing or editing an approval file by hand, or copying another scenario's | Without the key there is no valid HMAC; scenario and environment are signed |
| `yes \| crabfall approve`, `echo host \| crabfall approve` | No interactive terminal: refused |
| Claude running `crabfall approve` or `crabfall run` | The `PreToolUse` hook from `crabfall claude install` blocks it first (an extra layer, not the guard); only `crabfall run --ask` gets through, and a person confirms it in a window |

Updating Crabfall invalidates the approvals too, since the `user-agent` changes: the traffic is different. Approvals do not expire; they last until something changes.

What it does **not** stop: someone acting deliberately with your user account. Any process running as you can read the keyring key, emulate a terminal, or send load without Crabfall (a `curl` loop). `approved_by` is a signed label, not an authentication. The guard prevents accidents and misuse of this tool; it does not replace trust in the agent or the security of the machine. Without a person or a keyring, approving in CI is not supported yet.

### Letting the agent ask for a run: `--ask`

So that Claude can iterate (run, compare against the baseline, change the API's code, run again) without you typing each command, it runs `crabfall run <name> --env <env> --ask` ([ADR-017](docs/adr/ADR-017-window-confirmation.md)):

1. Crabfall builds and checks the plan and the `limits` first: errors go back to Claude without bothering you.
2. It opens a **new console window** with the risk summary. If the plan has no valid approval (first time, or it changed), it shows the diff and asks you to type the host, as `crabfall approve` does. If it is approved, answering `y` is enough.
3. Instead of `y`, you can answer a number of **minutes (1-120)**: the same plan runs without asking for that long. After typing the host, the same question comes as an optional step (Enter just runs it). The allowance is signed with the same key, is tied to the plan's fingerprint (any change to the scenario asks again; a change on the server, like dropping an index, doesn't) and is revoked by deleting `.crabfall/approvals/<scenario>.<env>.allow.json`.
4. **The load runs in that window**, with its progress: Ctrl+C stops it, whatever Claude does. At the end it shows the summary and closes after 30 s (or Enter).
5. Claude's command prints where the results are and exits with the run's code; Claude reads them with `crabfall findings --json` and `crabfall compare --baseline --json`.

Nobody answering in 5 minutes cancels it (exit code `3`), and so does closing the window. `--window`, the flag the window uses internally, refuses unless its console is new and visible, so it cannot confirm anything from Claude's console or a shared terminal. In a terminal, `--ask` asks right there.

The window opens on Windows (a new console) and on Linux with a desktop (`DISPLAY` or `WAYLAND_DISPLAY`; WSL2 with WSLg works). On Linux it uses `$TERMINAL` if set, or the first of `x-terminal-emulator`, gnome-terminal, ptyxis, konsole, xfce4-terminal, mate-terminal, kitty, alacritty, wezterm, foot and xterm; there `--window` checks that it runs straight under a known terminal, in a session of its own. On Linux the approval key lives in the Secret Service (GNOME Keyring or KWallet), which has to be running and unlocked. On macOS, and without a desktop (SSH, Claude Code on the web), `--ask` refuses and asks you to run the command.

Against a deliberately malicious agent this is no stronger than the rest: same-user processes can type into that window (`SendInput`), read the keyring key or send load without Crabfall. What would resist is keeping the agent away from the target and the key (Claude Code's sandbox with a network allowlist, a container or a VM), or making the target require a short-lived token issued off the agent's machine; ADR-017 compares the options.

The only thing that needs no approval is `crabfall check`: each flow once, in series, at most 50 requests, and only to hosts that resolve to loopback, redirects included.

### Terminal UI

Run `crabfall` without a subcommand, in a terminal, to open the TUI ([ADR-015](docs/adr/ADR-015-tui.md)):

- **Pick** a scenario and the environment (`e`), with each scenario's approval state, its last run and its full description.
- **Tweak** rate, duration, VUs, pauses, renewal and thresholds; the plan is rebuilt as you type, and it shows the equivalent `crabfall` command.
- **Check** (`c`), **approve** (`a`, typing the host) and **run** (`r`). A line at the top shows these three steps and what to do next; below the tweaks, what the selected one means, with an example.
- **Watch it live**: the last second against what you asked for, p95 and errors per second, the steps, and warnings as they happen. `q` stops it, the same way Ctrl+C does.
- **Results** in tabs, and `o` opens the HTML report.
- **History** (`h`): the scenario's runs, newest first. Enter chooses a run and Enter on another compares them; `c` compares the selected one with the previous run; `b` sets it as the baseline.
- **Help**: `?` on any screen explains it; the bottom lines always show its keys, wrapped to fit a narrow terminal. After a run, the summary lists what to do next; `m` goes back to the menu from any screen.

The HTML report is still the place to analyze and share a run; the TUI is for operating it.

To stop a run, press Ctrl+C: it stops starting iterations, waits for the ones in progress and saves what ran, marked as stopped. A second Ctrl+C cuts them right away.

### Language

Crabfall prints in English by default. To use Spanish, any of these works, from highest to lowest priority ([ADR-016](docs/adr/ADR-016-language.md)):

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

One VU is prepared per `vus` (or `max_vus`); the `crabfall approve` summary shows how many logins that is ([ADR-010](docs/adr/ADR-010-per-vu-setup.md)).

If the token expires during the load, `vu_refresh` renews it in the background, without cutting iterations or adding to their metrics. With a refresh token:

```yaml
vu_refresh:
  every: 4m             # less than the token lifetime
  steps:
    - name: refresh
      request: { method: POST, url: /auth/refresh, json: { refresh_token: "{{refresh_token}}" } }
      extract: { token: { json: $.access_token }, refresh_token: { json: $.refresh_token } }
```

Without `steps`, it repeats `vu_setup` (logs in again). `--refresh-every 4m` sets it by flag. A VU that cannot renew stops iterating ([ADR-011](docs/adr/ADR-011-session-renewal.md)).

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

With large files, watch the traffic: `crabfall run` shows the bytes sent and received on the socket (headers, body and TLS), with the average and the peak per second, and the report charts them ([ADR-013](docs/adr/ADR-013-traffic.md)):

```
traffic      sent 137.4 MB (2.1 MB/s, peak 4.3 MB/s = 36 Mbit/s)  received 137.6 MB (2.1 MB/s, peak 4.1 MB/s = 34 Mbit/s)
```

### Cookies, redirects and compression

Crabfall behaves like a real client ([ADR-014](docs/adr/ADR-014-real-http-client.md)):

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
crabfall run checkout --diagnosis-off long_tail --latency-margin 100ms --min-requests 50
```

Full list in `crabfall run --help`. The approval signs the ones that change the traffic: pass the same ones to `crabfall approve` and `crabfall run`. `--threshold` replaces the YAML ones and needs no new approval ([ADR-008](docs/adr/ADR-008-think-time-and-tweaks.md)). Neither do the diagnosis settings: `--diagnosis-off` turns rules off by finding id, `--latency-margin` replaces the 50 ms the rules add to "3× the baseline", and `--min-requests` lowers how many requests a step needs. They also go in a `diagnosis:` block in the scenario or in an environment of `config.yaml`; they are recorded in `results.json` and the report, and `compare` warns when two runs used different ones ([ADR-018](docs/adr/ADR-018-diagnosis-v03.md)).

## With Claude Code

```bash
crabfall claude install              # in the repo under test: skill, hook and permissions in .claude/
```

Then, in a Claude Code session: *"build me a load test of the checkout flow"*. Claude reads the code, writes the scenario, checks it with `crabfall validate` and `crabfall check` (only against loopback) and hands you the `crabfall approve` and `crabfall run` commands to run yourself. It cannot approve or run on its own ([ADR-001](docs/adr/ADR-001-claude-first.md), ADR-004): when it wants to run, it uses `crabfall run --ask`, and you confirm each run in a window (or allow the same plan for some minutes) while it compares the results and keeps working on the code ([`--ask`](#letting-the-agent-ask-for-a-run---ask)).

`crabfall findings [--json]` shows the findings of the last run (or of one by id): the breaking point, capacity exceeded, errors from a certain rate, degradation over time, a long tail, connections not reused, a bandwidth ceiling, and, for a slow step, whether its time goes to the server, to downloading the body or to getting a connection ([ADR-005](docs/adr/ADR-005-diagnosis.md), [ADR-018](docs/adr/ADR-018-diagnosis-v03.md)). `crabfall compare [base] [new] [--json]` compares two runs (by default, the last one with the previous one of the same scenario): what improved, what got worse and whether the change is consistent or may be noise ([ADR-006](docs/adr/ADR-006-comparison.md)). Two runs with fixed VUs that differ only in how many say whether the throughput followed the VUs or the server is saturated ([ADR-018](docs/adr/ADR-018-diagnosis-v03.md)). To compare against an accepted reference instead of the previous run, set a baseline with `crabfall baseline [run]` (it goes in `.crabfall/baselines/`; commit it to share it) and use `crabfall compare --baseline`. In the TUI history, `b` sets it. `crabfall schema` prints the JSON Schema of the scenario.

Each `crabfall run` also leaves `report.html` next to `results.json`: a file that opens offline, with per-second charts (iterations, latency, p95 per step, traffic), the findings with their window shaded, the comparison with the previous run and the tables. `crabfall report [run] --open` regenerates it and opens it in the browser ([ADR-009](docs/adr/ADR-009-html-report.md)).

Exit codes: `0` ok · `1` `check` with failed steps, thresholds not met, `setup` failed or `compare` with regressions · `2` invalid scenario or configuration · `3` refused (no valid approval, host not allowed, limits, no terminal, or nobody confirmed `--ask`) · `4` error saving or accessing the keyring.

Full scenario format: [ADR-002](docs/adr/ADR-002-scenario-format.md).

## What's next

Still to try for real (the code is done): TLS, compression behind Caddy and the bandwidth diagnosis against a remote server; the skill in a fresh Claude Code session; the TUI with someone who has never seen it.

**v0.3:**
- An MCP server.
- Importers: OpenAPI, curl, Postman.
- Diagnosis: after [comparing what other tools detect](docs/studies/diagnosis-comparison.md), where a slow step's time goes (done, slice 26), turning rules off and tuning a few margins per scenario (done, slice 28), saturation in the closed model by comparing runs with different VUs (done, slice 27), and within a single run with `ramping_vus` ([ADR-018](docs/adr/ADR-018-diagnosis-v03.md)).

**Later, in no particular order:** the `--ask` window on macOS; defenses against a deliberately malicious agent (running it in a sandbox, a short-lived token required by the target); approving in CI without a person, and a GitHub Action; correlation with server metrics (OTLP, Prometheus); HTTP/2, WebSockets, gRPC, Kafka; choosing when to renew a session from `expires_in`; a desktop GUI. New load models (`ramping_vus`, fixed iterations, per-VU arrival) come last.

Details in the [roadmap](docs/roadmap.md).

## Development

```bash
cargo test --workspace
cargo run --release -p lab-server -- --port 3999   # lab server
```

The tests check the Spanish messages, so `.cargo/config.toml` sets `CRABFALL_LANG=es` for processes started by cargo in this workspace; English has its own tests (`tests/english.rs` in both crates). An installed `crabfall` still defaults to English.

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check` and `clippy` on Linux, and the tests on Linux and Windows, on every push to `main` or `dev` and on every pull request. Warnings count as errors. On Windows the diagnosis pathologies are skipped (`CRABFALL_SKIP_TIMING_TESTS=1`): they measure real time and that 2-core runner cannot keep the load schedule. The engine tests still run there, without requiring the absence of the "scheduler fell behind" warning.

To test against more realistic APIs: [shop-api](../shop-api) (local, with planted performance problems) and [market-api](../market-api) (Postgres, JWT, rate limit and uploads), separate repos with scenarios in their `.crabfall/`.

## Documentation

- [Roadmap](docs/roadmap.md)
- [Study: the diagnosis compared with other tools](docs/studies/diagnosis-comparison.md)
- [ADR-001 — Claude-first](docs/adr/ADR-001-claude-first.md)
- [ADR-002 — Scenario format](docs/adr/ADR-002-scenario-format.md)
- [ADR-003 — hyper and the scheduler](docs/adr/ADR-003-hyper-and-scheduler.md)
- [ADR-004 — Approval and threat model](docs/adr/ADR-004-approval.md)
- [ADR-005 — Diagnosis](docs/adr/ADR-005-diagnosis.md)
- [ADR-006 — Comparing runs](docs/adr/ADR-006-comparison.md)
- [ADR-007 — Datasets](docs/adr/ADR-007-datasets.md)
- [ADR-008 — Think time and tweaks by flag](docs/adr/ADR-008-think-time-and-tweaks.md)
- [ADR-009 — HTML report](docs/adr/ADR-009-html-report.md)
- [ADR-010 — Setup per VU](docs/adr/ADR-010-per-vu-setup.md)
- [ADR-011 — Session renewal](docs/adr/ADR-011-session-renewal.md)
- [ADR-012 — File uploads (multipart)](docs/adr/ADR-012-multipart.md)
- [ADR-013 — Bytes sent and received](docs/adr/ADR-013-traffic.md)
- [ADR-014 — Cookies, redirects, compression and forms](docs/adr/ADR-014-real-http-client.md)
- [ADR-015 — Terminal TUI](docs/adr/ADR-015-tui.md)
- [ADR-016 — Output language](docs/adr/ADR-016-language.md)
- [ADR-017 — The agent asks, a person confirms in a window](docs/adr/ADR-017-window-confirmation.md)
- [ADR-018 — Diagnosis for v0.3](docs/adr/ADR-018-diagnosis-v03.md)

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option. Copyright 2026 Gonzalo Martínez.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
