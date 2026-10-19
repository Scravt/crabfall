//! Comparación de dos corridas (ADR-006).
//!
//! Lee dos `results.json` y dice qué mejoró, qué empeoró y si el cambio de latencia es
//! real o ruido: si la carga es la misma, cada segundo de una corrida se empareja con el
//! mismo segundo de la otra y se aplica un test de signos. Mismos datos, mismo veredicto.

use std::{collections::BTreeMap, f64::consts::LN_2};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    diagnose::{Finding, Severity},
    metrics::{Percentiles, Series, StepPoint, Totals},
    scenario::Source,
};

/// Umbrales. Están juntos para poder ajustarlos con casos reales.
const LATENCY_MIN_RATIO: f64 = 0.10;
const LATENCY_MIN_DELTA_MS: f64 = 2.0;
const SIGN_TEST_ALPHA: f64 = 0.05;
const SIGN_TEST_MIN_WINDOWS: usize = 6;
const RATE_MIN_DELTA_PP: f64 = 1.0;
const BREAKING_POINT_MIN_RATIO: f64 = 0.10;
/// Un cambio de tamaño por request que vale la pena avisar: al menos ×1,5 (o la mitad
/// menos) y 1 KB.
const SIZE_MIN_RATIO: f64 = 1.5;
const SIZE_MIN_DELTA_BYTES: f64 = 1024.0;

/// Lo que se lee de `results.json`; el resto se ignora.
#[derive(Debug, Deserialize)]
pub struct Run {
    pub run_id: String,
    pub scenario: String,
    pub environment: String,
    #[serde(default)]
    pub aborted: Option<String>,
    #[serde(default)]
    pub approval: Option<Approval>,
    #[serde(default)]
    pub load: Value,
    pub totals: Totals,
    #[serde(default)]
    pub thresholds: Vec<ThresholdRun>,
    pub steps: Vec<StepRun>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub series: Series,
}

#[derive(Debug, Deserialize)]
pub struct Approval {
    pub hash: String,
}

#[derive(Debug, Deserialize)]
pub struct ThresholdRun {
    pub threshold: String,
    pub actual: Option<f64>,
    pub passed: bool,
}

#[derive(Debug, Deserialize)]
pub struct StepRun {
    pub name: String,
    #[serde(default)]
    pub source: Option<Source>,
    pub requests: u64,
    pub failed: u64,
    pub non_2xx: u64,
    pub latency_ms: Option<Percentiles>,
    /// Las corridas anteriores a ADR-013 no los tienen.
    #[serde(default)]
    pub bytes_sent: u64,
    #[serde(default)]
    pub bytes_received: u64,
}

impl StepRun {
    fn error_pct(&self) -> Option<f64> {
        pct(self.failed + self.non_2xx, self.requests)
    }

    /// Bytes por request; `None` sin requests o si la corrida no midió bytes.
    fn per_request(&self, bytes: u64) -> Option<f64> {
        (self.requests > 0 && bytes > 0).then(|| (bytes as f64 / self.requests as f64).round())
    }
}

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub base: RunRef,
    pub new: RunRef,
    pub verdict: Verdict,
    /// Misma `load`: los segundos se emparejan y la latencia pasa por el test de signos.
    pub same_load: bool,
    /// Segundos de carga presentes en las dos corridas.
    pub paired_windows: usize,
    pub warnings: Vec<String>,
    /// Una línea por regresión, con los números.
    pub regressions: Vec<String>,
    pub improvements: Vec<String>,
    pub totals: TotalsDiff,
    pub steps: Vec<StepDiff>,
    pub findings: Vec<FindingDiff>,
    pub thresholds: Vec<ThresholdDiff>,
}

#[derive(Debug, Serialize)]
pub struct RunRef {
    pub run_id: String,
    pub scenario: String,
    pub environment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Better,
    Worse,
    Same,
    Mixed,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Delta {
    pub base: Option<f64>,
    pub new: Option<f64>,
    /// Cambio relativo en %, si la base es mayor que cero.
    pub change_pct: Option<f64>,
}

impl Delta {
    fn new(base: Option<f64>, new: Option<f64>) -> Self {
        let change_pct = match (base, new) {
            (Some(b), Some(n)) if b > 0.0 => Some(round1(100.0 * (n - b) / b)),
            _ => None,
        };
        Self {
            base,
            new,
            change_pct,
        }
    }

