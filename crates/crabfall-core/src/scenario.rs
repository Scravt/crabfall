//! Modelo del escenario (ADR-002). Por ahora es el subconjunto de la rebanada 1:
//! flows con peso, pasos con request y el executor `constant_rate`.
//!
//! Los doc comments de los campos van al JSON Schema (`crabfall schema`): están en inglés
//! (ADR-016) y escritos para quien escribe el escenario, incluido Claude.

use std::{collections::BTreeMap, time::Duration};

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    load::{LoadPlan, Segment},
    units::{HumanDuration, Rate, Think},
};

pub const FORMAT_VERSION: u32 = 1;

/// A Crabfall load test scenario. It goes in `.crabfall/scenarios/<name>.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Format version. Always `1`.
    pub version: u32,
    /// Scenario name: letters, digits, `_`, `.` and `-`. It should match the file name.
    pub name: String,
    /// What the scenario simulates, in one or two sentences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Free-form data that does not affect the traffic, for example `created_by: agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    /// Scenario variables, usable as `{{name}}`. The environment's, `--var` and `extract` override them (in that order).
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub vars: IndexMap<String, Value>,
    /// Datasets: CSV or JSON files, relative to `.crabfall/`. Each iteration takes a row of each dataset its flow uses, and the steps see it as `{{name.column}}`. They cannot be used in `setup`.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub data: IndexMap<String, DataSpec>,
    /// Cookies: each VU keeps the ones the server sends (`set-cookie`) and sends them back in its requests, like a browser, from its `vu_setup` to its last iteration. `setup` uses its own, separately. Default `true`; `false` turns them off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies: Option<bool>,
    /// Headers, timeout, pause and redirects inherited by every step of `flows` (not by `setup`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<Defaults>,
    /// Steps that run once, in order, before the load (for example, a login). Their `extract`s become variables for every flow. If any fails, the run aborts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<Step>,
    /// Steps each VU runs once, before the load (for example, each user's login). Their `extract`s become variables of that VU in all its iterations. They see `setup` and the datasets (one row per VU). They run for `vus` (constant_vus) or `max_vus` (rate executors). If they fail in a VU, that VU is left out; if they fail in all of them, the run aborts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vu_setup: Vec<Step>,
    /// Renewal of each VU's session, for tokens that expire during the load. Requires `vu_setup`. It runs in the background: the iteration in progress keeps the previous token and the next one takes the new one. It is not part of the load metrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vu_refresh: Option<VuRefresh>,
    /// User flows. Each iteration picks one by its `weight` and runs its steps in order.
    pub flows: IndexMap<String, Flow>,
    /// Load profile. The rate is measured in iterations per second, not requests.
    pub load: Load,
    /// Pass criteria of the run. If any fails, `crabfall run` exits with code 1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<Threshold>,
    /// Diagnosis settings: rules turned off and two named margins. They don't change the traffic, so they need no new approval. The environment in `config.yaml` can set them too; the scenario's win, and `off` adds up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<DiagnosisSpec>,
}

/// Diagnosis settings (ADR-018). Use them for known noise, not to make a run pass.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiagnosisSpec {
    /// Rules to turn off, by the id of their finding: breaking_point, capacity_exceeded, degradation_over_time, error_threshold, long_tail, new_connections, bandwidth_limit, time_split, fixed_concurrency. `invalid_result` can't be turned off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub off: Vec<String>,
    /// How much slower than its baseline a second has to be, besides 3×, to count as degraded (breaking point, bandwidth), and the minimum distance between p50 and p99 for a long tail. Default 50ms; from 1ms to 10s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_margin: Option<HumanDuration>,
    /// The fewest requests a step needs for long_tail (default 200), new_connections and time_split (default 100). From 10 to 100000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_requests: Option<u64>,
}

