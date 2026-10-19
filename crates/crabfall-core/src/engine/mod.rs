//! Motor: scheduler de modelo abierto (ADR-003).

pub mod cookies;
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
    metrics::{
        Collector, Outcome, Percentiles, RunResult, SetupStep, Totals, VuRefreshResult,
        VuSetupResult, VuSetupStep, histogram, record,
    },
};
use cookies::Jar;
use http::{Body, Client, Failure, FailureKind, Meter, Origin, Response, Traffic};
use hyper::{
    Method, Request,
    header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, HOST, HeaderValue, LOCATION},
};
use plan::{Plan, StepPlan, Vars};

/// Contadores en vivo, para mostrar progreso.
#[derive(Default)]
pub struct Progress {
    pub started: AtomicU64,
    pub dropped: AtomicU64,
    pub requests: AtomicU64,
    pub failed: AtomicU64,
    /// VUs que completaron `vu_setup`, de `vus_total`.
    pub vus_ready: AtomicU64,
    pub vus_total: AtomicU64,
}

/// Lo que el motor le cuenta al colector, con su instante.
enum Event {
    Sample(Sample),
    /// Arrancó una iteración (planificada para este instante en el modelo abierto).
    Started(Instant),
    /// Se descartó una iteración planificada para este instante.
    Dropped(Instant),
}

struct Sample {
    step: usize,
    /// Cuándo terminó el request.
    done: Instant,
    latency: Duration,
    outcome: Outcome,
    traffic: Traffic,
    /// La respuesta llegó, pero un `extract` falló.
    extract_error: Option<String>,
    checks: Vec<Option<String>>,
}

/// Resultado de ejecutar un paso: la respuesta (o la falla) y lo extraído.
pub struct StepRun {
    pub result: Result<Response, Failure>,
    /// Bytes que pasaron por el socket, aunque no haya llegado respuesta.
    pub traffic: Traffic,
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
    jar: &Jar,
) -> StepRun {
    let request = step.request(&Vars {
        plan,
        setup,
        locals,
    });
    let meter = Arc::new(Meter::default());
    let result = send_step(client, plan, step, request, &meter, jar).await;
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
        traffic: meter.traffic(),
        extracted,
        extract_error,
        checks,
    }
}

/// Por qué un paso de `setup` o `vu_setup` no sirve: sin respuesta, status >= 400, un
/// `extract` fallido o un check fallido. `None` si salió bien.
fn step_problem(step: &StepPlan, run: &StepRun) -> Option<String> {
    match (&run.result, &run.extract_error) {
        (Err(f), _) => Some(f.message.clone()),
        (Ok(r), _) if r.status >= 400 => Some(format!("respondió {}", r.status)),
        (_, Some(e)) => Some(e.clone()),
        _ => step.checks.iter().zip(&run.checks).find_map(|(c, r)| {
            r.as_ref()
                .map(|why| format!("el check `{}` falló: {why}", c.label))
        }),
    }
}

/// VUs preparados (o renovados) a la vez como máximo.
const VU_SETUP_CONCURRENCY: usize = 50;

/// La sesión de un VU: sus variables (lo que extrajo su `vu_setup`, más su fila de cada
/// dataset) y, con `vu_refresh`, cuándo le toca renovar. Sin `vu_setup`, sin variables.
/// La renovación la cambia en segundo plano: cada iteración toma las variables al empezar.
struct Session {
    vars: std::sync::Mutex<Arc<HashMap<String, Value>>>,
    /// `None` sin `vu_refresh` o mientras se está renovando.
    renew_at: std::sync::Mutex<Option<Instant>>,
    /// Falló al renovar: deja de iterar.
    retired: std::sync::atomic::AtomicBool,
    /// Sus cookies, desde `vu_setup` hasta la última iteración.
    cookies: Jar,
}

type Vu = Arc<Session>;

