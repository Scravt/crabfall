# ADR-009 — HTML report

Status: **accepted** · 17 Oct 2026

## Context

Results are read in the terminal (`crabfall run`, `crabfall findings`, `crabfall compare`) or in `results.json`. Seeing the shape of a run (when it breaks, how fast latency rises, whether it improved over the previous one) needs a chart. It also needs something that can be handed to another person who doesn't have `crabfall` installed. And it's the first visual step before the GUI (principle 4): what gets solved here is reused there.

## Decision

### One self-contained file per run

- **When it's generated:** `crabfall run` writes `.crabfall/runs/<id>/report.html` next to `results.json`. `crabfall report [run] [--open]` regenerates it for any run, for example an old one, or after upgrading `crabfall`.
- **Self-contained:** a single file, with no CDN or external resources. It opens offline and can be attached or sent as is. The data goes inside, in a `<script type="application/json">`, and `<` is escaped so no value (a body, a server error message) can close the tag.
- **Security:** all text that comes from the run enters the DOM with `textContent`, never as HTML.
- **Comparison:** if there is a previous run of the same scenario and environment, the report includes the comparison (ADR-006). With the same load, also a chart of both runs' p95, second by second.

### What it shows

1. **Header:** scenario, environment, date, the thresholds' verdict, load, pauses, tweaks by flag and approval.
2. **Indicators:** requests, iterations, errors, dropped and checks.
3. **Warnings and findings:** each finding with its severity (icon and text, never color alone), the evidence with readable names, the responsible code and the possible causes, marked as hypotheses.
4. **Comparison** with the previous run: verdict, regressions, improvements and p95 per step.
5. **Per second:** iterations (started and dropped), latency (p50, p95 and p99), errors if there were any, and a small chart of each step's p95. A finding's window, such as the breaking point, is shaded.
6. **Tables:** steps with percentiles, checks (with reasons if they failed), thresholds and the per-second data. The tables are the chart-free view and cover the low contrast of some colors.

### Chart criteria

- **One axis per chart.** Different measures go in different charts.
- **Interaction:** a crosshair with a tooltip showing every series for that second. It can also be walked with the keyboard (arrows).
- **Optional logarithmic scale for latency:** after a breaking point, the tail of seconds flattens the healthy millisecond line. The button says what it does, and `report.html?log` opens straight in that scale.
- **Palette:** the first three positions of the reference categorical palette, validated in light and dark. Status colors (good, warning, severe) are reserved for severity and verdicts.
- **Theme:** light and dark mode following the system, plus a button to switch.

### Why generated HTML and not a server

A static file needs nothing running, can be versioned or attached, and opens no ports. The GUI will have its own process. The report solves "look at and share a run" without depending on it.

## Consequences

- **For:** every run has its graphical view with no extra steps. One file is shared. The template (`crates/crabfall-cli/src/report.html`) is dependency-free HTML and JavaScript, reusable in the GUI.
- **Against:** the file grows with the duration (~1 KB per second of run). The template is untyped JavaScript: the Rust tests check that the data goes in correctly, and the interaction was checked by hand with headless Edge (a screenshot and a simulated mouse through the DevTools protocol).
