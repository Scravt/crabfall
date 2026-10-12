//! Agregación de métricas por paso, checks, thresholds y resultados de una corrida.

use std::{collections::BTreeMap, time::Duration};

use hdrhistogram::Histogram;
use serde::Serialize;

use crate::{
    engine::{
        http::{FailureKind, Timings},
        plan::{Plan, StepPlan},
    },
    scenario::Source,
    threshold::{Filter, Metric, Stat, ThresholdResult},
};

pub fn histogram() -> Histogram<u64> {
    // Microsegundos, de 1 µs a 1 h, 3 cifras significativas.
    Histogram::new_with_bounds(1, 3_600_000_000, 3).unwrap()
}

pub fn record(h: &mut Histogram<u64>, d: Duration) {
    h.saturating_record(d.as_micros().max(1) as u64);
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Percentiles {
    pub count: u64,
    pub mean: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub p999: f64,
    pub max: f64,
}

impl Percentiles {
    /// En milisegundos. `None` si no hay muestras.
    pub fn from(h: &Histogram<u64>) -> Option<Self> {
        if h.is_empty() {
            return None;
        }
        let ms = |us: u64| round(us as f64 / 1000.0);
        Some(Self {
            count: h.len(),
            mean: round(h.mean() / 1000.0),
            p50: ms(h.value_at_quantile(0.5)),
            p90: ms(h.value_at_quantile(0.9)),
            p95: ms(h.value_at_quantile(0.95)),
            p99: ms(h.value_at_quantile(0.99)),
            p999: ms(h.value_at_quantile(0.999)),
            max: ms(h.max()),
        })
    }
}

fn round(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Lo que produce cada request.
pub enum Outcome {
    Response { status: u16, timings: Timings },
    Failure { kind: FailureKind, message: String },
}

struct CheckCount {
    label: String,
    passed: u64,
    failed: u64,
    samples: Vec<String>,
}

struct StepStats {
    requests: u64,
    statuses: BTreeMap<u16, u64>,
    failures: BTreeMap<FailureKind, u64>,
    error_samples: Vec<String>,
    checks: Vec<CheckCount>,
    latency: Histogram<u64>,
    ttfb: Histogram<u64>,
    connect: Histogram<u64>,
    tls: Histogram<u64>,
    dns: Histogram<u64>,
}

impl StepStats {
    fn new(step: &StepPlan) -> Self {
        Self {
            requests: 0,
            statuses: BTreeMap::new(),
            failures: BTreeMap::new(),
            error_samples: Vec::new(),
            checks: step
                .checks
                .iter()
                .map(|c| CheckCount {
                    label: c.label.clone(),
                    passed: 0,
                    failed: 0,
                    samples: vec![],
                })
                .collect(),
            latency: histogram(),
            ttfb: histogram(),
            connect: histogram(),
            tls: histogram(),
            dns: histogram(),
        }
    }

    fn sample(&mut self, message: String) {
        if self.error_samples.len() < 3 && !self.error_samples.contains(&message) {
            self.error_samples.push(message);
        }
    }

    /// Requests sin respuesta, con `extract` fallido o con status >= 400.
    fn errors(&self) -> u64 {
        let failures: u64 = self.failures.values().sum();
        let http_errors: u64 = self
            .statuses
            .iter()
            .filter(|(c, _)| **c >= 400)
            .map(|(_, n)| n)
            .sum();
        failures + http_errors
    }
}

/// Acumula muestras durante la corrida.
pub struct Collector {
    steps: Vec<StepStats>,
}

impl Collector {
    pub fn new(plan: &Plan) -> Self {
        Self {
            steps: plan.steps().map(StepStats::new).collect(),
        }
    }

    /// `latency` se mide desde el instante planificado (primer paso) o desde el fin del paso anterior.
    pub fn record(&mut self, step: usize, latency: Duration, outcome: Outcome) {
        let s = &mut self.steps[step];
        s.requests += 1;
        match outcome {
            Outcome::Response { status, timings } => {
                *s.statuses.entry(status).or_default() += 1;
                record(&mut s.latency, latency);
                record(&mut s.ttfb, timings.ttfb);
                for (h, d) in [
                    (&mut s.connect, timings.connect),
                    (&mut s.tls, timings.tls),
                    (&mut s.dns, timings.dns),
                ] {
                    if let Some(d) = d {
                        record(h, d);
                    }
                }
            }
            Outcome::Failure { kind, message } => {
                *s.failures.entry(kind).or_default() += 1;
                s.sample(message);
            }
        }
    }

    /// La respuesta ya se registró con `record`; esto suma la falla del `extract`.
    pub fn record_extract_failure(&mut self, step: usize, message: String) {
        let s = &mut self.steps[step];
        *s.failures.entry(FailureKind::Extract).or_default() += 1;
        s.sample(message);
    }

    /// Un resultado por check del paso: `None` si pasó, o por qué falló.
    pub fn record_checks(&mut self, step: usize, results: &[Option<String>]) {
        for (count, result) in self.steps[step].checks.iter_mut().zip(results) {
            match result {
                None => count.passed += 1,
                Some(why) => {
                    count.failed += 1;
                    if count.samples.len() < 3 && !count.samples.contains(why) {
                        count.samples.push(why.clone());
                    }
                }
            }
        }
    }

    /// Evalúa los thresholds del plan sobre lo acumulado.
    pub fn thresholds(&self, plan: &Plan, duration_s: f64) -> Vec<ThresholdResult> {
        plan.thresholds
            .iter()
            .map(|t| {
                let selected: Vec<&StepStats> = plan
                    .steps()
                    .zip(&self.steps)
                    .filter(|(p, _)| match &t.filter {
                        Filter::All => true,
                        Filter::Step(name) => &p.name == name,
                        Filter::Tag(tag) => p.tags.contains(tag),
                    })
                    .map(|(_, s)| s)
                    .collect();
                let actual = measure(t.metric, &selected, duration_s);
                ThresholdResult {
                    threshold: t.label.clone(),
                    op: t.op,
                    limit: t.limit,
                    actual: actual.map(round),
                    unit: t.unit(),
                    passed: actual.is_some_and(|a| t.passes(a)),
                }
            })
            .collect()
    }

    pub fn finish(self, plan: &Plan) -> Vec<StepResult> {
        plan.steps()
            .zip(self.steps)
            .map(|(p, s)| {
                let non_2xx = s
                    .statuses
                    .iter()
                    .filter(|(c, _)| !(200..300).contains(*c))
                    .map(|(_, n)| n)
                    .sum();
                StepResult {
                    name: p.name.clone(),
                    flow: p.flow.clone(),
                    method: p.method.to_string(),
                    url: p.url(),
                    tags: p.tags.clone(),
                    source: p.source.clone(),
                    requests: s.requests,
                    failed: s.failures.values().sum(),
                    non_2xx,
                    status_codes: s
                        .statuses
                        .iter()
                        .map(|(c, n)| (c.to_string(), *n))
                        .collect(),
                    failures: s.failures,
                    error_samples: s.error_samples,
                    checks: s
                        .checks
                        .into_iter()
                        .map(|c| CheckResult {
                            check: c.label,
                            passed: c.passed,
                            failed: c.failed,
                            samples: c.samples,
                        })
                        .collect(),
                    latency_ms: Percentiles::from(&s.latency),
                    ttfb_ms: Percentiles::from(&s.ttfb),
                    connect_ms: Percentiles::from(&s.connect),
                    tls_ms: Percentiles::from(&s.tls),
                    dns_ms: Percentiles::from(&s.dns),
                }
            })
            .collect()
    }
}

/// Valor de la métrica sobre los pasos elegidos. `None` si no hubo datos.
fn measure(metric: Metric, steps: &[&StepStats], duration_s: f64) -> Option<f64> {
    let requests: u64 = steps.iter().map(|s| s.requests).sum();
    match metric {
        Metric::Latency(stat) => {
            let mut merged = histogram();
            for s in steps {
                merged.add(&s.latency).ok()?;
            }
            if merged.is_empty() {
                return None;
            }
            let us = match stat {
                Stat::Quantile(p) => merged.value_at_quantile(p / 100.0) as f64,
                Stat::Avg => merged.mean(),
                Stat::Max => merged.max() as f64,
            };
            Some(us / 1000.0)
        }
        Metric::ErrorRate => (requests > 0).then(|| {
            100.0 * steps.iter().map(|s| s.errors()).sum::<u64>() as f64 / requests as f64
        }),
        Metric::Rps => (duration_s > 0.0).then(|| requests as f64 / duration_s),
        Metric::ChecksPassRate => {
            let (passed, total) = steps
                .iter()
                .flat_map(|s| &s.checks)
                .fold((0, 0), |(p, t), c| (p + c.passed, t + c.passed + c.failed));
            (total > 0).then(|| 100.0 * passed as f64 / total as f64)
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub check: String,
    pub passed: u64,
    pub failed: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct StepResult {
    pub name: String,
    pub flow: String,
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    pub requests: u64,
    /// Requests sin respuesta (error de red, TLS, timeout) o con `extract` fallido.
    pub failed: u64,
    /// Respuestas con código fuera de 2xx.
    pub non_2xx: u64,
    pub status_codes: BTreeMap<String, u64>,
    pub failures: BTreeMap<FailureKind, u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub error_samples: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckResult>,
    pub latency_ms: Option<Percentiles>,
    pub ttfb_ms: Option<Percentiles>,
    pub connect_ms: Option<Percentiles>,
    pub tls_ms: Option<Percentiles>,
    pub dns_ms: Option<Percentiles>,
}

#[derive(Debug, Default, Serialize)]
pub struct Totals {
    /// Iteraciones que el scheduler planificó.
    pub planned: u64,
    /// Iteraciones que arrancaron.
    pub started: u64,
    /// Iteraciones descartadas porque no había VUs libres.
    pub dropped: u64,
    /// Iteraciones cortadas por `graceful_stop`.
    pub interrupted: u64,
    pub requests: u64,
    pub failed: u64,
    pub non_2xx: u64,
    pub checks_passed: u64,
    pub checks_failed: u64,
    /// Iteraciones iniciadas por segundo durante el período planificado.
    pub iteration_rate: f64,
    pub request_rate: f64,
}

#[derive(Debug, Serialize)]
pub struct RunResult {
    /// Verdadero si la corrida terminó y pasaron todos los thresholds.
    pub passed: bool,
    pub duration_s: f64,
    /// Por qué no se generó carga, si el `setup` falló.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aborted: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<SetupStep>,
    pub totals: Totals,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<ThresholdResult>,
    pub scheduler_lag_ms: Option<Percentiles>,
    pub steps: Vec<StepResult>,
    pub warnings: Vec<String>,
}

/// Un paso de `setup`. No entra en las métricas de carga.
#[derive(Debug, Serialize)]
pub struct SetupStep {
    pub name: String,
    pub status: Option<u16>,
    pub duration_ms: f64,
    /// Nombres de las variables extraídas (sin sus valores).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extracted: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RunResult {
    /// Corrida abortada antes de generar carga.
    pub fn aborted(plan: &Plan, setup: Vec<SetupStep>, reason: String) -> Self {
        Self {
            passed: false,
            duration_s: 0.0,
            aborted: Some(reason),
            setup,
            totals: Totals::default(),
            thresholds: vec![],
            scheduler_lag_ms: None,
            steps: Collector::new(plan).finish(plan),
            warnings: vec![],
        }
    }
}