impl Session {
    fn new(vars: HashMap<String, Value>, renew_at: Option<Instant>, cookies: Jar) -> Vu {
        Arc::new(Self {
            vars: std::sync::Mutex::new(Arc::new(vars)),
            renew_at: std::sync::Mutex::new(renew_at),
            retired: false.into(),
            cookies,
        })
    }

    fn vars(&self) -> Arc<HashMap<String, Value>> {
        self.vars.lock().unwrap().clone()
    }

    fn retired(&self) -> bool {
        self.retired.load(Relaxed)
    }
}

/// Latencias, fallas y motivos por paso de `vu_setup` o `vu_refresh`.
struct StepStats(Vec<(Histogram<u64>, u64, u64, Vec<String>)>);

impl StepStats {
    fn new(steps: usize) -> Self {
        Self(
            (0..steps)
                .map(|_| (histogram(), 0, 0, Vec::new()))
                .collect(),
        )
    }

    fn record(&mut self, step: usize, latency: Duration, problem: Option<String>) {
        let (h, requests, failed, errors) = &mut self.0[step];
        record(h, latency);
        *requests += 1;
        if let Some(p) = problem {
            *failed += 1;
            if errors.len() < 3 && !errors.contains(&p) {
                errors.push(p);
            }
        }
    }

    fn results(&self, steps: &[StepPlan]) -> Vec<VuSetupStep> {
        steps
            .iter()
            .zip(&self.0)
            .map(|(step, (h, requests, failed, errors))| VuSetupStep {
                name: step.name.clone(),
                requests: *requests,
                failed: *failed,
                latency_ms: Percentiles::from(h),
                errors: errors.clone(),
            })
            .collect()
    }
}

/// Corre `steps` en orden con las variables del VU; lo extraído las pisa. Devuelve las
/// variables nuevas (o `None` si un paso falló) y la latencia y el problema de cada paso.
async fn session_steps(
    client: &Client,
    plan: &Plan,
    steps: &[StepPlan],
    setup: &HashMap<String, Value>,
    mut vars: HashMap<String, Value>,
    jar: &Jar,
) -> (
    Option<HashMap<String, Value>>,
    Vec<(usize, Duration, Option<String>)>,
) {
    let mut samples = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let t = Instant::now();
        let run = run_step(client, plan, step, setup, &vars, jar).await;
        let problem = step_problem(step, &run);
        samples.push((i, t.elapsed(), problem.clone()));
        if problem.is_some() {
            return (None, samples);
        }
        vars.extend(run.extracted);
    }
    (Some(vars), samples)
}

/// Corre `vu_setup` para `count` VUs, antes de la carga: a lo sumo 50 a la vez y sin
/// superar `limits.max_rps`. Devuelve los VUs que lo completaron.
async fn prepare_vus(
    client: &Arc<Client>,
    plan: &Arc<Plan>,
    setup: &Arc<HashMap<String, Value>>,
    count: u32,
    progress: &Arc<Progress>,
) -> (Vec<Vu>, VuSetupResult) {
    let started = Instant::now();
    let limit = Arc::new(Semaphore::new(VU_SETUP_CONCURRENCY));
    let pacer = plan.request_cap.map(|cap| {
        Arc::new(Pacer::new(
            Duration::from_secs_f64(plan.vu_setup.len().max(1) as f64 / cap),
            started,
        ))
    });
    let never = started + Duration::from_secs(10 * 365 * 24 * 3600);
    let every = plan.vu_refresh.as_ref().map(|r| r.every);
    progress.vus_total.store(u64::from(count), Relaxed);

    let mut tasks = JoinSet::new();
    for _ in 0..count {
        let (client, plan, setup, limit, pacer, progress) = (
            client.clone(),
            plan.clone(),
            setup.clone(),
            limit.clone(),
            pacer.clone(),
            progress.clone(),
        );
        tasks.spawn(async move {
            let _permit = limit
                .acquire_owned()
                .await
                .expect("el semáforo no se cierra");
            if let Some(p) = &pacer {
                p.wait(never).await;
            }
            let jar = Jar::new(plan.cookies);
            let (vars, samples) = session_steps(
                &client,
                &plan,
                &plan.vu_setup,
                &setup,
                plan.vu_row_vars(),
                &jar,
            )
            .await;
            if vars.is_some() {
                progress.vus_ready.fetch_add(1, Relaxed);
            }
            // El token se emitió recién: la cuenta para renovarlo arranca ahora.
            let vu = vars.map(|vars| Session::new(vars, every.map(|e| Instant::now() + e), jar));
            (vu, samples)
        });
    }

    let mut ready = Vec::new();
    let mut stats = StepStats::new(plan.vu_setup.len());
    while let Some(joined) = tasks.join_next().await {
        let (vu, samples) = joined.expect("la preparación de un VU no entra en pánico");
        for (i, latency, problem) in samples {
            stats.record(i, latency, problem);
        }
        ready.extend(vu);
    }
    let result = VuSetupResult {
        vus: count,
        failed: count - ready.len() as u32,
        duration_ms: round3(started.elapsed().as_secs_f64() * 1000.0),
        steps: stats.results(&plan.vu_setup),
    };
    (ready, result)
}

