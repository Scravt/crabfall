# Study — The diagnosis compared with other tools

24 Oct 2026 · for v0.3 · extends [ADR-005](../adr/ADR-005-diagnosis.md)

## Questions

1. What do other load and observability tools detect that Crabfall doesn't?
2. Which rules are worth adding in v0.3?
3. Can the rules be customized, or do they stay fixed in the engine?

## What Crabfall does today

The engine detects with deterministic rules over what is measured on the client side; Claude explains by going to the code and marks it as a hypothesis (principle 3, ADR-005). Same data, same findings.

| Rule | Load model | What it looks at |
| --- | --- | --- |
| `invalid_result` | all | the generator was late (scheduler lag p99 > 5 ms) |
| `breaking_point` | `ramping_rate` | the first sustained window (2 of 3) with dropped iterations, p95 ≥ max(3× baseline, baseline + 50 ms) or errors ≥ 5% |
| `capacity_exceeded` | `constant_rate` | more than 1% of iterations dropped |
| `degradation_over_time` | `constant_rate`, `constant_vus` | the p95 grows in a straight line: end ≥ 2× start, +20 ms, R² ≥ 0.6 |
| `error_threshold` | per step | from < 1% to ≥ 5% errors, sustained; tells apart a functional error (from the start), an expired session (401s midway) and a rate limit (with the policy the server declares) |
| `long_tail` | per step | p99 ≥ 10× p50 and +50 ms, with ≥ 200 requests |
| `new_connections` | per step | new connections above the concurrency > 10% of the requests |
| `bandwidth_limit` | all | the traffic stalls at a ceiling while the load asks for 25% more and the p95 triples |

The thresholds are constants at the top of `diagnose.rs`. They can't be changed or turned off.

## What the others do

| Tool | Approach | What it detects | Customizable? |
| --- | --- | --- | --- |
| **k6** (open source) | User criteria | Nothing on its own: `thresholds` and `checks` written by the user | The user writes everything |
| **Grafana Cloud k6**, Cloud Insights | Fixed rules with a score (0-100, weighted) | 16 audits in three groups: reliability (failure rate per URL, Web Vitals), script best practices (no `thresholds`, no `checks`, too many tag values, lots of third-party content, an old version) and generator health (CPU and memory) | Audits can be turned off or chosen (`cloud.insights.disabled`, `enabledSets`); the thresholds can't be changed. Cloud only |
| **k6 Cloud**, Performance Insights (the previous version) | Fixed rules | *Throughput limit*: requests in flight rise with the VUs while completed ones stay flat. Increasing failures: ~10% above the start. High failures early: > 15% in the first 100 iterations, "usually the script" | No |
| **Gatling** (open source) | User criteria | Global or per-request `assertions` on percentiles and failure percentage | The user writes everything |
| **Gatling Enterprise**, AI Analysis (Jun 2026) | Generative AI | A summary of one run, the trend over the latest runs, and a comparison of 2 to 5 runs with a verdict (`Similar`, `SomeDiscrepancies`, `Divergent`), a confidence level and 2-4 recommendations | No; an admin enables it. Gatling notes it has to be verified before deciding |
| **NeoLoad**, Augmented Analysis | Machine learning | Splits the run into intervals (healthy, degraded, neutral) with a name ("possible crash at 2000 VUs") from latency, errors, throughput and VUs | It doesn't say it can be tuned; intervals can be added by hand |
| **LoadRunner Enterprise** | Analytics (per the vendor) | Anomaly detection live and afterwards, correlation, trends | Not publicly documented |
| **Azure Load Testing**, Actionable Insights (preview, May 2025) | Generative AI with server metrics | Latency spikes, failures, throughput anomalies and resource bottlenecks, correlating client and server (for example, more VUs together with latency in Cosmos DB) | The server components and metrics are chosen |
| **Locust** | User criteria | Exits with 1 if any request failed; the rest with a custom listener (failures > 1%, p95 > 800 ms) | The user writes everything |
| **JMeter**, HTML report | A standard score | APDEX per transaction (satisfied / tolerated) | Global or per-transaction thresholds |
| **Artillery** | User criteria and a score | The `ensure` (pass/fail) and `apdex` plugins | The user writes everything |
| **Dynatrace** (APM, production) | Self-learning baselines | Degradation relative to normal, with an absolute and a relative margin (example: 100 ms and 50% over the baseline), persistence ("3 of 5 samples") and no alerting on low traffic | Sensitivity, margins, and a choice between an adaptive baseline, a seasonal one or a fixed threshold |

