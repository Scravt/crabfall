# ADR-006 — Comparing runs

Status: **accepted** · 15 Oct 2026

## Context

The usage cycle ends with "I fixed `scryptSync`, did it get better?". Today that is answered by eyeballing two tables. `crabfall` needs to say what improved, what got worse and whether the change is real or noise, always with the same criteria, so Claude doesn't have to guess.

## Decision

`crabfall compare [base] [new] [--json]`. With no arguments, it compares the most recent run with the previous one of the same scenario and environment. `new` defaults to `latest`. The logic lives in `crabfall-core` (`compare.rs`) and reads `results.json`; nothing has to run again.

### Are they comparable?

Scenario, environment, `load` and the approval hash (which covers the whole plan) are compared. Any difference is a warning, not an error: sometimes the scenario is changed on purpose. If `load` is the same, every second of one run has the same offered rate as the same second of the other, and they can be **paired second by second**. If not, only the aggregates are compared, with a warning that the difference may come from the load. If either run has `invalid_result`, that warning comes first.

### Real or noise? (per-step latency)

Each step's verdict comes from the p95:

1. **Magnitude:** it changes by ≥ 10% and ≥ 2 ms. Otherwise, `same`.
2. **Consistency:** a sign test over the paired seconds (those with requests for the step in both runs): in how many seconds did the p95 move in the direction of the change? With p < 0.05 (binomial, two-tailed) it is `better` or `worse`. If it falls short, or there are fewer than 6 seconds, `uncertain`: "it changed, but it may be noise".
3. Without pairing (different loads), the magnitude is enough, with a warning.

The sign test was chosen because it assumes nothing about the shape of the distribution, it explains itself in one line ("worse in 52 of 60 seconds") and it works with what is already stored (the per-second series), without storing histograms.

### What counts as a regression

- A `worse` step.
- Errors: +1 percentage point or more (global or per step).
- Dropped iterations: +1 percentage point or more.
- A new finding of high or medium severity.
- A `breaking_point` that persists but drops by ≥ 10% (the system breaks earlier).
- A threshold that passed and now fails.

The symmetric cases count as improvements. The overall verdict is `better`, `worse`, `same` or `mixed`.

### Findings

They are paired by `id` and step: `new`, `resolved` or `persists`. If a `breaking_point` persists, the rate before and after is shown.

### Output

- Text: verdict, warnings, regressions and improvements one per line, a per-step table and the findings.
- `--json`: the same, structured, with the evidence (seconds for and against, p-value).
- Exit code `1` if there are regressions (useful for CI) and `2` if a run doesn't exist or was aborted.

## Consequences

- Without stored histograms, the test works on the per-second p95. With very few requests per second, each window's p95 is noisy and the test leans toward `uncertain`: it errs on the safe side.
- The thresholds (10%, 2 ms, 1 pp, p < 0.05) are constants at the top of the module, as in the diagnosis.
- Comparing against a fixed "baseline run" (stored apart, for CI) is left for later: today the base is any run in `.crabfall/runs/`.
- Each step also compares bytes per request (ADR-013), sent and received. A change of at least ×1.5 (or half) and 1 KB comes out as a warning, not as a regression or improvement: a response that grew can explain a latency change without the server getting worse. Runs without bytes aren't compared.

## Fixed baseline run (20 Oct 2026)

- **Setting it:** `crabfall baseline [run]` (by default, the most recent) copies its `results.json` to `.crabfall/baselines/<scenario>.<environment>.json`. That folder isn't in `.gitignore`: it is versioned, so the team compares against the same reference. An aborted run can't be the baseline. In the TUI, `b` in the history does the same, and the history marks which one it is.
- **Using it:** `crabfall compare --baseline [run]` compares the run (by default, the most recent) against the baseline of its scenario and environment, instead of the previous one. It exits with 1 if there are regressions, as always. Without a baseline, it exits with 2 and says how to set one.
- **Why not the previous run:** always comparing against the previous one lets regressions slip in little by little (each run a bit worse than the last, never enough). The fixed baseline is the accepted reference until someone decides to move it.
- **What CI still needs:** running in CI needs a way to approve without a person at a terminal (ADR-004), which doesn't exist. For now the baseline works on the machine of whoever runs it.
