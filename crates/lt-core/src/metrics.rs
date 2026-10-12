//! Agregación de métricas por paso y resultados de una corrida.

use std::{collections::BTreeMap, time::Duration};

use hdrhistogram::Histogram;
use serde::Serialize;

use crate::{
    engine::{
        http::{FailureKind, Timings},
        plan::Plan,
    },
    scenario::Source,
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

pub struct StepStats {
    requests: u64,
    statuses: BTreeMap<u16, u64>,
    failures: BTreeMap<FailureKind, u64>,
    error_samples: Vec<String>,
    latency: Histogram<u64>,
    ttfb: Histogram<u64>,
    connect: Histogram<u64>,
    tls: Histogram<u64>,
    dns: Histogram<u64>,
}

impl StepStats {
    fn new() -> Self {
        Self {
            requests: 0,
            statuses: BTreeMap::new(),
            failures: BTreeMap::new(),
            error_samples: Vec::new(),
            latency: histogram(),
            ttfb: histogram(),
            connect: histogram(),
            tls: histogram(),
            dns: histogram(),
        }
    }
}

/// Acumula muestras durante la corrida.
pub struct Collector {
    steps: Vec<StepStats>,
}

impl Collector {
    pub fn new(plan: &Plan) -> Self {
        Self {
            steps: plan.steps().map(|_| StepStats::new()).collect(),
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
                if s.error_samples.len() < 3 && !s.error_samples.contains(&message) {
                    s.error_samples.push(message);
                }
            }
        }
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
    /// Requests sin respuesta (error de red, TLS, timeout).
    pub failed: u64,
    /// Respuestas con código fuera de 2xx.
    pub non_2xx: u64,
    pub status_codes: BTreeMap<String, u64>,
    pub failures: BTreeMap<FailureKind, u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub error_samples: Vec<String>,
    pub latency_ms: Option<Percentiles>,
    pub ttfb_ms: Option<Percentiles>,
    pub connect_ms: Option<Percentiles>,
    pub tls_ms: Option<Percentiles>,
    pub dns_ms: Option<Percentiles>,
}

#[derive(Debug, Serialize)]
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
    /// Iteraciones iniciadas por segundo durante el período planificado.
    pub iteration_rate: f64,
    pub request_rate: f64,
}

#[derive(Debug, Serialize)]
pub struct RunResult {
    pub duration_s: f64,
    pub totals: Totals,
    pub scheduler_lag_ms: Option<Percentiles>,
    pub steps: Vec<StepResult>,
    pub warnings: Vec<String>,
}