## What comes out of the comparison

### 1. There are three approaches, and Crabfall is in the one that can be explained

- **User criteria** (k6, Gatling, Locust, Artillery): they say whether it passed, not what happened. Crabfall already has them: `thresholds`, `checks` and `--threshold`.
- **Fixed rules** (k6 Cloud, Crabfall): same data, same finding, with numbers. They can be tested with a lab.
- **ML or generative AI** (NeoLoad, Gatling Enterprise, Azure, LoadRunner): they explain more, but they're neither reproducible nor verifiable, and the vendors themselves ask you to check what they say. None of them separates what was measured from what is assumed.

Crabfall already gets what generative AI gives through Claude, and marks it as a hypothesis. **Recommendation: no ML or LLM in the engine.** The rules stay deterministic.

### 2. What Crabfall detects and the others don't (worth showing in the docs)

- The **breaking point with dropped iterations** in an open model, without coordinated omission: VU-based tools mistake it for "high latency".
- The **generator as the bottleneck**, through the scheduler lag, not only through CPU as in k6 Cloud.
- The **limit the server declares** (`RateLimit-Policy`, `Retry-After`), the **expired session** (401s midway) and the **functional error** (failures from the start, like k6's rule for the first 100 iterations).
- Too many **new connections** and **bandwidth**.

### 3. What's missing, by value

| # | Candidate rule | Who has it | Why |
| --- | --- | --- | --- |
| 1 | **Saturation with fixed VUs** (`throughput_plateau`) — *see the correction below* | k6 (*throughput limit*), NeoLoad | With `constant_vus` there are no drops: `capacity_exceeded` never fires and `breaking_point` only looks at ramps. By Little's law (concurrency = throughput × latency), if the VUs are busy and the throughput doesn't rise, latency pays for it. Today that run comes out "no findings". With `ramping_vus` (backlog) it will be the same or worse. |
| 2 | **Server or payload** (TTFB vs receiving) | — (it was in ADR-005) | We already measure each request's phases. It separates "the server is slow to answer" from "the response is big or the link is slow", and tells Claude where to look in the code. |
| 3 | **Contagion between steps** as a finding of its own | NeoLoad (partly) | Today it's inside `breaking_point`. A healthy step degrading when another saturates points to a shared resource (pool, CPU, lock). |
| 4 | **Scenario best practices** | k6 Cloud | No `checks` (a 200 with an error goes unseen), a closed model without `think`, too many distinct URLs. They go as `validate` or `approve` warnings, not as run findings. |
| 5 | **Run phases** | NeoLoad | A summary of the run in stretches (stable, degrading, saturated, recovering). It helps the report and Claude. It's more a view of the findings and the series than a new rule. |
| 6 | **Trend across several runs** | Gatling Enterprise | Today runs are compared in pairs or against the baseline. A slow drift from version to version doesn't show. Better after "approving in CI". |
| — | Server metrics | Azure, APM | Still out (D4). It's what adds the most, but it changes the scope: it stays for after v0.3. |

Rules 1 and 2 are the clearest: no other open-source tool owns them, they come from data we already measure, and they can be tested with the lab (a server with a fixed pool under `constant_vus`, and large responses through a slow link).

## Customizable?

What the comparison shows:

- **The pass/fail criteria are defined by the user in all of them.** Crabfall already does that with `thresholds`.
- **Nobody lets you rewrite the diagnosis rules.** k6 Cloud lets you turn audits off, not change thresholds. Dynatrace, which lives off this in production, lets you tune sensitivity and margins, not write rules.

Options:

| | What it is | For | Against |
| --- | --- | --- | --- |
| A | Fixed, as today | Reproducible; two projects say the same with the same data | A noisy system gives false positives that can't be silenced |
| B | **Turn a rule off** per scenario or in `config.yaml` | Cheap; solves known noise ("this endpoint always has a long tail") | Something that matters can be turned off |
| C | **Tune a few thresholds** (sensitivity or margins) | Covers very fast or very slow systems (+50 ms doesn't mean the same at 2 ms as at 2 s) | It gets tuned until nothing comes out; comparing runs with different thresholds misleads |
| D | Custom rules (DSL or plugins) | Maximum flexibility | Expensive; overlaps with `thresholds`; for what the rules don't cover, there's Claude |

**Recommendation: B and a bounded version of C; not D.**

- `diagnosis:` in the scenario or in `config.yaml`, and by flag (principle 7): `off: [long_tail]` and a few named settings with bounds (for example, `latency_margin` and `min_requests`), not the internal constants.
- What was changed is recorded in `results.json` and in the report. If two runs use different settings, `compare` warns, as it does today when the scenario changes.
- Not signed (it doesn't change the traffic), like `thresholds`.
- The skill tells Claude not to turn off or loosen rules so a run "passes": if it thinks something is noise, it proposes it to the person.

## Decisions

Accepted on 24 Oct 2026, as recommended:

1. The rules stay deterministic, with no ML or LLM in the engine.
2. In v0.3: rules 1 and 2; rule 3 if there's time; rule 4 as `validate` warnings.
3. Customization: B and a bounded C; not D.
4. Phases (5) and the trend (6) come later.

They went into [ADR-018](../adr/ADR-018-diagnosis-v03.md), with their lab.

**Correction (24 Oct 2026).** Rule 1 doesn't work as written. With `constant_vus` all VUs start together and the concurrency is N for the whole run, so the throughput is flat whether the server is healthy or saturated, and the latency is always N ÷ throughput: there is no "before" to compare against. k6's *throughput limit* needs VUs that rise. The rule moves to `ramping_vus`; meanwhile, `fixed_concurrency` (ADR-018) says what a closed model can't show.

## Sources

- Grafana Cloud k6: [Cloud Insights, reference](https://grafana.com/docs/grafana-cloud/testing/k6/analyze-results/get-cloud-insights/reference/), [configure](https://grafana.com/docs/grafana-cloud/testing/k6/analyze-results/get-cloud-insights/configure/), [Performance Insights](https://grafana.com/docs/grafana-cloud/k6/analyze-results/get-performance-insights/), [Cloud Insights GA (Jul 2024)](https://grafana.com/whats-new/2024-07-15-cloud-insights-in-grafana-cloud-k6-is-generally-available/)
- Gatling: [assertions](https://docs.gatling.io/concepts/assertions/), [AI Analysis](https://gatling.io/product/ai-analysis), [AI run comparison](https://docs.gatling.io/ai-for-analysis/ai-run-comparison/index.md), [trends and comparison](https://gatling.io/updates/ai-analysis-trends-comparison)
- NeoLoad: [Augmented analysis](https://docs.tricentis.com/neoload-web-v2/en-us/content/references/augmented_analysis.htm)
- LoadRunner Enterprise: [capabilities summary (NC DIT)](https://it.nc.gov/services/software-quality-security-testing/testing-tools-capabilities/loadrunner-enterprise)
- Azure Load Testing: [actionable insights](https://learn.microsoft.com/ga-ie/azure/app-testing/load-testing/how-to-analyze-test-results-using-actionable-insights), [server metrics](https://learn.microsoft.com/en-us/azure/app-testing/load-testing/how-to-monitor-server-side-metrics)
- Locust: [running without the web UI, exit codes](https://docs.locust.io/en/latest/running-without-web-ui.html)
- JMeter: [HTML report and APDEX](https://jmeter.apache.org/usermanual/generating-dashboard.html)
- Artillery: [apdex](https://www.artillery.io/docs/reference/extensions/apdex), [best practices, ensure](https://artillery.io/docs/get-started/best-practices)
- Dynatrace: [anomaly detection](https://docs.dynatrace.com/managed/dynatrace-intelligence/anomaly-detection), [adjust sensitivity](https://docs.dynatrace.com/docs/dynatrace-intelligence/anomaly-detection/adjust-sensitivity-anomaly-detection)
- Saturation: [the saturation point of an instance (OneUptime, 2026)](https://oneuptime.com/blog/post/2026-09-08-single-instance-saturation-point/view), [USL (Baron Schwartz)](https://www.datacouncil.ai/hubfs/DataEngConf/NYC%2018/slides/Quantifying%20Scalability%20With%20the%20Universal%20Scalability%20Law%20-%20Baron%20Schwartz.pdf?hsLang=en)
