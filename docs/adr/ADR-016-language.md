# ADR-016 — Output language

Status: **accepted** · 19 Oct 2026 · revised 24 Oct 2026 (all the documentation in English)

## Context

Everything Crabfall printed was in Spanish: the terminal, validation errors, findings, the help and the HTML report. Publishing it needs English, and it's best decided before the TUI (ADR-015) so it isn't translated twice.

## Decision

### Languages and how one is chosen

- **Languages:** English and Spanish. **English by default**, always: the operating system's language isn't used.
- **Priority:** `--lang en|es` (on any command, before or after the subcommand) > the `CRABFALL_LANG` variable > `language:` in `.crabfall/config.yaml` (per project) > English.
- **Early resolution:** it's resolved at startup, before clap builds the help, so the help also comes out in the chosen language.

### What is translated

- **The tool's output:** terminal, validation and plan errors, findings and their causes, warnings, the comparison, the commands' help and the HTML report (which also formats numbers and dates according to the language).
- **`results.json`:** the texts (finding titles, causes, warnings) stay in the run's language; the keys are always the same. The report is built in the language of whoever generates it.
- **What doesn't change:** the YAML keys, the JSON keys, command and flag names, the checks' technical labels (`status in`, `body contains`) and the findings' ids.
- **Documentation:** in English: the README, the Claude skill, the schema's documentation (`crabfall schema`), and since 24 Oct 2026 also the ADRs, the roadmap and the studies, so a public repo reads in one language. Code comments stay in Spanish.

### How it's done

- **Both texts together:** each message has its two versions in the same place: `tr!("english {x}", "español {x}")`, or `pick(en, es)` for fixed texts. That way a message can't be added in one language and forgotten in the other. There are no separate translation files.
- **Help:** clap takes it from the doc comments, which stay in English; a table in `main.rs` applies the Spanish when needed.
- **Report:** the language travels in the embedded data and the JavaScript chooses with `L(en, es)`.
- **Tests:** the existing tests check the Spanish texts, so the workspace runs them with `CRABFALL_LANG=es` (`.cargo/config.toml`). English has its own tests (`tests/english.rs` in both crates). An installed `crabfall` doesn't read that file and starts in English.

## Consequences

- **For:** the tool can be published in English without losing Spanish, and the TUI was born bilingual.
- **Against:** every new message is written twice. One more language (for example, Portuguese) would mean moving from pairs to catalogs.