impl DiagnosisSpec {
    /// Los problemas de cada campo, con su ruta relativa (`off[1]`, `latency_margin`).
    pub fn problems(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (i, rule) in self.off.iter().enumerate() {
            if rule == "invalid_result" {
                out.push((
                    format!("off[{i}]"),
                    crate::tr!(
                        "`invalid_result` can't be turned off: it says whether the numbers are valid",
                        "`invalid_result` no se puede apagar: dice si los números valen"
                    ),
                ));
            } else if !crate::diagnose::RULES.contains(&rule.as_str()) {
                out.push((
                    format!("off[{i}]"),
                    crate::tr!(
                        "unknown rule `{rule}`; they are: {}",
                        "regla desconocida `{rule}`; son: {}",
                        crate::diagnose::RULES.join(", ")
                    ),
                ));
            }
        }
        if let Some(m) = self.latency_margin
            && !(Duration::from_millis(1)..=Duration::from_secs(10)).contains(&m.0)
        {
            out.push((
                "latency_margin".into(),
                crate::tr!("use between 1ms and 10s", "usá entre 1ms y 10s"),
            ));
        }
        if let Some(n) = self.min_requests
            && !(10..=100_000).contains(&n)
        {
            out.push((
                "min_requests".into(),
                crate::tr!("use between 10 and 100000", "usá entre 10 y 100000"),
            ));
        }
        out
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// A header defined in the step overrides the one in `defaults`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<HumanDuration>,
    /// Pause after each flow step, like the step's `think`. A step with its own `think` (even `0s`) overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think: Option<Think>,
    /// How many redirects each step follows, like the request's `redirects`. Default 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirects: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    /// Relative weight against the other flows (they do not need to add up to 100). Default 1.
    #[serde(default = "default_weight")]
    #[schemars(range(min = 1))]
    pub weight: u32,
    /// Steps run in order in each iteration.
    #[schemars(length(min = 1))]
    pub steps: Vec<Step>,
}

fn default_weight() -> u32 {
    1
}

/// Renewal of each VU's session, for example `{ every: 4m, steps: [refresh] }`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VuRefresh {
    /// How often each VU renews, counted from its `vu_setup` or its last renewal. It must be shorter than the token lifetime minus the length of an iteration, because each iteration uses the token it had when it started.
    pub every: HumanDuration,
    /// Renewal steps, in order (for example, `POST /auth/refresh`). They see the VU's variables and what they extract overrides them. Without steps, `vu_setup` is repeated (logs in again). If they fail, that VU stops iterating.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
}

/// A dataset, for example `{ file: data/users.csv }`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataSpec {
    /// File relative to `.crabfall/`. `.csv` with a header row (every cell is a string) or `.json` with an array of objects (keeps types). Column names use letters, digits and `_`.
    pub file: String,
    /// `sequential` (default): rows in order, starting over at the end. `random`: a random row in each iteration. `unique`: each row only once in the whole run (for example, to register users); when they run out, the load ends there.
    #[serde(default)]
    pub pick: Pick,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    #[default]
    Sequential,
    Random,
    /// Cada fila una sola vez en toda la corrida; al acabarse, la carga termina.
    Unique,
}

impl Pick {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Random => "random",
            Self::Unique => "unique",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Stable name, unique in the whole scenario (`[A-Za-z0-9_.-]+`). Metrics are grouped by this name, not by URL.
    #[schemars(regex(pattern = r"^[A-Za-z0-9_.-]+$"))]
    pub name: String,
    pub request: RequestSpec,
    /// Free-form tags. Use `write` on steps that change data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Where the code that serves this step is. Fill it in whenever you know it: diagnosis uses it to go to the code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Response values that become variables (`{{name}}`) for the following steps.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub extract: IndexMap<String, Extract>,
    /// Checks on the response. They do not stop the iteration: they count in `checks.pass_rate`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    /// Pause after this step, like a person reading the page: `2s` or random within a range (`1s-3s`). It does not count in the latency, but it occupies the VU. `crabfall check` ignores it and it is not allowed in `setup`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think: Option<Think>,
}