    /// `nueva - base`.
    pub fn diff(&self) -> Option<f64> {
        Some(self.new? - self.base?)
    }
}

#[derive(Debug, Serialize)]
pub struct TotalsDiff {
    pub iteration_rate: Delta,
    pub dropped_pct: Delta,
    pub error_pct: Delta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Both,
    OnlyBase,
    OnlyNew,
}

/// Veredicto de latencia de un paso (sobre el p95).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Better,
    Worse,
    /// Cambio por debajo del umbral de magnitud.
    Same,
    /// Cambio grande en el agregado pero no consistente segundo a segundo: puede ser ruido.
    Uncertain,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SignTest {
    /// Segundos emparejados con requests del paso en las dos corridas y p95 distinto.
    pub windows: usize,
    pub better: usize,
    pub worse: usize,
    /// Binomial de dos colas.
    pub p_value: f64,
}

#[derive(Debug, Serialize)]
pub struct StepDiff {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    pub presence: Presence,
    pub change: Change,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sign_test: Option<SignTest>,
    pub requests: Delta,
    pub error_pct: Delta,
    pub p50_ms: Delta,
    pub p95_ms: Delta,
    pub p99_ms: Delta,
    /// Bytes por request (ADR-013).
    pub sent_per_request: Delta,
    pub received_per_request: Delta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingState {
    New,
    Resolved,
    Persists,
}

#[derive(Debug, Serialize)]
pub struct FindingDiff {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    pub state: FindingState,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_title: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ThresholdDiff {
    pub threshold: String,
    pub base_passed: Option<bool>,
    pub new_passed: Option<bool>,
    pub base_actual: Option<f64>,
    pub new_actual: Option<f64>,
}

pub fn compare(base: &Run, new: &Run) -> Result<Comparison, String> {
    for run in [base, new] {
        if let Some(reason) = &run.aborted {
            return Err(format!(
                "la corrida `{}` no generó carga ({reason}): no hay métricas para comparar",
                run.run_id
            ));
        }
    }

    let same_load = base.load == new.load;
    let windows = if same_load {
        paired_windows(base, new)
    } else {
        vec![]
    };
    let (mut regressions, mut improvements) = (Vec::new(), Vec::new());
    let totals = totals(base, new);
    for (what, d) in [
        ("errores", totals.error_pct),
        ("iteraciones descartadas", totals.dropped_pct),
    ] {
        rate_change(&mut regressions, &mut improvements, what, d);
    }
    let steps = steps(base, new, same_load.then_some(windows.as_slice()));
    for step in &steps {
        step_changes(&mut regressions, &mut improvements, step, same_load);
    }
    let findings = findings(base, new, &mut regressions, &mut improvements);
    let thresholds = thresholds(base, new, &mut regressions, &mut improvements);

    let verdict = match (regressions.is_empty(), improvements.is_empty()) {
        (true, true) => Verdict::Same,
        (true, false) => Verdict::Better,
        (false, true) => Verdict::Worse,
        (false, false) => Verdict::Mixed,
    };
    Ok(Comparison {
        base: run_ref(base),
        new: run_ref(new),
        verdict,
        same_load,
        paired_windows: windows.len(),
        warnings: [warnings(base, new, same_load), size_changes(&steps)].concat(),
        regressions,
        improvements,
        totals,
        steps,
        findings,
        thresholds,
    })
}

fn run_ref(r: &Run) -> RunRef {
    RunRef {
        run_id: r.run_id.clone(),
        scenario: r.scenario.clone(),
        environment: r.environment.clone(),
    }
}

fn warnings(base: &Run, new: &Run, same_load: bool) -> Vec<String> {
    let mut out = Vec::new();
    for r in [base, new] {
        if r.findings.iter().any(|f| f.id == "invalid_result") {
            out.push(format!(
                "la corrida `{}` tiene `invalid_result`: el generador fue el cuello de botella y sus números no son confiables",
                r.run_id
            ));
        }
    }
    if base.scenario != new.scenario {
        out.push(format!(
            "son escenarios distintos (`{}` y `{}`)",
            base.scenario, new.scenario
        ));
    }
    if base.environment != new.environment {
        out.push(format!(
            "son entornos distintos (`{}` y `{}`)",
            base.environment, new.environment
        ));
    }
    if !same_load {
        out.push(
            "la carga es distinta: se comparan solo los agregados y las diferencias pueden deberse a la carga".into(),
        );
    } else if let (Some(a), Some(b)) = (&base.approval, &new.approval)
        && a.hash != b.hash
    {
        out.push(
            "el escenario cambió entre corridas (otra aprobación): requests, pasos o variables pueden ser distintos".into(),
        );
    }
    out
}

/// Pasos cuyo tamaño por request cambió mucho: no es mejora ni regresión, pero puede
/// explicar un cambio de latencia (una respuesta que creció, un archivo más grande).
fn size_changes(steps: &[StepDiff]) -> Vec<String> {
    let mut out = Vec::new();
    for s in steps {
        for (what, d) in [
            ("la respuesta", &s.received_per_request),
            ("el request", &s.sent_per_request),
        ] {
            let (Some(b), Some(n)) = (d.base, d.new) else {
                continue;
            };
            let ratio = n.max(1.0) / b.max(1.0);
            if (n - b).abs() >= SIZE_MIN_DELTA_BYTES
                && (ratio >= SIZE_MIN_RATIO || ratio <= 1.0 / SIZE_MIN_RATIO)
            {
                out.push(format!(
                    "`{}`: {what} pasó de {} a {} por request (×{:.1}); puede explicar parte del cambio de latencia",
                    s.name,
                    crate::upload::size(b as usize),
                    crate::upload::size(n as usize),
                    ratio
                ));
            }
        }
    }
    out
}

/// Segundos de carga (con iteraciones iniciadas) en las dos corridas. Deja afuera la
/// cola de `graceful_stop`.
fn paired_windows(base: &Run, new: &Run) -> Vec<u32> {
    let loaded = |r: &Run| -> Vec<u32> {
        r.series
            .points
            .iter()
            .filter(|p| p.started > 0)
            .map(|p| p.t)
            .collect()
    };
    let other = loaded(new);
    loaded(base)
        .into_iter()
        .filter(|t| other.contains(t))
        .collect()
}

fn totals(base: &Run, new: &Run) -> TotalsDiff {
    let dropped = |r: &Run| pct(r.totals.dropped, r.totals.planned);
    let errors = |r: &Run| pct(r.totals.failed + r.totals.non_2xx, r.totals.requests);
    TotalsDiff {
        iteration_rate: Delta::new(
            Some(base.totals.iteration_rate),
            Some(new.totals.iteration_rate),
        ),
        dropped_pct: Delta::new(dropped(base), dropped(new)),
        error_pct: Delta::new(errors(base), errors(new)),
    }
}

/// Una tasa en % que sube o baja al menos `RATE_MIN_DELTA_PP` puntos.
fn rate_change(
    regressions: &mut Vec<String>,
    improvements: &mut Vec<String>,
    what: &str,
    d: Delta,
) {
    let (Some(b), Some(n), Some(diff)) = (d.base, d.new, d.diff()) else {
        return;
    };
    if diff.abs() < RATE_MIN_DELTA_PP {
        return;
    }
    let line = format!("{what}: {}% → {}%", round1(b), round1(n));
    if diff > 0.0 {
        regressions.push(line);
    } else {
        improvements.push(line);
    }
}

fn steps(base: &Run, new: &Run, windows: Option<&[u32]>) -> Vec<StepDiff> {
    let mut names: Vec<&str> = base.steps.iter().map(|s| s.name.as_str()).collect();
    for s in &new.steps {
        if !names.contains(&s.name.as_str()) {
            names.push(&s.name);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let b = base.steps.iter().find(|s| s.name == name);
            let n = new.steps.iter().find(|s| s.name == name);
            let lat = |s: Option<&StepRun>, f: fn(&Percentiles) -> f64| {
                s.and_then(|s| s.latency_ms.as_ref()).map(f)
            };
            let p95 = Delta::new(lat(b, |p| p.p95), lat(n, |p| p.p95));
            let sign_test = windows.and_then(|w| {
                let (b, n) = (b?, n?);
                Some(sign_test(
                    step_points(base, &b.name),
                    step_points(new, &n.name),
                    w,
                ))
            });
            StepDiff {
                name: name.to_string(),
                source: n.or(b).and_then(|s| s.source.clone()),
                presence: match (b, n) {
                    (Some(_), Some(_)) => Presence::Both,
                    (Some(_), None) => Presence::OnlyBase,
                    _ => Presence::OnlyNew,
                },
                change: latency_change(p95, sign_test.as_ref(), windows.is_some()),
                sign_test,
                requests: Delta::new(b.map(|s| s.requests as f64), n.map(|s| s.requests as f64)),
                error_pct: Delta::new(
                    b.and_then(StepRun::error_pct),
                    n.and_then(StepRun::error_pct),
                ),
                p50_ms: Delta::new(lat(b, |p| p.p50), lat(n, |p| p.p50)),
                p95_ms: p95,
                p99_ms: Delta::new(lat(b, |p| p.p99), lat(n, |p| p.p99)),
                sent_per_request: Delta::new(
                    b.and_then(|s| s.per_request(s.bytes_sent)),
                    n.and_then(|s| s.per_request(s.bytes_sent)),
                ),
                received_per_request: Delta::new(
                    b.and_then(|s| s.per_request(s.bytes_received)),
                    n.and_then(|s| s.per_request(s.bytes_received)),
                ),
            }
        })
        .collect()
}

fn step_points<'a>(r: &'a Run, name: &str) -> &'a [StepPoint] {
    r.series
        .steps
        .iter()
        .find(|s| s.name == name)
        .map_or(&[], |s| s.points.as_slice())
}

/// Cuenta en cuántos segundos emparejados el p95 del paso bajó o subió.
fn sign_test(base: &[StepPoint], new: &[StepPoint], windows: &[u32]) -> SignTest {
    let p95 = |points: &[StepPoint], t: u32| {
        points
            .iter()
            .find(|p| p.t == t && p.requests > 0)
            .and_then(|p| p.p95)
    };
    let (mut better, mut worse) = (0, 0);
    for &t in windows {
        if let (Some(b), Some(n)) = (p95(base, t), p95(new, t)) {
            if n < b {
                better += 1;
            } else if n > b {
                worse += 1;
            }
        }
    }
    SignTest {
        windows: better + worse,
        better,
        worse,
        p_value: round4(binomial_two_sided(better, worse)),
    }
}

/// p-valor de dos colas de observar `max(a, b)` éxitos en `a + b` tiradas de una moneda.
fn binomial_two_sided(a: usize, b: usize) -> f64 {
    let n = a + b;
    if n == 0 {
        return 1.0;
    }
    let ln_fact: Vec<f64> = (0..=n)
        .scan(0.0, |acc, i| {
            if i > 0 {
                *acc += (i as f64).ln();
            }
            Some(*acc)
        })
        .collect();
    let tail: f64 = (a.max(b)..=n)
        .map(|i| (ln_fact[n] - ln_fact[i] - ln_fact[n - i] - n as f64 * LN_2).exp())
        .sum();
    (2.0 * tail).min(1.0)
}

fn latency_change(p95: Delta, test: Option<&SignTest>, paired: bool) -> Change {
    let (Some(b), Some(diff)) = (p95.base, p95.diff()) else {
        return Change::Same;
    };
    if diff.abs() < LATENCY_MIN_DELTA_MS || diff.abs() < b * LATENCY_MIN_RATIO {
        return Change::Same;
    }
    let direction = if diff > 0.0 {
        Change::Worse
    } else {
        Change::Better
    };
    if !paired {
        return direction;
    }
    let Some(t) = test else {
        return Change::Uncertain;
    };
    let agrees = match direction {
        Change::Worse => t.worse > t.better,
        _ => t.better > t.worse,
    };
    if t.windows >= SIGN_TEST_MIN_WINDOWS && t.p_value < SIGN_TEST_ALPHA && agrees {
        direction
    } else {
        Change::Uncertain
    }
}

fn step_changes(
    regressions: &mut Vec<String>,
    improvements: &mut Vec<String>,
    s: &StepDiff,
    paired: bool,
) {
    if matches!(s.change, Change::Better | Change::Worse) {
        let (Some(b), Some(n)) = (s.p95_ms.base, s.p95_ms.new) else {
            return;
        };
        let consistency = match (&s.sign_test, paired) {
            (Some(t), true) => {
                let agree = if s.change == Change::Worse {
                    t.worse
                } else {
                    t.better
                };
                format!(", en {agree} de {} segundos", t.windows)
            }
            _ => String::new(),
        };
        let line = format!(
            "`{}`: p95 {} → {} ms ({:+}%){consistency}",
            s.name,
            ms(b),
            ms(n),
            s.p95_ms.change_pct.unwrap_or_default()
        );
        if s.change == Change::Worse {
            regressions.push(line);
        } else {
            improvements.push(line);
        }
    }
    if let (Some(b), Some(n), Some(diff)) = (s.error_pct.base, s.error_pct.new, s.error_pct.diff())
        && diff.abs() >= RATE_MIN_DELTA_PP
    {
        let line = format!("`{}`: errores {}% → {}%", s.name, round1(b), round1(n));
        if diff > 0.0 {
            regressions.push(line);
        } else {
            improvements.push(line);
        }
    }
}

fn findings(
    base: &Run,
    new: &Run,
    regressions: &mut Vec<String>,
    improvements: &mut Vec<String>,
) -> Vec<FindingDiff> {
    let key = |f: &Finding| (f.id.clone(), f.step.clone());
    let base_by: BTreeMap<_, &Finding> = base.findings.iter().map(|f| (key(f), f)).collect();
    let new_by: BTreeMap<_, &Finding> = new.findings.iter().map(|f| (key(f), f)).collect();
    // `invalid_result` ya es un aviso; no cuenta como regresión ni mejora.
    let counts = |f: &Finding| f.id != "invalid_result" && f.severity != Severity::Info;

    let mut out = Vec::new();
    for f in &new.findings {
        match base_by.get(&key(f)) {
            Some(b) => {
                if f.id == "breaking_point" {
                    breaking_point_shift(b, f, regressions, improvements);
                }
                out.push(FindingDiff {
                    id: f.id.clone(),
                    step: f.step.clone(),
                    state: FindingState::Persists,
                    severity: f.severity,
                    base_title: Some(b.title.clone()),
                    new_title: Some(f.title.clone()),
                });
            }
            None => {
                if counts(f) {
                    regressions.push(format!("hallazgo nuevo: {}", f.title));
                }
                out.push(FindingDiff {
                    id: f.id.clone(),
                    step: f.step.clone(),
                    state: FindingState::New,
                    severity: f.severity,
                    base_title: None,
                    new_title: Some(f.title.clone()),
                });
            }
        }
    }
    for f in &base.findings {
        if new_by.contains_key(&key(f)) {
            continue;
        }
        if counts(f) {
            improvements.push(format!("resuelto: {}", f.title));
        }
        out.push(FindingDiff {
            id: f.id.clone(),
            step: f.step.clone(),
            state: FindingState::Resolved,
            severity: f.severity,
            base_title: Some(f.title.clone()),
            new_title: None,
        });
    }
    out
}

/// Si el punto de quiebre se movió al menos `BREAKING_POINT_MIN_RATIO`.
fn breaking_point_shift(
    base: &Finding,
    new: &Finding,
    regressions: &mut Vec<String>,
    improvements: &mut Vec<String>,
) {
    let rate = |f: &Finding| f.evidence["offered_iterations_per_s"].as_f64();
    let (Some(b), Some(n)) = (rate(base), rate(new)) else {
        return;
    };
    let line = format!("el punto de quiebre pasó de ~{b} a ~{n} it/s");
    if n <= b * (1.0 - BREAKING_POINT_MIN_RATIO) {
        regressions.push(line);
    } else if n >= b * (1.0 + BREAKING_POINT_MIN_RATIO) {
        improvements.push(line);
    }
}

fn thresholds(
    base: &Run,
    new: &Run,
    regressions: &mut Vec<String>,
    improvements: &mut Vec<String>,
) -> Vec<ThresholdDiff> {
    let mut names: Vec<&str> = base
        .thresholds
        .iter()
        .map(|t| t.threshold.as_str())
        .collect();
    for t in &new.thresholds {
        if !names.contains(&t.threshold.as_str()) {
            names.push(&t.threshold);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let b = base.thresholds.iter().find(|t| t.threshold == name);
            let n = new.thresholds.iter().find(|t| t.threshold == name);
            match (b.map(|t| t.passed), n.map(|t| t.passed)) {
                (Some(true), Some(false)) => {
                    regressions.push(format!("threshold `{name}` ahora falla"))
                }
                (Some(false), Some(true)) => {
                    improvements.push(format!("threshold `{name}` ahora pasa"))
                }
                _ => {}
            }
            ThresholdDiff {
                threshold: name.to_string(),
                base_passed: b.map(|t| t.passed),
                new_passed: n.map(|t| t.passed),
                base_actual: b.and_then(|t| t.actual),
                new_actual: n.and_then(|t| t.actual),
            }
        })
        .collect()
}

fn pct(part: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| (10_000.0 * part as f64 / total as f64).round() / 100.0)
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// Milisegundos para mostrar: un decimal debajo de 100 ms.
pub fn ms(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Una corrida con un paso `a` y `windows` segundos de p95 dados.
    fn run(id: &str, p95s: &[f64], findings: Value) -> Run {
        let points: Vec<Value> = p95s
            .iter()
            .enumerate()
            .map(|(t, p)| json!({ "t": t, "requests": 10, "errors": 0, "p50": p / 2.0, "p95": p, "p99": p }))
            .collect();
        let global: Vec<Value> = (0..p95s.len())
            .map(|t| json!({ "t": t, "started": 10, "dropped": 0, "requests": 10, "errors": 0, "p50": 1.0, "p95": 1.0, "p99": 1.0 }))
            .collect();
        let p95 = p95s.iter().copied().fold(0.0, f64::max);
        serde_json::from_value(json!({
            "run_id": id,
            "scenario": "s",
            "environment": "local",
            "load": { "executor": "constant_rate", "rate": "10/s" },
            "totals": { "planned": 100, "started": 100, "dropped": 0, "interrupted": 0, "requests": 100,
                        "failed": 0, "non_2xx": 0, "checks_passed": 0, "checks_failed": 0,
                        "iteration_rate": 10.0, "request_rate": 10.0 },
            "steps": [{ "name": "a", "requests": 100, "failed": 0, "non_2xx": 0,
                        "latency_ms": { "count": 100, "mean": p95, "p50": p95 / 2.0, "p90": p95, "p95": p95, "p99": p95, "p999": p95, "max": p95 } }],
            "findings": findings,
            "series": { "window_s": 1, "points": global, "steps": [{ "name": "a", "points": points }] },
        }))
        .unwrap()
    }

    #[test]
    fn warns_when_the_response_size_changes_a_lot() {
        let sized = |id: &str, received: u64| {
            let mut r = run(id, &[10.0; 8], json!([]));
            r.steps[0].bytes_sent = 100 * 300;
            r.steps[0].bytes_received = 100 * received;
            r
        };
        let c = compare(&sized("a", 4_000), &sized("b", 40_000)).unwrap();
        assert_eq!(c.steps[0].received_per_request.base, Some(4_000.0));
        assert_eq!(
            c.warnings,
            [
                "`a`: la respuesta pasó de 4 KB a 39 KB por request (×10.0); puede explicar parte del cambio de latencia"
            ]
        );
        // Un cambio chico, o una corrida sin bytes (anterior a ADR-013), no avisa.
        assert!(
            compare(&sized("a", 4_000), &sized("b", 4_500))
                .unwrap()
                .warnings
                .is_empty()
        );
        assert!(
            compare(&sized("a", 0), &sized("b", 40_000))
                .unwrap()
                .warnings
                .is_empty()
        );
    }

    fn breaking(rate: u64) -> Value {
        json!([{ "id": "breaking_point", "severity": "high", "title": format!("quiebre en ~{rate} it/s"),
                 "step": "a", "evidence": { "offered_iterations_per_s": rate }, "causes": [] }])
    }

    #[test]
    fn binomial_matches_known_values() {
        assert!((binomial_two_sided(6, 0) - 0.03125).abs() < 1e-9);
        assert!((binomial_two_sided(5, 5) - 1.0).abs() < 1e-9);
        assert!((binomial_two_sided(9, 1) - 0.021484375).abs() < 1e-9);
        // No se rompe con corridas largas.
        assert!(binomial_two_sided(3000, 600) < 1e-10);
    }

    #[test]
    fn identical_runs_are_the_same() {
        let p = [10.0, 11.0, 12.0, 10.0, 11.0, 12.0, 10.0, 11.0];
        let c = compare(&run("a", &p, json!([])), &run("b", &p, json!([]))).unwrap();
        assert_eq!(c.verdict, Verdict::Same, "{c:#?}");
        assert_eq!(c.steps[0].change, Change::Same);
        assert_eq!(c.paired_windows, 8);
    }

    #[test]
    fn consistently_slower_is_worse() {
        let base = [10.0, 11.0, 12.0, 10.0, 11.0, 12.0, 10.0, 11.0];
        let slower = base.map(|p| p + 20.0);
        let c = compare(&run("a", &base, json!([])), &run("b", &slower, json!([]))).unwrap();
        assert_eq!(c.steps[0].change, Change::Worse);
        assert_eq!(c.verdict, Verdict::Worse);
        assert!(
            c.regressions[0].contains("en 8 de 8 segundos"),
            "{:?}",
            c.regressions
        );
    }

    #[test]
    fn an_inconsistent_change_is_uncertain() {
        // El p95 agregado sube por dos segundos malos; el resto queda igual o mejor.
        let base = [10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0];
        let noisy = [9.0, 9.0, 9.0, 9.0, 11.0, 11.0, 80.0, 80.0];
        let c = compare(&run("a", &base, json!([])), &run("b", &noisy, json!([]))).unwrap();
        assert_eq!(c.steps[0].change, Change::Uncertain, "{c:#?}");
        assert!(c.regressions.is_empty());
    }

    #[test]
    fn different_load_compares_only_aggregates() {
        let base = [10.0; 8];
        let mut new = run("b", &base.map(|p| p * 3.0), json!([]));
        new.load = json!({ "executor": "constant_rate", "rate": "20/s" });
        let c = compare(&run("a", &base, json!([])), &new).unwrap();
        assert!(!c.same_load);
        assert_eq!(c.paired_windows, 0);
        assert!(
            c.warnings
                .iter()
                .any(|w| w.contains("la carga es distinta"))
        );
        assert_eq!(c.steps[0].change, Change::Worse);
    }

    #[test]
    fn breaking_point_moving_up_is_an_improvement() {
        let p = [10.0; 8];
        let c = compare(&run("a", &p, breaking(36)), &run("b", &p, breaking(120))).unwrap();
        assert_eq!(c.findings[0].state, FindingState::Persists);
        assert_eq!(c.verdict, Verdict::Better);
        assert!(
            c.improvements[0].contains("~36 a ~120"),
            "{:?}",
            c.improvements
        );
    }

    #[test]
    fn findings_appear_and_disappear() {
        let p = [10.0; 8];
        let c = compare(&run("a", &p, breaking(36)), &run("b", &p, json!([]))).unwrap();
        assert_eq!(c.findings[0].state, FindingState::Resolved);
        assert_eq!(c.verdict, Verdict::Better);

        let c = compare(&run("a", &p, json!([])), &run("b", &p, breaking(36))).unwrap();
        assert_eq!(c.findings[0].state, FindingState::New);
        assert_eq!(c.verdict, Verdict::Worse);
    }

    #[test]
    fn an_aborted_run_cannot_be_compared() {
        let p = [10.0; 8];
        let mut aborted = run("b", &p, json!([]));
        aborted.aborted = Some("setup falló".into());
        assert!(compare(&run("a", &p, json!([])), &aborted).is_err());
    }
}
