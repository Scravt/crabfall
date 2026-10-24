# ADR-015 — A TUI in the terminal

Status: **accepted** · 19 Oct 2026

## Context

Today Crabfall is used through commands (`approve`, `run`, `findings`, `compare`), with Claude designing and analyzing, and with an HTML report per run. The original plan was a Tauri GUI; it was swapped for a TUI in the terminal, at least for now (roadmap, 19 Oct 2026). What's missing is a place to operate without remembering flags and, above all, to watch a run while it happens: today there's only a progress line.

## Decision

### Who does what

| | What for | When |
| --- | --- | --- |
| TUI | Operating and watching live: choose, tweak, try, approve, run, stop | Before and during |
| HTML report (ADR-009) | Analyzing in detail and sharing | After |
| Claude (ADR-001) | Designing scenarios and explaining findings from the code | Before and after |

The TUI doesn't copy the HTML's charts: it opens the report with a key. What it adds is what the HTML can't do, the live run.

### How it opens

`crabfall` with no arguments, in an interactive terminal, opens the TUI. Without a terminal (Claude, CI, a pipe) it shows the help as before. Subcommands don't change. The Claude Code hook also blocks bare `crabfall`.

### Features (first version)

- **Choose:** the scenarios in `.crabfall/` with their description, the approval status per environment (approved, changed since the approval, not approved) and the last run. The environment is chosen at the top.
- **Tweak:** the same tweaks as the flags (ADR-008), with the same `Overrides` type from `crabfall-core`. They're validated while typing, with the error on the field, and the equivalent command is shown to copy or hand to Claude. It doesn't edit the YAML.
- **Try:** the smoke check, only against loopback as always (ADR-004 §4).
- **Approve:** the same risk summary as `crabfall approve` (with the diff if it changed), and the host is typed. The TUI is a terminal with a person, so it complies with ADR-004; Claude can't use it.
- **Run and stop:** the live view below, with `q` to stop after confirming.
- **Results:** the summary at the end, with `o` to open `report.html`.
- **History (`h`):** the scenario's runs in the environment, newest first, with the result, duration, requests, worst p95 and findings. Enter compares the chosen one with the previous; with a marked baseline (Space), against the baseline. The comparison is `crabfall compare`'s: verdict, warnings, regressions, improvements, steps and findings (slice 19).

### During the run

- **Header:** scenario and environment, progress, phase (`vu_setup`, load or graceful stop), the approval and the `http` line.
- **Last-second indicators, against what was asked:** iterations/s and dropped, busy VUs, requests/s, errors, checks and bytes per second.
- **Sparklines:** p95 and errors per second, to see the shape (whether it rises, whether it breaks).
- **Steps:** requests, error %, p50 and the p95 of the last 5 s (not the cumulative one, so the change shows), and the status codes.
- **Live warnings:** late scheduler, dropped iterations, VUs left out of `vu_setup`, renewal failures. Findings come at the end: the diagnosis needs the whole run.

### At the end

- **Tabs:** summary (verdict, thresholds, totals, traffic and the quick comparison with the previous run), findings (evidence, causes, code file), steps (percentiles, codes, declared limit, bytes per request) and failed checks.
- **Keys:** `o` opens the report, `r` runs again with the same tweaks, `c` compares (once the history exists).

### What the engine needs

- **Stopping in order:** stop starting iterations, wait for those in progress (graceful stop) and save `results.json` and the report, marked as interrupted by the person. The diagnosis runs over what there was. `crabfall run` uses it too, with Ctrl+C.
- **The live series:** the collector publishes every second the point it closed (global and per step), not only at the end.

### Library

`ratatui` with `crossterm`: the standard choice in Rust, and it works on Windows, Linux and macOS.

## How it turned out (slice 18)

- **Code:** `crates/crabfall-cli/src/tui/` (state and keys in `mod.rs`, drawing in `view.rs`). It uses `prepare_in`, `approve::{summary, verify, sign}` and `run::execute`, which the commands also use: there is no duplicated logic.
- **Form:** executor, rate, duration, VUs, max VUs, start rate, stages, graceful stop, pause, renewal and thresholds. `--data`, `--pick` and `--var` stay CLI-only. A scenario approved with tweaks opens with those tweaks loaded, so it can run without approving again.
- **Live:** each step's "last 5 s" p95 is the worst per-second p95 of those five (there is no histogram for the window). Checks aren't shown live: the collector doesn't publish them per second.
- **Stopping:** `q` (or Ctrl+C) asks and stops in order; while draining, `q` asks whether to cut it now. Leaving the TUI with a run in progress cuts it and waits for it to save.
- **Tests:** the TUI is drawn on an in-memory terminal (`TestBackend`): the list, the detail with valid and invalid tweaks, running without approval, approving with the wrong host, the background check and the run screen. Signing isn't tested there because it uses the system keyring.

## Consequences

- **For:** a complete flow without remembering flags, and the live run, which didn't exist. Stopping with results also improves `crabfall run`.
- **Against:** a second interface to keep up to date with every new tweak; that's why it uses the same `Overrides` and the same approval summary as the CLI, not copies.

## Help for whoever opens it for the first time (20 Oct 2026)

Trying it, the footer with the keys wasn't enough: it said which keys there are, but not what to do next or why, and the scenario's description was cut off.

- **What's next:** the scenario shows the three steps at the top (check → approve → run), which one is done, and a "Next" line with the key and the reason. The check is marked "doesn't apply" if the target isn't loopback, because `check` doesn't run there. The check's status lives in the TUI session: it isn't saved.
- **`?`:** a help per screen, in sentences. Any key closes it and does nothing else (so `q` doesn't quit by accident). While typing a value or the host, `?` is a character.
- **Enter in the history:** before, it compared with the previous run and Space marked the baseline; Enter is what one tries first to choose. Now Enter chooses a run and Enter on another compares them (the first one is the "before"); `c` compares with the previous run. Space still works like Enter.
- **No welcome screen:** with "Next" and `?` it isn't needed, and it would annoy whoever already uses it.