/// A check. Pick a subject (`status`, `status_in`, `json`, `header`, `body_contains`, `body_matches`); `json` and `header` also take a predicate (`exists`, `equals`, `contains`, `matches`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Expected status code. If a step has status checks, they replace the 2xx expectation in `crabfall check`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// One of these codes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_in: Option<Vec<u16>>,
    /// JSONPath on the body, as in `extract`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<String>,
    /// Name of a response header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// The body contains this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_contains: Option<String>,
    /// The body matches this regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_matches: Option<String>,
    /// With `json` or `header`: it exists (`true`) or not (`false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    /// With `json` or `header`: equal to this value (accepts `{{variables}}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    /// With `header`: contains this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// With `json` or `header`: matches this regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
}

/// A criterion on a metric, for example `{ metric: http.latency.p95, lt: 300ms }`. Use exactly one of `lt`, `lte`, `gt`, `gte`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Threshold {
    /// `http.latency.p50` … `http.latency.p99.9`, `http.latency.avg`, `http.latency.max` (a duration, e.g. `300ms`); `http.error_rate` and `checks.pass_rate` (a percentage, e.g. `1%`); `http.rps` (a number).
    pub metric: String,
    /// Only this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Only the steps with this tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lte: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gte: Option<Value>,
}

/// Where an extracted value comes from. Use exactly one of the three.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Extract {
    /// Simple JSONPath on the body: `$.token`, `$.items[0].id`, `$['a-b']`. Keeps the type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<String>,
    /// Name of a response header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Regex on the body: takes the first capture group, or the whole match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestSpec {
    /// GET, POST, PUT, PATCH, DELETE, HEAD or OPTIONS.
    pub method: String,
    /// Relative to the environment's `base_url` (starts with `/`) or absolute (`https://...`). The host must be in `allowed_hosts`. Accepts `{{variables}}` in the path and the query, never in the host.
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Query parameters; they are added to the URL, encoded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, Value>,
    /// Request timeout. Default 30s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<HumanDuration>,
    /// How many redirects (301, 302, 303, 307, 308) it follows, only to hosts in `allowed_hosts`. Default 10; `0` follows none and the step responds with the 3xx. The latency includes every hop and the status is the last response's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirects: Option<u32>,
    /// JSON body; adds `content-type: application/json`. Excludes `body`, `multipart` and `form`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    /// Text body, as is. Excludes `json`, `multipart` and `form`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// `multipart/form-data` body, for example to upload files: `{ file: { file: files/photo.jpg }, description: "text" }`. Each key is the field name; a string is a text field. Crabfall sets the `content-type` with its boundary. Excludes `json`, `body` and `form`.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub multipart: IndexMap<String, MultipartPart>,
    /// Form body (`application/x-www-form-urlencoded`), for example `{ username: "{{users.email}}", remember: true }`. Adds the `content-type`. Excludes `json`, `body` and `multipart`.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub form: IndexMap<String, Value>,
}

