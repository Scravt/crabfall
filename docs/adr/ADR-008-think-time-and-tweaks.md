# ADR-008 — Think time and tweaks by flag

Status: **accepted** · 17 Oct 2026

## Context

Two things were missing for a run to look like a person using the API and to be tweakable without editing the YAML:

1. **Pauses.** Today a VU chains the steps without a break. With `constant_vus`, 10 VUs can generate hundreds of requests per second, when 10 real people generate a few.
2. **Tweaks.** Trying "the same, but at twice the rate" means editing the scenario. Also, the idea is to have a graphical interface later with a customization panel. That panel has to express everything it tweaks with the same parameters as the CLI, so the GUI is a thin layer (ADR-001).

## Decision

### Think time

```yaml
defaults:
  think: 1s-3s              # for every step in the flows
flows:
  main:
    steps:
      - name: list_products
        request: { method: GET, url: /products }
        think: 5s           # this step overrides defaults; `0s` removes it
```

- **Value:** `2s` is a fixed pause. `1s-3s` is a random pause in that range, uniformly distributed.
- **When:** the pause comes **after** the step, the last one included, because that's what a person reading the page before moving on does.
- **It takes up the VU, but doesn't count toward latency.** The next step measures from the end of the pause. In the open model, the pause counts toward `max_vus`: the skill takes it into account when computing it.
- **Where it doesn't apply:** it isn't allowed in `setup`, because it runs once. `crabfall check` ignores pauses, so the smoke check stays fast.
- **Signature:** the approval signs each step's pause. It only appears in the canonical plan if the step has a pause, so earlier approvals still hold.
- **Comparison:** `results.json` stores the pauses in `load.think`. That way `crabfall compare` doesn't pair second by second two runs with different pauses, because the offered load isn't the same.

### Tweaks by flag

Criterion: anything that would make sense in a run's customization panel goes in as a flag. What defines *what* is tested (steps, requests, checks) doesn't, because that's the scenario.

| Panel | Flags |
| --- | --- |
| Environment and variables | `--env`, `--var name=value` (they existed already) |
| Load profile | `--executor`, `--rate`, `--duration`, `--vus`, `--max-vus`, `--start-rate`, `--stages 100/s:1m,0/s:30s`, `--graceful-stop` |
| Pauses | `--think 1s-3s` (overrides them all; `0s` removes them) |
| Data | `--data users=data/others.csv`, `--pick users=random` |
| Criteria | `--threshold "http.latency.p95<300ms"`, `--threshold "http.latency.p99{step=login}<1s"` (they replace the YAML's; not signed) |

- **Commands:** the four that build the plan accept the same flags: `validate`, `check`, `approve` and `run`. A GUI validates with the same parameters it later runs with.
- **When they apply:** on the parsed scenario, **before** building the plan, and then the `validate` rules run again. So the same rules as in the YAML apply, and the error says which flag fixes it: `missing stages (use --stages)`, or `duration doesn't apply to ramping_rate (remove --duration)`.
- **`--executor`** drops the `load` fields that don't apply to it. For example, `--executor constant_vus` removes `rate` and `max_vus` and keeps `duration`.
- **Signature:** since they change the plan, the approval signs them just like `--var`. The same flags have to be passed to `approve` and `run`; if they don't match, `run` refuses and shows the plan's diff. The approval also stores, unsigned, the flags it was approved with. If `run` comes with others, it shows both sets and the commands to run what was approved or approve the new one. That field is only a help: what authorizes is still the hash.
- **Visibility:** the `approve` summary shows them on the `Tweaks` line, so whoever approves sees that the run isn't exactly the YAML. `run` shows them in the header and `results.json` stores them in `tweaks`.
- **In the code:** `crabfall-core` has the `Overrides` type (`overrides.rs`) and the CLI only parses. A GUI, or an MCP server, uses the same type without going through the CLI.

### Out for now

- **Choosing flows or changing their weights** (`--flow buy=50`): it changes *what* is tested more than *how*. To be considered with a real case.

## Consequences

- **For:** more realistic runs with `constant_vus`, and a single definition of what can be tweaked, shared by the CLI, the GUI and Claude. Errors name the flag.
- **Against:** a scenario plus tweaks is harder to read than the YAML alone. That's why the tweaks are shown in `approve`, in `run` and in `results.json`.
