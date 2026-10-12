//! Motor: scheduler de modelo abierto (ADR-003).

pub mod http;
pub mod plan;

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use hdrhistogram::Histogram;
use serde_json::Value;
use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinSet,
    time::{sleep_until, timeout},
};

use crate::{
    load::LoadPlan,
    metrics::{Collector, Outcome, Percentiles, RunResult, SetupStep, Totals, histogram, record},
};
use http::{Body, Client, Failure, FailureKind, Response};
use hyper::Request;
use plan::{Plan, StepPlan, Vars};

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
    /// La respuesta llegó, pero un `extract` falló.
    extract_error: Option<String>,
    checks: Vec<Option<String>>,
}

/// Resultado de ejecutar un paso: la respuesta (o la falla) y lo extraído.
pub struct StepRun {
    pub result: Result<Response, Failure>,
    pub extracted: Vec<(String, Value)>,
    pub extract_error: Option<String>,
    /// Por check: `None` si pasó, o por qué falló. Vacío si no hubo respuesta.
    pub checks: Vec<Option<String>>,
}

impl StepRun {
    /// Sin respuesta o con un `extract` fallido: los pasos siguientes no pueden seguir.
    pub fn broken(&self) -> bool {
        self.result.is_err() || self.extract_error.is_some()
    }
}

/// Arma el request con las variables visibles, lo envía y aplica los `extract`.
pub async fn run_step(
    client: &Client,
    plan: &Plan,
    step: &StepPlan,
    setup: &HashMap<String, Value>,
    locals: &HashMap<String, Value>,
) -> StepRun {
    let request = step.request(&Vars {
        plan,
        setup,
        locals,
    });
    let result = send_step(client, step, request).await;
    let mut extracted = Vec::new();
    let mut extract_error = None;
    let mut checks = Vec::new();
    if let Ok(response) = &result {
        let scope = Vars {
            plan,
            setup,
            locals,
        };
        checks = step
            .checks
            .iter()
            .map(|c| {
                c.evaluate(response.status, &response.headers, &response.body, &scope)
                    .err()
            })
            .collect();
        for (name, extractor) in &step.extract {
            match extractor.apply(&response.headers, &response.body) {
                Ok(value) => extracted.push((name.clone(), value)),
                Err(e) => {
                    extract_error = Some(format!("extract.{name}: {e}"));
                    break;
                }
            }
        }
    }
    StepRun {
        result,
        extracted,
        extract_error,
        checks,
    }
}

/// Corre `setup` en orden. Devuelve las variables extraídas, el detalle de cada paso y,
/// si algo falló, por qué se aborta la corrida.
pub async fn run_setup(
    client: &Client,
    plan: &Plan,
) -> (HashMap<String, Value>, Vec<SetupStep>, Option<String>) {
    let mut vars = HashMap::new();
    let mut steps = Vec::new();
    for step in &plan.setup {
        let start = Instant::now();
        let run = run_step(client, plan, step, &vars, &HashMap::new()).await;
        let status = run.result.as_ref().ok().map(|r| r.status);
        let problem = match (&run.result, &run.extract_error) {
            (Err(f), _) => Some(f.message.clone()),
            (Ok(r), _) if r.status >= 400 => Some(format!("respondió {}", r.status)),
            (_, Some(e)) => Some(e.clone()),
            _ => step.checks.iter().zip(&run.checks).find_map(|(c, r)| {
                r.as_ref()
                    .map(|why| format!("el check `{}` falló: {why}", c.label))
            }),
        };
        steps.push(SetupStep {
            name: step.name.clone(),
            status,
            duration_ms: round3(start.elapsed().as_secs_f64() * 1000.0),
            extracted: run.extracted.iter().map(|(n, _)| n.clone()).collect(),
            error: problem.clone(),
        });
        if let Some(problem) = problem {
            return (
                vars,
                steps,
                Some(format!("el setup falló en `{}`: {problem}", step.name)),
            );
        }
        vars.extend(run.extracted);
    }
    (vars, steps, None)
}

/// Lo que comparten las iteraciones.
#[derive(Clone)]
struct Shared {
    plan: Arc<Plan>,
    client: Arc<Client>,
    setup: Arc<HashMap<String, Value>>,
    tx: mpsc::UnboundedSender<Sample>,
    progress: Arc<Progress>,
}

impl Shared {
    async fn iteration(&self, flow: usize, intended: Instant) {
        iteration(
            &self.plan,
            flow,
            &self.client,
            &self.setup,
            &self.tx,
            &self.progress,
            intended,
        )
        .await;
    }
}

/// Lo que devuelve cada modelo de carga.
struct Dispatch {
    /// Iteraciones del calendario; `None` en el modelo cerrado (se cuentan las iniciadas).
    planned: Option<u64>,
    /// Segundos durante los que se iniciaron iteraciones.
    schedule_secs: f64,
    lag: Option<Histogram<u64>>,
    interrupted: u64,
    /// Iteraciones que esperaron por `limits.max_rps` (modelo cerrado).
    paced: u64,
}