/// Estado de `vu_refresh` durante la corrida.
struct Refresh {
    every: Duration,
    /// Después de esto la carga ya no arranca iteraciones: no tiene sentido renovar.
    until: Instant,
    stats: std::sync::Mutex<StepStats>,
    refreshes: AtomicU64,
    failed_vus: AtomicU64,
}

impl Refresh {
    fn result(&self, plan: &Plan) -> VuRefreshResult {
        VuRefreshResult {
            every_s: round3(self.every.as_secs_f64()),
            refreshes: self.refreshes.load(Relaxed),
            failed_vus: self.failed_vus.load(Relaxed),
            steps: self.stats.lock().unwrap().results(plan.refresh_steps()),
        }
    }
}

/// Renueva la sesión de un VU. Si falla, el VU queda retirado y deja de iterar.
async fn renew(shared: &Shared, refresh: &Refresh, vu: &Session) {
    let (vars, samples) = session_steps(
        &shared.client,
        &shared.plan,
        shared.plan.refresh_steps(),
        &shared.setup,
        (*vu.vars()).clone(),
        &vu.cookies,
    )
    .await;
    {
        let mut stats = refresh.stats.lock().unwrap();
        for (i, latency, problem) in samples {
            stats.record(i, latency, problem);
        }
    }
    refresh.refreshes.fetch_add(1, Relaxed);
    match vars {
        Some(vars) => {
            *vu.vars.lock().unwrap() = Arc::new(vars);
            *vu.renew_at.lock().unwrap() = Some(Instant::now() + refresh.every);
        }
        None => {
            vu.retired.store(true, Relaxed);
            refresh.failed_vus.fetch_add(1, Relaxed);
        }
    }
}

/// Renueva en segundo plano cada sesión cuando le toca, esté el VU libre o en una
/// iteración: la iteración en curso sigue con el token anterior, que todavía vale, y la
/// próxima ya toma el nuevo. Así renovar no le saca capacidad a la carga.
async fn refresher(shared: Shared, refresh: Arc<Refresh>, vus: Vec<Vu>) {
    let limit = Arc::new(Semaphore::new(VU_SETUP_CONCURRENCY));
    let mut renewals = JoinSet::new();
    loop {
        let now = Instant::now();
        if now >= refresh.until {
            break;
        }
        for vu in &vus {
            let mut at = vu.renew_at.lock().unwrap();
            if at.is_some_and(|at| at <= now) {
                // `None` mientras se renueva, para no renovarlo dos veces.
                *at = None;
                let (shared, refresh, limit, vu) =
                    (shared.clone(), refresh.clone(), limit.clone(), vu.clone());
                renewals.spawn(async move {
                    let _permit = limit.acquire_owned().await.expect("no se cierra");
                    renew(&shared, &refresh, &vu).await;
                });
            }
        }
        while renewals.try_join_next().is_some() {}
        // Hasta el próximo vencimiento; como mucho 100 ms, por las renovaciones en curso.
        let next = vus
            .iter()
            .filter_map(|vu| *vu.renew_at.lock().unwrap())
            .min()
            .unwrap_or(now + Duration::from_millis(100))
            .clamp(now, now + Duration::from_millis(100));
        sleep_until(next.into()).await;
    }
    while renewals.join_next().await.is_some() {}
}