/// A field of a multipart body: text (`"hello {{x}}"`) or a file (`{ file: files/a.pdf }`).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum MultipartPart {
    Text(String),
    File(FilePart),
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FilePart {
    /// File relative to `.crabfall/`, for example `files/photo.jpg`. It is read once and sent whole in each request; the approval signs its content. Up to 100 MB.
    pub file: String,
    /// Name the server sees. Default: the file's. Accepts `{{variables}}`, for example `photo-{{$uuid}}.jpg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// File type. By default it comes from the extension (`application/octet-stream` if unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// Where the code that serves this step is. The engine does not use it: it copies it into the results.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// File path, relative to the repo root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Function, method or handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// OpenAPI `operationId`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

/// The `load` block. A flat struct (not a tagged enum) so errors keep their path and
/// line; `validate` checks which fields apply to each executor.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Load {
    /// `constant_rate`: `rate` iterations/s for `duration` (requires `rate`, `duration`, `max_vus`).
    /// `ramping_rate`: the rate goes from `start_rate` to each `target` of `stages` (requires `stages`, `max_vus`).
    /// `constant_vus`: `vus` users in a loop for `duration`; the rate depends on the latency (requires `vus`, `duration`).
    pub executor: Executor,
    /// Iterations per second, for example `50/s` or `3000/m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<Rate>,
    /// How long the load lasts, for example `30s` or `5m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<HumanDuration>,
    /// Maximum iterations in flight. If there is no room, the iteration is dropped and reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_vus: Option<u32>,
    /// Starting rate of `ramping_rate`. Default `0/s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_rate: Option<Rate>,
    /// Stages of `ramping_rate`: the rate changes linearly up to `target` within `duration`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stages: Option<Vec<Stage>>,
    /// Concurrent users of `constant_vus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub vus: Option<u32>,
    /// How long to wait for the iterations in progress when finishing. Default 30s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graceful_stop: Option<HumanDuration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Executor {
    ConstantRate,
    RampingRate,
    ConstantVus,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    /// Rate at the end of the stage, for example `100/s`.
    pub target: Rate,
    pub duration: HumanDuration,
}

pub const DEFAULT_GRACEFUL_STOP: Duration = Duration::from_secs(30);

impl Load {
    /// Requiere que el escenario haya pasado `validate`.
    pub fn plan(&self) -> LoadPlan {
        let graceful_stop = self.graceful_stop.map_or(DEFAULT_GRACEFUL_STOP, |g| g.0);
        match self.executor {
            Executor::ConstantRate => {
                let rate = self.rate.expect("validado").per_second();
                LoadPlan::Open {
                    ramping: false,
                    segments: vec![Segment {
                        from: rate,
                        to: rate,
                        duration: self.duration.expect("validado").0,
                    }],
                    max_vus: self.max_vus.expect("validado"),
                    graceful_stop,
                }
            }
            Executor::RampingRate => {
                let mut from = self.start_rate.map_or(0.0, |r| r.per_second());
                let segments = self
                    .stages
                    .as_deref()
                    .expect("validado")
                    .iter()
                    .map(|s| {
                        let to = s.target.per_second();
                        let segment = Segment {
                            from,
                            to,
                            duration: s.duration.0,
                        };
                        from = to;
                        segment
                    })
                    .collect();
                LoadPlan::Open {
                    ramping: true,
                    segments,
                    max_vus: self.max_vus.expect("validado"),
                    graceful_stop,
                }
            }
            Executor::ConstantVus => LoadPlan::Closed {
                vus: self.vus.expect("validado"),
                duration: self.duration.expect("validado").0,
                graceful_stop,
            },
        }
    }
}

impl Scenario {
    /// Todos los pasos con el nombre de su flow, en orden.
    pub fn steps(&self) -> impl Iterator<Item = (&str, &Step)> {
        self.flows
            .iter()
            .flat_map(|(flow, f)| f.steps.iter().map(move |s| (flow.as_str(), s)))
    }

    /// Cota de requests/s: tasa máxima de iteraciones × máximo de pasos por flow.
    /// `None` con `constant_vus`, cuya tasa depende de la latencia.
    pub fn max_request_rate(&self) -> Option<f64> {
        let max_steps = self
            .flows
            .values()
            .map(|f| f.steps.len())
            .max()
            .unwrap_or(0);
        self.load.plan().peak_rate().map(|r| r * max_steps as f64)
    }
}

/// JSON Schema del escenario, generado desde los tipos (`crabfall schema`).
pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Scenario)).expect("el schema siempre serializa")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_strict_and_documented() {
        let schema = json_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert!(
            schema["properties"]["flows"]["description"]
                .as_str()
                .unwrap()
                .contains("weight")
        );
        let step = &schema["$defs"]["Step"];
        assert_eq!(step["additionalProperties"], false);
        assert_eq!(schema["$defs"]["Duration"]["type"], "string");
    }
}