/// Ejecuta el plan. No verifica aprobación: eso es responsabilidad de quien llama.
pub async fn run(plan: Arc<Plan>, progress: Arc<Progress>) -> RunResult {
    let client = Arc::new(Client::new());
    let (setup_vars, setup, aborted) = run_setup(&client, &plan).await;
    if let Some(reason) = aborted {
        return RunResult::aborted(&plan, setup, reason);
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Sample>();
    let collector_plan = plan.clone();
    let collector = tokio::spawn(async move {
        let mut c = Collector::new(&collector_plan);
        while let Some(s) = rx.recv().await {
            c.record(s.step, s.latency, s.outcome);
            c.record_checks(s.step, &s.checks);
            if let Some(e) = s.extract_error {
                c.record_extract_failure(s.step, e);
            }
        }
        c
    });

    let shared = Shared {
        plan: plan.clone(),
        client,
        setup: Arc::new(setup_vars),
        tx,
        progress: progress.clone(),
    };
    let start = Instant::now() + Duration::from_millis(50);
    let dispatch = match &plan.load {
        LoadPlan::Open { .. } => open_model(shared, start).await,
        LoadPlan::Closed { vus, duration, .. } => {
            closed_model(shared, start, *vus, *duration).await
        }
    };

    let elapsed = start.elapsed().as_secs_f64();
    let collector = collector.await.expect("el colector no falla");
    let thresholds = collector.thresholds(&plan, elapsed);
    let steps = collector.finish(&plan);

    let started = progress.started.load(Relaxed);
    let requests: u64 = steps.iter().map(|s| s.requests).sum();
    let totals = Totals {
        planned: dispatch.planned.unwrap_or(started),
        started,
        dropped: progress.dropped.load(Relaxed),
        interrupted: dispatch.interrupted,
        requests,
        failed: steps.iter().map(|s| s.failed).sum(),
        non_2xx: steps.iter().map(|s| s.non_2xx).sum(),
        checks_passed: steps.iter().flat_map(|s| &s.checks).map(|c| c.passed).sum(),
        checks_failed: steps.iter().flat_map(|s| &s.checks).map(|c| c.failed).sum(),
        iteration_rate: round1(started as f64 / dispatch.schedule_secs),
        request_rate: round1(requests as f64 / elapsed),
    };
    let scheduler_lag = dispatch.lag.as_ref().and_then(Percentiles::from);
    let mut warnings = warnings(&totals, scheduler_lag.as_ref());
    if dispatch.paced > 0 {
        warnings.push(format!(
            "{} iteraciones esperaron para no superar limits.max_rps ({}/s): la carga quedó limitada por el entorno.",
            dispatch.paced,
            plan.request_cap.unwrap_or_default()
        ));
    }

    RunResult {
        passed: thresholds.iter().all(|t| t.passed),
        duration_s: round1(elapsed),
        aborted: None,
        setup,
        totals,
        thresholds,
        scheduler_lag_ms: scheduler_lag,
        steps,
        warnings,
    }
}

/// Modelo abierto: cada iteración arranca en su instante planificado; si no hay VU
/// libre, se descarta (nunca se demora).
async fn open_model(shared: Shared, start: Instant) -> Dispatch {
    let plan = shared.plan.clone();
    let vus = Arc::new(Semaphore::new(plan.load.concurrency() as usize));
    let mut picker = WeightedPicker::new(plan.flows.iter().map(|f| f.weight).collect());
    let mut lag = histogram();
    let mut tasks = JoinSet::new();
    let mut planned = 0;
    let mut last_dispatch = start;

    for offset in plan.load.schedule() {
        planned += 1;
        let intended = start + offset;
        sleep_until(intended.into()).await;
        last_dispatch = Instant::now();
        record(&mut lag, last_dispatch.saturating_duration_since(intended));
        while tasks.try_join_next().is_some() {}

        let flow = picker.next();
        let Ok(permit) = vus.clone().try_acquire_owned() else {
            shared.progress.dropped.fetch_add(1, Relaxed);
            continue;
        };
        shared.progress.started.fetch_add(1, Relaxed);
        let shared = shared.clone();
        tasks.spawn(async move {
            shared.iteration(flow, intended).await;
            drop(permit);
        });
    }
    let schedule_secs = (last_dispatch - start).as_secs_f64().max(1e-3);
    drop(shared);
    let interrupted = drain(&mut tasks, plan.load.graceful_stop()).await;
    Dispatch {
        planned: Some(planned),
        schedule_secs,
        lag: Some(lag),
        interrupted,
        paced: 0,
    }
}

/// Modelo cerrado: `vus` usuarios en loop hasta `duration`. Si el entorno tiene
/// `max_rps`, un pacer espacia los inicios para no superarlo.
async fn closed_model(shared: Shared, start: Instant, vus: u32, duration: Duration) -> Dispatch {
    let plan = shared.plan.clone();
    let deadline = start + duration;
    let picker = Arc::new(std::sync::Mutex::new(WeightedPicker::new(
        plan.flows.iter().map(|f| f.weight).collect(),
    )));
    let pacer = plan.request_cap.map(|cap| {
        let max_steps = plan
            .flows
            .iter()
            .map(|f| f.steps.len())
            .max()
            .unwrap_or(1)
            .max(1);
        Arc::new(Pacer::new(
            Duration::from_secs_f64(max_steps as f64 / cap),
            start,
        ))
    });

    let mut tasks = JoinSet::new();
    for _ in 0..vus {
        let (shared, picker, pacer) = (shared.clone(), picker.clone(), pacer.clone());
        tasks.spawn(async move {
            sleep_until(start.into()).await;
            loop {
                if let Some(p) = &pacer
                    && !p.wait(deadline).await
                {
                    break;
                }
                if Instant::now() >= deadline {
                    break;
                }
                let flow = picker.lock().unwrap().next();
                shared.progress.started.fetch_add(1, Relaxed);
                shared.iteration(flow, Instant::now()).await;
            }
        });
    }
    drop(shared);
    sleep_until(deadline.into()).await;
    let interrupted = drain(&mut tasks, plan.load.graceful_stop()).await;
    Dispatch {
        planned: None,
        schedule_secs: duration.as_secs_f64(),
        lag: None,
        interrupted,
        paced: pacer.map_or(0, |p| p.delayed.load(Relaxed)),
    }
}

/// Espera a que terminen las tareas, hasta `graceful_stop`; corta las que queden.
async fn drain(tasks: &mut JoinSet<()>, graceful_stop: Duration) -> u64 {
    let drained = timeout(graceful_stop, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_ok();
    let interrupted = if drained { 0 } else { tasks.len() as u64 };
    tasks.shutdown().await;
    interrupted
}

/// Espacia inicios de iteración para no superar una tasa: cada inicio toma el próximo
/// turno libre.
struct Pacer {
    next: std::sync::Mutex<Instant>,
    interval: Duration,
    delayed: AtomicU64,
}

impl Pacer {
    fn new(interval: Duration, start: Instant) -> Self {
        Self {
            next: std::sync::Mutex::new(start),
            interval,
            delayed: AtomicU64::new(0),
        }
    }

    /// Espera el turno. `false` si el turno cae después de `deadline`.
    async fn wait(&self, deadline: Instant) -> bool {
        let slot = {
            let mut next = self.next.lock().unwrap();
            let slot = (*next).max(Instant::now());
            *next = slot + self.interval;
            slot
        };
        if slot >= deadline {
            return false;
        }
        if slot > Instant::now() + Duration::from_millis(1) {
            self.delayed.fetch_add(1, Relaxed);
            sleep_until(slot.into()).await;
        }
        true
    }
}

/// Ejecuta los pasos de un flow en orden. Si un paso falla sin respuesta o un `extract`
/// no encuentra su valor, la iteración se corta: los pasos siguientes dependen de él.
async fn iteration(
    plan: &Plan,
    flow: usize,
    client: &Client,
    setup: &HashMap<String, Value>,
    tx: &mpsc::UnboundedSender<Sample>,
    progress: &Progress,
    intended: Instant,
) {
    let mut from = intended;
    let mut locals = HashMap::new();
    for step in &plan.flows[flow].steps {
        let run = run_step(client, plan, step, setup, &locals).await;
        let done = Instant::now();
        let broken = run.broken();
        let outcome = match run.result {
            Ok(r) => Outcome::Response {
                status: r.status,
                timings: r.timings,
            },
            Err(f) => Outcome::Failure {
                kind: f.kind,
                message: f.message,
            },
        };
        progress.requests.fetch_add(1, Relaxed);
        if broken {
            progress.failed.fetch_add(1, Relaxed);
        }
        let _ = tx.send(Sample {
            step: step.id,
            latency: done.saturating_duration_since(from),
            outcome,
            extract_error: run.extract_error,
            checks: run.checks,
        });
        if broken {
            return;
        }
        locals.extend(run.extracted);
        from = done;
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
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

/// Envía el request ya armado de un paso, con su timeout. Si armarlo falló, devuelve
/// una falla de tipo `template`.
pub async fn send_step(
    client: &Client,
    step: &StepPlan,
    request: Result<Request<Body>, String>,
) -> Result<Response, Failure> {
    let request = request.map_err(|e| Failure::new(FailureKind::Template, e))?;
    match timeout(
        step.timeout,
        client.send(&step.origin, || http::duplicate(&request)),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(Failure::new(
            FailureKind::Timeout,
            format!(
                "sin respuesta en {}",
                crate::units::HumanDuration(step.timeout)
            ),
        )),
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
