# ADR-019 — An MCP server: the same CLI, for any agent

Status: **accepted** · 24 Oct 2026 · extends [ADR-001](ADR-001-claude-first.md) · keeps [ADR-004](ADR-004-approval.md) and [ADR-017](ADR-017-window-confirmation.md)

## Context

ADR-001 made Crabfall Claude-first: a CLI with `--json`, a skill and a hook, installed with `crabfall claude install`. It left MCP for later, "as another thin interface over the same core, when other clients need support". Other agents (Cursor, VS Code, Claude Desktop, anything that speaks MCP) can already use the CLI through a shell, but without the skill they don't know which commands exist, how to call them, or what the exit codes mean. There's no client to integrate with yet: the server is built against the protocol and the two test projects (`market-api`, `shop-api`).

## Decision

### 1. `crabfall mcp`, over stdio

- `crabfall mcp [--project DIR]` speaks MCP over stdio: JSON-RPC 2.0, one message per line. By default the project is the current folder or a parent with `.crabfall/`; the client starts it there.
- Methods: `initialize` (it negotiates `2025-06-18`, `2025-03-26` or `2024-11-05`, and returns instructions for the agent), `ping`, `tools/list`, `tools/call`. Notifications get no answer. No resources or prompts yet: the tools are enough.
- Written by hand, with no SDK: it's ~400 lines and has no dependencies; the protocol surface used is small.

### 2. Each tool runs the same binary

Each tool runs `crabfall <command> … --json` as a child process in the project folder, and returns its JSON in `structuredContent`, along with the exit code, what it means and what it printed on stderr.

- **One source of truth:** the server can't behave differently from the terminal, or skip the guard. A new rule or a new tweak reaches the MCP without touching it.
- **Isolation:** a failing command doesn't take down the server.
- **Cost:** a process per call, a few milliseconds. Refactoring every command to return a value was the alternative; it's a bigger change for no gain in behavior.
- `list_scenarios` is the exception: there's no CLI command for it, so it reads the project with the core.

### 3. What's exposed, and what isn't

| Tool | What | CLI |
| --- | --- | --- |
| `list_scenarios` | scenarios with description and executor (or why they don't parse), environments, each one's latest run and baselines | — |
| `schema` | the scenario's JSON Schema | `crabfall schema` |
| `validate` | validation with tweaks, errors with their path | `crabfall validate --json` |
| `check` | the smoke check, loopback only (ADR-004 §4) | `crabfall check --json` |
| `request_run` | asks a person for the run in a window (ADR-017) | `crabfall run --ask --json` |
| `findings` | the compact summary of a run | `crabfall findings --json` |
| `compare` | two runs, or against the baseline, with `scaling` | `crabfall compare --json` |

- **No `approve`, no `run` without `--ask`, no `baseline`, no TUI.** Load only goes out through `request_run`, which is `run --ask`: the plan and the `limits` are checked first, then a window opens where a person approves or confirms, and the load runs there, where they can stop it. A client without a desktop gets the same refusal as the CLI.
- **Arguments, not flags:** `scenario`, `env`, `vars` (an object) and `tweaks` (an object with the CLI's tweak names: `rate`, `vus`, `diagnosis_off`…; lists repeat the flag). Unknown tweaks, a scenario starting with `-`, and internal flags (`--window`, `--ask`) are rejected before running anything.
- **Annotations:** the read tools are `readOnlyHint`; `check` isn't (it sends requests, once); `request_run` is `destructiveHint` and `openWorldHint`.

### 4. Claude Code and the hook

With Claude Code, the CLI and the skill are still the main path (ADR-001). `crabfall claude install --mcp` also registers the server in `.mcp.json` (keeping other servers) and allows the tools that send no load (`mcp__crabfall__validate`, …); `request_run` isn't allowed, so Claude Code asks before each one, besides the window. The agent can't enable it on its own: Claude Code asks the person to approve a project's servers. If a project registers the server, the `PreToolUse` hook doesn't see MCP calls, but it doesn't need to: the server only exposes what the hook lets through, and the guard is in the binary either way.

### 5. What it doesn't change

The threat model is ADR-004's and ADR-017's: mistakes and over-initiative, not a hostile agent. A malicious agent with a shell doesn't need the MCP to generate load.

## Validation

- `crates/crabfall-cli/src/mcp.rs`: arguments to flags, rejected tweaks and flags, version negotiation.
- `crates/crabfall-cli/tests/mcp.rs`: a client over stdio against a temporary project and a loopback HTTP server: `initialize`, a notification without an answer, `tools/list` (no `approve`), `list_scenarios`, `validate` (valid, an invalid tweak with its flag, an internal flag rejected), `check`, `request_run` refused before the window (an invalid scenario, the environment's limits), `findings` without runs, an unknown tool and `ping`. It never opens a window.
- Read-only sessions against `market-api` and `shop-api` (24 Oct 2026): all 17 scenarios validate, `list_scenarios` shows latest runs and the `search-ramp` baseline, and `compare` returns the saturation of slice 27. Neither `check` nor `request_run` was used.

## Consequences

- **For:** any MCP client can design, check and analyze with Crabfall, and ask a person for a run, with the same guarantees as the CLI.
- **Against:** one more interface to document. A long `request_run` holds the server (one call at a time) and some clients cut long tool calls: their timeout has to cover the person's answer plus the run.
