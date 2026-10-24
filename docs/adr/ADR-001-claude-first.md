# ADR-001 — Claude-first: a CLI with JSON output + a Claude Code skill and hook

Status: **accepted** · 12 Oct 2026 · revised 12 Oct 2026 (installation)

## Context

The tool has to work with an agent that designs scenarios and analyzes results. The original plan aimed at a generic MCP server from the start, plus importers (OpenAPI, curl, Postman) and a GUI.

We are one person, and the client we will use is Claude Code, which already has a shell and reads and writes the repo's files.

## Decision

1. **One core, thin interfaces.** All the logic (scenario, engine, metrics, diagnosis) lives in a library. The interfaces only call it.
2. **The first interface is the `crabfall` CLI**, and every command Claude uses has stable `--json` output (today `validate`, `check` and `run`; later `findings`). `crabfall schema` prints the scenario's JSON Schema, generated from the types.
3. **The Claude integration is installed with `crabfall claude install`** in the user's repo:
   - the `crabfall` *skill* in `.claude/skills/crabfall/SKILL.md` (source: `integrations/claude/`), which teaches the scenario format (ADR-002) and the cycle design → validate → `check` → the person runs it → analyze. It is invoked as `/crabfall`, or on its own when the request fits;
   - in `.claude/settings.json`, a `PreToolUse` hook that calls `crabfall hook pre-tool-use` and blocks `crabfall run` and `crabfall approve` when Claude invokes them (an extra defense; the real guard is in the binary, see ADR-004);
   - in the same file, permissions so Claude can use `crabfall validate`, `crabfall check` and `crabfall schema` without asking.

   Packaging it as a marketplace plugin was ruled out for now: project files are the simplest and most stable mechanism, and they don't depend on the plugin format. To be revisited when publishing.
4. **No importers in the first version.** Claude reads the OpenAPI spec, the curl command or the code and writes the YAML. What matters is that `validate` gives precise errors.
5. **MCP comes later**, as another thin interface over the same core, when other clients (Cursor, etc.) need support.

## Consequences

- There is less to build before the tool is useful: engine + CLI + skill.
- The CLI's JSON output becomes a contract: it is versioned like the scenario.
- Without MCP, other agents can use the CLI through a shell, but without the skill.
- If Claude often writes bad scenarios, the skill and the `validate` messages get better before importers are considered.
- `crabfall` has to be on the PATH (`cargo install --path crates/crabfall-cli`): the skill and the hook invoke it as `crabfall`. If it isn't, the hook fails open; the binary's guard still holds.
