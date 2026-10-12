//! Motor: scheduler de modelo abierto (ADR-003).

pub mod http;
pub mod plan;

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinSet,
    time::{sleep_until, timeout},
};

use crate::{
    metrics::{Collector, Outcome, Percentiles, RunResult, Totals, histogram, record},
    scenario::LoadPlan,
};
use http::{Client, FailureKind};
use plan::Plan;

/// Contadores en vivo, para mostrar progreso.
#[derive(Default)]
pub struct Progress {
    pub started: AtomicU64,
    pub dropped: AtomicU64,
    pub requests: AtomicU64,
    pub failed: AtomicU64,
}

struct Sample {
    step: usize,
    latency: Duration,
    outcome: Outcome,
}

/// Ejecuta el plan. No verifica aprobación: eso es responsabilidad de quien llama.
pub async fn run(plan: Arc<Plan>, progress: Arc<Progress>) -> RunResult {
    let LoadPlan::ConstantRate {
        rate,
        duration,
        max_vus,
        graceful_stop,
    } = plan.load;
    let per_second = rate.per_second();
    let planned = (per_second * duration.as_secs_f64()).floor() as u64;
    let interval = Duration::from_secs_f64(1.0 / per_second);

    let client = Arc::new(Client::new());
    let vus = Arc::new(Semaphore::new(max_vus as usize));
    let (tx, mut rx) = mpsc::unbounded_channel::<Sample>();

    let collector_plan = plan.clone();
    let collector = tokio::spawn(async move {
        let mut c = Collector::new(&collector_plan);
        while let Some(s) = rx.recv().await {
            c.record(s.step, s.latency, s.outcome);
        }
        c
    });

    let mut picker = WeightedPicker::new(plan.flows.iter().map(|f| f.weight).collect());
    let mut lag = histogram();
    let mut tasks = JoinSet::new();
    let start = Instant::now() + Duration::from_millis(50);
    let mut last_dispatch = start;

    for i in 0..planned {
        let intended = start + interval.mul_f64(i as f64);
        sleep_until(intended.into()).await;
        last_dispatch = Instant::now();
        record(&mut lag, last_dispatch.saturating_duration_since(intended));
        while tasks.try_join_next().is_some() {}

        let flow = picker.next();
        let Ok(permit) = vus.clone().try_acquire_owned() else {
            progress.dropped.fetch_add(1, Relaxed);
            continue;
        };
        progress.started.fetch_add(1, Relaxed);
        let (plan, client, tx, progress) =
            (plan.clone(), client.clone(), tx.clone(), progress.clone());
        tasks.spawn(async move {
            iteration(&plan, flow, &client, &tx, &progress, intended).await;
            drop(permit);
        });
    }
    let schedule_secs = (last_dispatch - start)
        .as_secs_f64()
        .max(interval.as_secs_f64());

    let drained = timeout(graceful_stop, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_ok();
    let interrupted = if drained { 0 } else { tasks.len() as u64 };
    tasks.shutdown().await;
    drop(tx);
    let steps = collector.await.expect("el colector no falla").finish(&plan);

    let started = progress.started.load(Relaxed);
    let dropped = progress.dropped.load(Relaxed);
    let requests: u64 = steps.iter().map(|s| s.requests).sum();
    let totals = Totals {
        planned,
        started,
        dropped,
        interrupted,
        requests,
        failed: steps.iter().map(|s| s.failed).sum(),
        non_2xx: steps.iter().map(|s| s.non_2xx).sum(),
        iteration_rate: round1(started as f64 / schedule_secs),
        request_rate: round1(requests as f64 / start.elapsed().as_secs_f64()),
    };
    let scheduler_lag = Percentiles::from(&lag);
    let warnings = warnings(&totals, scheduler_lag.as_ref());

    RunResult {
        duration_s: round1(start.elapsed().as_secs_f64()),
        totals,
        scheduler_lag_ms: scheduler_lag,
        steps,
        warnings,
    }
}

/// Ejecuta los pasos de un flow en orden. Si un paso falla sin respuesta, la iteración se corta.
async fn iteration(
    plan: &Plan,
    flow: usize,
    client: &Client,
    tx: &mpsc::UnboundedSender<Sample>,
    progress: &Progress,
    intended: Instant,
) {
    let mut from = intended;
    for step in &plan.flows[flow].steps {
        let result = timeout(step.timeout, client.send(&step.origin, || step.request())).await;
        let done = Instant::now();
        let outcome = match result {
            Ok(Ok(r)) => Outcome::Response {
                status: r.status,
                timings: r.timings,
            },
            Ok(Err(f)) => Outcome::Failure {
                kind: f.kind,
                message: f.message,
            },
            Err(_) => Outcome::Failure {
                kind: FailureKind::Timeout,
                message: format!(
                    "sin respuesta en {}",
                    crate::units::HumanDuration(step.timeout)
                ),
            },
        };
        let failed = matches!(outcome, Outcome::Failure { .. });
        progress.requests.fetch_add(1, Relaxed);
        if failed {
            progress.failed.fetch_add(1, Relaxed);
        }
        let _ = tx.send(Sample {
            step: step.id,
            latency: done.saturating_duration_since(from),
            outcome,
        });
        if failed {
            return;
        }
        from = done;
    }
}

fn warnings(t: &Totals, lag: Option<&Percentiles>) -> Vec<String> {
    let mut out = Vec::new();
    if t.dropped > 0 {
        let pct = 100.0 * t.dropped as f64 / t.planned.max(1) as f64;
        out.push(format!(
            "{} iteraciones descartadas ({pct:.1}%) por falta de VUs libres: el servidor no da abasto \
             a esta tasa o max_vus es bajo. Las latencias medidas subestiman la real.",
            t.dropped
        ));
    }
    if lag.is_some_and(|l| l.p99 > 5.0) {
        out.push(
            "el scheduler llegó tarde (p99 > 5 ms): el generador puede ser el cuello de botella; \
             los resultados no son confiables."
                .into(),
        );
    }
    if t.interrupted > 0 {
        out.push(format!(
            "{} iteraciones cortadas por graceful_stop.",
            t.interrupted
        ));
    }
    out
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Round-robin ponderado suave: reparte los flows según su peso, de forma determinista.
struct WeightedPicker {
    weights: Vec<i64>,
    current: Vec<i64>,
    total: i64,
}

impl WeightedPicker {
    fn new(weights: Vec<u32>) -> Self {
        let weights: Vec<i64> = weights.into_iter().map(i64::from).collect();
        Self {
            current: vec![0; weights.len()],
            total: weights.iter().sum(),
            weights,
        }
    }

    fn next(&mut self) -> usize {
        for (c, w) in self.current.iter_mut().zip(&self.weights) {
            *c += w;
        }
        let best = (0..self.current.len())
            .max_by_key(|&i| (self.current[i], -(i as i64)))
            .unwrap();
        self.current[best] -= self.total;
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_respects_weights_and_interleaves() {
        let mut p = WeightedPicker::new(vec![7, 3]);
        let picks: Vec<_> = (0..10).map(|_| p.next()).collect();
        assert_eq!(picks.iter().filter(|&&f| f == 0).count(), 7);
        assert_ne!(picks[..3], [1, 1, 1]);
    }
}
