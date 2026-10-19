//! Agregación de métricas por paso, checks, thresholds y resultados de una corrida.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

/// Una ventana de 1 s de la corrida.
struct Window {
    started: u64,
    dropped: u64,
    requests: u64,
    errors: u64,
    latency: Option<Histogram<u64>>,
    steps: Vec<WindowStep>,
}

#[derive(Default)]
struct WindowStep {
    requests: u64,
    errors: u64,
    latency: Option<Histogram<u64>>,
}

impl Window {
    fn new(steps: usize) -> Self {
        Self {
            started: 0,
            dropped: 0,
            requests: 0,
            errors: 0,
            latency: None,
            steps: (0..steps).map(|_| WindowStep::default()).collect(),
        }
    }
}

/// Histograma de ventana: 2 cifras significativas alcanzan y ocupan mucho menos.
fn window_record(h: &mut Option<Histogram<u64>>, d: Duration) {
    let h = h.get_or_insert_with(|| Histogram::new_with_bounds(1, 3_600_000_000, 2).unwrap());
    record(h, d);
}

/// Acumula muestras durante la corrida.
pub struct Collector {
    steps: Vec<StepStats>,
    start: Instant,
    windows: Vec<Window>,
}

impl Collector {
    /// `start` es el inicio de la carga: la ventana 0 empieza ahí.
    pub fn new(plan: &Plan, start: Instant) -> Self {
        Self {
            steps: plan.steps().map(StepStats::new).collect(),
            start,
            windows: Vec::new(),
        }
    }

    fn window(&mut self, at: Instant) -> &mut Window {
        let i = at.saturating_duration_since(self.start).as_secs() as usize;
        let steps = self.steps.len();
        if self.windows.len() <= i {
            self.windows.resize_with(i + 1, || Window::new(steps));
        }
        &mut self.windows[i]
    }

    pub fn record_started(&mut self, at: Instant) {
        self.window(at).started += 1;
    }

    pub fn record_dropped(&mut self, at: Instant) {
        self.window(at).dropped += 1;
    }

    /// `latency` se mide desde el instante planificado (primer paso) o desde el fin del paso anterior.
    /// `at` es cuándo terminó el request.
    pub fn record(&mut self, step: usize, at: Instant, latency: Duration, outcome: Outcome) {
        let is_error = match &outcome {
            Outcome::Response { status, .. } => *status >= 400,
            Outcome::Failure { .. } => true,
        };
        let has_latency = matches!(outcome, Outcome::Response { .. });
        let w = self.window(at);
        w.requests += 1;
        w.steps[step].requests += 1;
        if is_error {
            w.errors += 1;
            w.steps[step].errors += 1;
        }
        if has_latency {
            window_record(&mut w.latency, latency);
            window_record(&mut w.steps[step].latency, latency);
        }

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
    pub fn record_extract_failure(&mut self, step: usize, at: Instant, message: String) {
        let w = self.window(at);
        w.errors += 1;
        w.steps[step].errors += 1;
        let s = &mut self.steps[step];
        *s.failures.entry(FailureKind::Extract).or_default() += 1;
        s.sample(message);
    }

    /// Serie por segundo, global y por paso.
    pub fn series(&self, plan: &Plan) -> Series {
        let quantiles = |h: &Option<Histogram<u64>>| match h {
            Some(h) if !h.is_empty() => {
                let ms = |q: f64| Some(round(h.value_at_quantile(q) as f64 / 1000.0));
                (ms(0.5), ms(0.95), ms(0.99))
            }
            _ => (None, None, None),
        };
        let points = self
            .windows
            .iter()
            .enumerate()
            .map(|(t, w)| {
                let (p50, p95, p99) = quantiles(&w.latency);
                Point {
                    t: t as u32,
                    started: w.started,
                    dropped: w.dropped,
                    requests: w.requests,
                    errors: w.errors,
                    p50,
                    p95,
                    p99,
                }
            })
            .collect();
        let steps = plan
            .steps()
            .map(|p| StepSeries {
                name: p.name.clone(),
                points: self
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(t, w)| {
                        let s = &w.steps[p.id];
                        let (p50, p95, p99) = quantiles(&s.latency);
                        StepPoint {
                            t: t as u32,
                            requests: s.requests,
                            errors: s.errors,
                            p50,
                            p95,
                            p99,
                        }
                    })
                    .collect(),
            })
            .collect();
        Series {
            window_s: 1,
            points,
            steps,
        }
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

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
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
    /// La preparación de los VUs (`vu_setup`), si el escenario la tiene.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vu_setup: Option<VuSetupResult>,
    /// Las renovaciones de sesión (`vu_refresh`), si el escenario las tiene.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vu_refresh: Option<VuRefreshResult>,
    pub totals: Totals,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<ThresholdResult>,
    pub scheduler_lag_ms: Option<Percentiles>,
    pub steps: Vec<StepResult>,
    pub warnings: Vec<String>,
    /// Hallazgos del diagnóstico, de más a menos grave.
    pub findings: Vec<crate::diagnose::Finding>,
    /// Serie por segundo: la materia prima del diagnóstico.
    pub series: Series,
}

/// Cómo les fue a los VUs con `vu_setup`. No entra en las métricas de carga.
#[derive(Debug, Serialize)]
pub struct VuSetupResult {
    /// VUs que lo intentaron.
    pub vus: u32,
    /// VUs que no lo completaron y quedaron afuera de la carga.
    pub failed: u32,
    pub duration_ms: f64,
    pub steps: Vec<VuSetupStep>,
}

#[derive(Debug, Serialize)]
pub struct VuSetupStep {
    pub name: String,
    pub requests: u64,
    pub failed: u64,
    pub latency_ms: Option<Percentiles>,
    /// Hasta tres motivos distintos de falla.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Cómo les fue a los VUs con `vu_refresh`. No entra en las métricas de carga.
#[derive(Debug, Serialize)]
pub struct VuRefreshResult {
    /// Cada cuánto renueva cada VU.
    pub every_s: f64,
    /// Renovaciones hechas o intentadas, sumando todos los VUs.
    pub refreshes: u64,
    /// VUs que fallaron al renovar y dejaron de iterar.
    pub failed_vus: u64,
    pub steps: Vec<VuSetupStep>,
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
            vu_setup: None,
            vu_refresh: None,
            totals: Totals::default(),
            thresholds: vec![],
            scheduler_lag_ms: None,
            steps: Collector::new(plan, Instant::now()).finish(plan),
            warnings: vec![],
            findings: vec![],
            series: Series::default(),
        }
    }
}

/// Serie por segundo. `t` es el segundo desde el inicio de la carga.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Series {
    pub window_s: u32,
    pub points: Vec<Point>,
    pub steps: Vec<StepSeries>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    pub t: u32,
    /// Iteraciones iniciadas en la ventana.
    pub started: u64,
    /// Iteraciones descartadas por falta de VUs.
    pub dropped: u64,
    /// Requests terminados en la ventana.
    pub requests: u64,
    /// Sin respuesta, con status >= 400 o con `extract` fallido.
    pub errors: u64,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepSeries {
    pub name: String,
    pub points: Vec<StepPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepPoint {
    pub t: u32,
    pub requests: u64,
    pub errors: u64,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
}