/// Corre `setup` en orden. Devuelve las variables extraídas, el detalle de cada paso y,
/// si algo falló, por qué se aborta la corrida.
pub async fn run_setup(
    client: &Client,
    plan: &Plan,
) -> (HashMap<String, Value>, Vec<SetupStep>, Option<String>) {
    let mut vars = HashMap::new();
    let mut steps = Vec::new();
    // `setup` corre una vez, con sus propias cookies: no son de ningún VU.
    let jar = Jar::new(plan.cookies);
    for step in &plan.setup {
        let start = Instant::now();
        let run = run_step(client, plan, step, &vars, &HashMap::new(), &jar).await;
        let status = run.result.as_ref().ok().map(|r| r.status);
        let problem = step_problem(step, &run);
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
    tx: mpsc::UnboundedSender<Event>,
    progress: Arc<Progress>,
}

impl Shared {
    async fn iteration(&self, flow: usize, intended: Instant, vu: &Session) {
        iteration(self, flow, &vu.vars(), &vu.cookies, intended).await;
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
    let setup_vars = Arc::new(setup_vars);

    // Un VU por cada lugar de concurrencia; con `vu_setup`, cada uno se prepara antes de la carga.
    let concurrency = plan.load.concurrency();
    let (vus, vu_setup) = if plan.vu_setup.is_empty() {
        // Sin variables ni renovación, pero cada VU con sus cookies.
        let vus = (0..concurrency)
            .map(|_| Session::new(HashMap::new(), None, Jar::new(plan.cookies)))
            .collect();
        (vus, None)
    } else {
        let (ready, result) =
            prepare_vus(&client, &plan, &setup_vars, concurrency, &progress).await;
        if ready.is_empty() {
            let why = result
                .steps
                .iter()
                .find_map(|s| s.errors.first().map(|e| format!("`{}`: {e}", s.name)))
                .unwrap_or_default();
            let mut aborted = RunResult::aborted(
                &plan,
                setup,
                format!("el vu_setup falló en los {concurrency} VUs; {why}"),
            );
            aborted.vu_setup = Some(result);
            return aborted;
        }
        (ready, Some(result))
    };

    let start = Instant::now() + Duration::from_millis(50);
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let collector_plan = plan.clone();
    let collector = tokio::spawn(async move {
        let mut c = Collector::new(&collector_plan, start);
        while let Some(event) = rx.recv().await {
            match event {
                Event::Started(at) => c.record_started(at),
                Event::Dropped(at) => c.record_dropped(at),
                Event::Sample(s) => {
                    let counted = c.record(s.step, s.done, s.latency, s.outcome, s.traffic);
                    c.record_checks(s.step, &s.checks);
                    if let Some(e) = s.extract_error {
                        c.record_extract_failure(s.step, s.done, e, counted);
                    }
                }
            }
        }
        c
    });

    let refresh = plan.vu_refresh.as_ref().map(|r| {
        Arc::new(Refresh {
            every: r.every,
            until: start + plan.load.duration(),
            stats: std::sync::Mutex::new(StepStats::new(plan.refresh_steps().len())),
            refreshes: AtomicU64::new(0),
            failed_vus: AtomicU64::new(0),
        })
    });
    let shared = Shared {
        plan: plan.clone(),
        client,
        setup: setup_vars,
        tx,
        progress: progress.clone(),
    };
    let refresher = refresh
        .clone()
        .map(|r| tokio::spawn(refresher(shared.clone(), r, vus.clone())));
    let dispatch = match &plan.load {
        LoadPlan::Open { .. } => open_model(shared, start, vus).await,
        LoadPlan::Closed { duration, .. } => closed_model(shared, start, vus, *duration).await,
    };
    // Tiene una copia del canal de eventos: el colector no termina mientras siga viva.
    if let Some(r) = refresher {
        r.abort();
        let _ = r.await;
    }

    let elapsed = start.elapsed().as_secs_f64();
    let collector = collector.await.expect("el colector no falla");
    let thresholds = collector.thresholds(&plan, elapsed);
    let series = collector.series(&plan);
    let steps = collector.finish(&plan);

    let started = progress.started.load(Relaxed);
    let requests: u64 = steps.iter().map(|s| s.requests).sum();
    let bytes_sent: u64 = steps.iter().map(|s| s.bytes_sent).sum();
    let bytes_received: u64 = steps.iter().map(|s| s.bytes_received).sum();
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
        bytes_sent,
        bytes_received,
        send_rate: round1(bytes_sent as f64 / elapsed),
        receive_rate: round1(bytes_received as f64 / elapsed),
    };
    let scheduler_lag = dispatch.lag.as_ref().and_then(Percentiles::from);
    let mut warnings = warnings(&totals, scheduler_lag.as_ref());
    if let Some(v) = vu_setup.as_ref().filter(|v| v.failed > 0) {
        warnings.push(format!(
            "{} de {} VUs no completaron vu_setup y quedaron afuera: la carga corrió con {}.",
            v.failed,
            v.vus,
            v.vus - v.failed
        ));
    }
    let vu_refresh = refresh.map(|r| r.result(&plan));
    if let Some(r) = vu_refresh.as_ref().filter(|r| r.failed_vus > 0) {
        warnings.push(format!(
            "{} VUs fallaron al renovar la sesión (vu_refresh) y dejaron de iterar.",
            r.failed_vus
        ));
    }
    // Las esperas cortas al arrancar (todos los VUs a la vez) no limitan la carga: se avisa
    // solo si algún segundo llegó cerca del tope.
    let cap = plan.request_cap.unwrap_or(f64::INFINITY);
    let near_cap = series
        .points
        .iter()
        .any(|p| p.requests as f64 >= 0.9 * cap * f64::from(series.window_s));
    if dispatch.paced > 0 && near_cap {
        warnings.push(format!(
            "{} iteraciones esperaron para no superar limits.max_rps ({}/s): la carga quedó limitada por el entorno.",
            dispatch.paced,
            plan.request_cap.unwrap_or_default()
        ));
    }

    let mut result = RunResult {
        passed: thresholds.iter().all(|t| t.passed),
        duration_s: round1(elapsed),
        aborted: None,
        setup,
        vu_setup,
        vu_refresh,
        totals,
        thresholds,
        scheduler_lag_ms: scheduler_lag,
        steps,
        warnings,
        findings: vec![],
        series,
    };
    result.findings = crate::diagnose::diagnose(&plan.load, &result);
    result
}

/// Modelo abierto: cada iteración arranca en su instante planificado; si no hay VU
/// libre, se descarta (nunca se demora).
async fn open_model(shared: Shared, start: Instant, vus: Vec<Vu>) -> Dispatch {
    let plan = shared.plan.clone();
    // VUs libres. Una iteración toma uno y lo devuelve al terminar; si no hay, se descarta.
    let free = Arc::new(std::sync::Mutex::new(vus));
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
        // Los que fallaron al renovar se descartan del pool.
        let vu = {
            let mut free = free.lock().unwrap();
            free.retain(|vu| !vu.retired());
            free.pop()
        };
        let Some(vu) = vu else {
            shared.progress.dropped.fetch_add(1, Relaxed);
            let _ = shared.tx.send(Event::Dropped(intended));
            continue;
        };
        shared.progress.started.fetch_add(1, Relaxed);
        let _ = shared.tx.send(Event::Started(intended));
        let (shared, free) = (shared.clone(), free.clone());
        tasks.spawn(async move {
            shared.iteration(flow, intended, &vu).await;
            free.lock().unwrap().push(vu);
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
async fn closed_model(
    shared: Shared,
    start: Instant,
    vus: Vec<Vu>,
    duration: Duration,
) -> Dispatch {
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
    for vu in vus {
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
                if vu.retired() {
                    break;
                }
                let flow = picker.lock().unwrap().next();
                shared.progress.started.fetch_add(1, Relaxed);
                let _ = shared.tx.send(Event::Started(Instant::now()));
                shared.iteration(flow, Instant::now(), &vu).await;
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
    shared: &Shared,
    flow: usize,
    vu: &HashMap<String, Value>,
    jar: &Jar,
    intended: Instant,
) {
    let Shared {
        plan,
        client,
        setup,
        tx,
        progress,
    } = shared;
    let mut from = intended;
    // Lo del VU (su `vu_setup`) y las filas de esta iteración; lo extraído después lo pisa.
    let mut locals = vu.clone();
    locals.extend(plan.row_vars(&plan.flows[flow]));
    for step in &plan.flows[flow].steps {
        let run = run_step(client, plan, step, setup, &locals, jar).await;
        let done = Instant::now();
        let broken = run.broken();
        let outcome = match run.result {
            Ok(r) => Outcome::Response {
                status: r.status,
                timings: r.timings,
                limit: (r.status == 429)
                    .then(|| Box::new(crate::ratelimit::Sample::from_headers(&r.headers))),
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
        let _ = tx.send(Event::Sample(Sample {
            step: step.id,
            done,
            latency: done.saturating_duration_since(from),
            outcome,
            traffic: run.traffic,
            extract_error: run.extract_error,
            checks: run.checks,
        }));
        if broken {
            return;
        }
        locals.extend(run.extracted);
        from = done;
        // La pausa ocupa el VU, pero no cuenta en la latencia del paso siguiente.
        if let Some(think) = step.think {
            tokio::time::sleep(think.sample()).await;
            from = Instant::now();
        }
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

/// Manda el request con las cookies del VU y sigue los redirects del paso, solo hacia
/// `allowed_hosts` (ADR-014). Devuelve la última respuesta, ya descomprimida.
async fn follow(
    client: &Client,
    plan: &Plan,
    step: &StepPlan,
    mut request: Request<Body>,
    meter: &Arc<Meter>,
    jar: &Jar,
) -> Result<Response, Failure> {
    let mut origin = step.origin.clone();
    // La cookie que puso el escenario, para sumarle las del jar sin perderla.
    let mut own_cookie = request.headers().get(COOKIE).cloned();
    let mut redirects = Vec::new();
    loop {
        let url = url_of(&origin, request.uri())?;
        let cookie = match (&own_cookie, jar.header(&url)) {
            (Some(own), Some(jar)) => Some(format!("{}; {jar}", own.to_str().unwrap_or_default())),
            (Some(own), None) => own.to_str().ok().map(String::from),
            (None, jar) => jar,
        };
        request.headers_mut().remove(COOKIE);
        if let Some(c) = cookie.and_then(|c| HeaderValue::from_str(&c).ok()) {
            request.headers_mut().insert(COOKIE, c);
        }

        let mut response = client
            .send(&origin, || http::duplicate(&request), meter)
            .await?;
        jar.store(&url, &response.headers);
        let location = match response.status {
            301 | 302 | 303 | 307 | 308 if redirects.len() < step.redirects as usize => response
                .headers
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(String::from),
            _ => None,
        };
        let Some(location) = location else {
            response.redirects = redirects;
            http::decode(&mut response)?;
            return Ok(response);
        };

        let next = url.join(&location).map_err(|e| {
            Failure::new(
                FailureKind::Redirect,
                format!("redirect a `{location}`: URL inválida ({e})"),
            )
        })?;
        let https = match next.scheme() {
            "http" => false,
            "https" => true,
            other => {
                return Err(Failure::new(
                    FailureKind::Redirect,
                    format!("redirect a `{next}`: esquema `{other}` no soportado"),
                ));
            }
        };
        let host = next.host_str().unwrap_or_default().to_ascii_lowercase();
        if !plan.allowed_hosts.contains(&host) {
            return Err(Failure::new(
                FailureKind::Redirect,
                format!("redirect a `{host}`, que no está en allowed_hosts del entorno"),
            ));
        }
        let next_origin = Origin {
            https,
            port: next
                .port_or_known_default()
                .unwrap_or(if https { 443 } else { 80 }),
            host,
        };
        // Como un navegador: las credenciales propias no viajan a otro host.
        let same_host = next_origin.host == origin.host;
        if !same_host {
            own_cookie = None;
        }
        request = redirected(&request, response.status, &next, same_host)?;
        origin = next_origin;
        redirects.push(next.to_string());
    }
}

/// URL completa de un request, para el jar y para resolver un `location` relativo.
fn url_of(origin: &Origin, uri: &hyper::Uri) -> Result<url::Url, Failure> {
    let scheme = if origin.https { "https" } else { "http" };
    let target = uri.path_and_query().map_or("/", |p| p.as_str());
    url::Url::parse(&format!(
        "{scheme}://{}:{}{target}",
        origin.host, origin.port
    ))
    .map_err(|e| Failure::new(FailureKind::Protocol, format!("URL inválida: {e}")))
}

/// El request que sigue a un redirect. 303, y 301 o 302 después de un POST, pasan a GET
/// sin body (como los navegadores); 307 y 308 repiten método y body.
fn redirected(
    previous: &Request<Body>,
    status: u16,
    next: &url::Url,
    same_host: bool,
) -> Result<Request<Body>, Failure> {
    let method = previous.method();
    let to_get = status == 303 && method != Method::HEAD
        || matches!(status, 301 | 302) && method == Method::POST;
    let mut request = http::duplicate(previous);
    if to_get {
        *request.method_mut() = Method::GET;
        *request.body_mut() = Body::default();
        request.headers_mut().remove(CONTENT_TYPE);
    }
    *request.uri_mut() = next[url::Position::BeforePath..]
        .parse()
        .map_err(|e| Failure::new(FailureKind::Redirect, format!("redirect a `{next}`: {e}")))?;
    let host = next.host_str().unwrap_or_default();
    let authority = match next.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let headers = request.headers_mut();
    headers.insert(
        HOST,
        HeaderValue::from_str(&authority)
            .map_err(|e| Failure::new(FailureKind::Redirect, e.to_string()))?,
    );
    if !same_host {
        headers.remove(AUTHORIZATION);
    }
    Ok(request)
}

/// Envía el request ya armado de un paso, con su timeout. Si armarlo falló, devuelve
/// una falla de tipo `template`.
pub async fn send_step(
    client: &Client,
    plan: &Plan,
    step: &StepPlan,
    request: Result<Request<Body>, String>,
    meter: &Arc<Meter>,
    jar: &Jar,
) -> Result<Response, Failure> {
    let request = request.map_err(|e| Failure::new(FailureKind::Template, e))?;
    // El timeout cubre todos los saltos: es lo que espera un cliente real.
    match timeout(
        step.timeout,
        follow(client, plan, step, request, meter, jar),
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
