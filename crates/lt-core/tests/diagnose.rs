//! Validación del diagnóstico: cada patología conocida tiene que disparar su hallazgo,
//! y una corrida sana no tiene que disparar nada grave.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use axum::{
    Router,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use lt_core::{
    config::Environment,
    diagnose::{Finding, Severity},
    engine::{self, Progress, plan::Plan},
    metrics::RunResult,
    validate::parse_scenario,
};
use tokio::{net::TcpListener, sync::Semaphore};

async fn serve(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{}", addr.port())
}

async fn run(base: &str, path: &str, load: &str) -> RunResult {
    lt_core::timer::high_resolution();
    let yaml = format!(
        "version: 1\nname: lab\nflows:\n  main:\n    steps:\n      - name: target\n        request: {{ method: GET, url: {path} }}\n\
         \x20       source: {{ file: src/target.rs, symbol: handler }}\nload:\n{load}"
    );
    let env: Environment =
        serde_saphyr::from_str(&format!("base_url: {base}\nallowed_hosts: [127.0.0.1]\n")).unwrap();
    let plan = Plan::build(&parse_scenario(&yaml).unwrap(), &env, &Default::default()).unwrap();
    engine::run(Arc::new(plan), Arc::new(Progress::default())).await
}

fn find<'a>(r: &'a RunResult, id: &str) -> &'a Finding {
    r.findings.iter().find(|f| f.id == id).unwrap_or_else(|| {
        panic!(
            "falta `{id}`; hallazgos: {:#?}\nserie: {:?}",
            r.findings, r.series.points
        )
    })
}

fn ids(r: &RunResult) -> Vec<&str> {
    r.findings.iter().map(|f| f.id.as_str()).collect()
}

const RAMP: &str = "  executor: ramping_rate\n  start_rate: 20/s\n  stages:\n    - { target: 400/s, duration: 5s }\n  max_vus: 60\n";

#[tokio::test(flavor = "multi_thread")]
async fn healthy_ramp_has_no_serious_findings() {
    let base = serve(Router::new().route("/ok", get(|| async { "ok" }))).await;
    let r = run(&base, "/ok", RAMP).await;
    assert!(
        r.findings.iter().all(|f| f.severity != Severity::High),
        "{:#?}",
        r.findings
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn small_pool_shows_a_breaking_point() {
    // 4 requests a la vez × 20 ms: capacidad ~200 req/s.
    let pool = Arc::new(Semaphore::new(4));
    let app = Router::new().route(
        "/pool",
        get(move || {
            let pool = pool.clone();
            async move {
                let _permit = pool.acquire().await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
                "ok"
            }
        }),
    );
    let base = serve(app).await;
    let r = run(&base, "/pool", RAMP).await;

    let f = find(&r, "breaking_point");
    assert_eq!(f.step.as_deref(), Some("target"));
    assert_eq!(
        f.source.as_ref().and_then(|s| s.file.as_deref()),
        Some("src/target.rs")
    );
    let at = f.evidence["offered_iterations_per_s"].as_u64().unwrap();
    assert!(
        (120..=280).contains(&at),
        "quiebre en {at} it/s; {}",
        f.title
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limiter_shows_an_error_threshold_with_429() {
    // Deja pasar 150 requests por segundo de reloj; el resto, 429.
    let window = Arc::new((AtomicU64::new(0), AtomicU64::new(0)));
    let started = Instant::now();
    let app = Router::new().route(
        "/limited",
        get(move || {
            let window = window.clone();
            async move {
                let second = started.elapsed().as_secs();
                if window.0.swap(second, Relaxed) != second {
                    window.1.store(0, Relaxed);
                }
                if window.1.fetch_add(1, Relaxed) < 150 {
                    StatusCode::OK
                } else {
                    StatusCode::TOO_MANY_REQUESTS
                }
            }
        }),
    );
    let base = serve(app).await;
    let r = run(&base, "/limited", RAMP).await;

    let f = find(&r, "error_threshold");
    assert!(f.title.contains("429"), "{}", f.title);
    assert!(f.causes[0].contains("rate limiter"), "{:?}", f.causes);
    let at = f.evidence["offered_iterations_per_s"].as_u64().unwrap();
    assert!(
        (120..=260).contains(&at),
        "errores desde {at} it/s; {}",
        f.title
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn growing_work_shows_degradation_over_time() {
    // Cada request tarda 2 ms + 0,15 ms por request ya atendida.
    let served = Arc::new(AtomicU64::new(0));
    let app = Router::new().route(
        "/leak",
        get(move || {
            let served = served.clone();
            async move {
                let n = served.fetch_add(1, Relaxed);
                tokio::time::sleep(Duration::from_micros(2_000 + 150 * n)).await;
                "ok"
            }
        }),
    );
    let base = serve(app).await;
    let r = run(
        &base,
        "/leak",
        "  executor: constant_rate\n  rate: 50/s\n  duration: 6s\n  max_vus: 100\n",
    )
    .await;

    let f = find(&r, "degradation_over_time");
    assert_eq!(f.step.as_deref(), Some("target"));
    assert!(!ids(&r).contains(&"capacity_exceeded"), "{:?}", ids(&r));
}

#[tokio::test(flavor = "multi_thread")]
async fn occasional_slow_requests_show_a_long_tail() {
    // Una de cada 25 requests tarda 150 ms; el resto, 1 ms.
    let served = Arc::new(AtomicU64::new(0));
    let app = Router::new().route(
        "/tail",
        get(move || {
            let served = served.clone();
            async move {
                let slow = served.fetch_add(1, Relaxed).is_multiple_of(25);
                tokio::time::sleep(Duration::from_millis(if slow { 150 } else { 1 })).await;
                "ok"
            }
        }),
    );
    let base = serve(app).await;
    let r = run(
        &base,
        "/tail",
        "  executor: constant_rate\n  rate: 100/s\n  duration: 3s\n  max_vus: 100\n",
    )
    .await;

    let f = find(&r, "long_tail");
    assert_eq!(f.severity, Severity::Medium);
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_every_connection_shows_new_connections() {
    let app = Router::new().route(
        "/close",
        get(|| async { ([(header::CONNECTION, "close")], "ok").into_response() }),
    );
    let base = serve(app).await;
    let r = run(
        &base,
        "/close",
        "  executor: constant_rate\n  rate: 100/s\n  duration: 2s\n  max_vus: 50\n",
    )
    .await;

    let f = find(&r, "new_connections");
    assert!(
        f.evidence["new_connections"].as_u64().unwrap() >= 150,
        "{}",
        f.evidence
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exceeding_capacity_at_constant_rate() {
    let pool = Arc::new(Semaphore::new(2));
    let app = Router::new().route(
        "/pool",
        get(move || {
            let pool = pool.clone();
            async move {
                let _permit = pool.acquire().await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
                "ok"
            }
        }),
    );
    let base = serve(app).await;
    // Capacidad ~100/s; se piden 200/s.
    let r = run(
        &base,
        "/pool",
        "  executor: constant_rate\n  rate: 200/s\n  duration: 3s\n  max_vus: 20\n",
    )
    .await;

    let f = find(&r, "capacity_exceeded");
    let sustained = f.evidence["sustained_iterations_per_s"].as_f64().unwrap();
    assert!((60.0..=140.0).contains(&sustained), "{}", f.title);
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_server_with_keep_alive_has_no_new_connections() {
    // Cada request tarda 300 ms: el pool crece hasta ~30 conexiones y las reutiliza.
    // Abrir una conexión por VU no es falta de keep-alive (caso `login-ramp` de shop-api).
    let app = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            "ok"
        }),
    );
    let base = serve(app).await;
    let r = run(
        &base,
        "/slow",
        "  executor: constant_rate\n  rate: 100/s\n  duration: 2s\n  max_vus: 100\n",
    )
    .await;

    assert!(!ids(&r).contains(&"new_connections"), "{:#?}", r.findings);
}

/// API con tokens que vencen a los 2 s: `/login` emite uno y `/me` responde 401 si venció.
async fn expiring_tokens() -> String {
    let started = Instant::now();
    let app = Router::new()
        .route(
            "/login",
            axum::routing::post(move || async move {
                let issued = started.elapsed().as_millis();
                axum::Json(serde_json::json!({ "token": issued.to_string() }))
            }),
        )
        .route(
            "/me",
            get(move |headers: axum::http::HeaderMap| async move {
                let issued: u128 = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok()?.parse().ok())
                    .unwrap_or(0);
                if started.elapsed().as_millis() - issued > 2000 {
                    (
                        StatusCode::UNAUTHORIZED,
                        axum::Json(serde_json::json!({ "error": "token vencido" })),
                    )
                } else {
                    (StatusCode::OK, axum::Json(serde_json::json!({ "id": 1 })))
                }
            }),
        );
    serve(app).await
}

async fn run_session(base: &str, refresh: &str) -> RunResult {
    lt_core::timer::high_resolution();
    // Con pausas, pocas requests por segundo: como un usuario real.
    let yaml = format!(
        "version: 1\nname: lab\nvu_setup:
  - name: login
    request: {{ method: POST, url: /login }}
    extract: {{ token: {{ json: $.token }} }}
{refresh}flows:
  main:
    steps:
      - name: me
        request: {{ method: GET, url: /me, headers: {{ Authorization: \"{{{{token}}}}\" }} }}
        extract: {{ id: {{ json: $.id }} }}
        think: 300ms-500ms
load:\n  executor: constant_vus\n  vus: 2\n  duration: 4s\n"
    );
    let env: Environment =
        serde_saphyr::from_str(&format!("base_url: {base}\nallowed_hosts: [127.0.0.1]\n")).unwrap();
    let plan = Plan::build(&parse_scenario(&yaml).unwrap(), &env, &Default::default()).unwrap();
    engine::run(Arc::new(plan), Arc::new(Progress::default())).await
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_session_says_so_and_suggests_vu_refresh() {
    let base = expiring_tokens().await;
    let r = run_session(&base, "").await;

    let f = find(&r, "error_threshold");
    // Empieza a mitad de corrida, no desde el inicio, aunque haya pocas requests por segundo.
    assert!(
        f.title.contains("recibe 401 desde el segundo 2"),
        "{}",
        f.title
    );
    assert!(f.title.contains("venció la sesión"), "{}", f.title);
    assert!(
        f.causes[0].contains("agregá `vu_refresh`"),
        "{:?}",
        f.causes
    );
    // Un 401 sin el campo del `extract` es un error, no dos.
    let rate = f.evidence["error_rate_in_window"].as_f64().unwrap();
    assert!(rate <= 1.0, "{rate}");
    for p in &r.series.steps[0].points {
        assert!(p.errors <= p.requests, "{p:?}");
    }

    // Con la renovación antes de que venza, no hay 401.
    let r = run_session(&base, "vu_refresh: { every: 1s }\n").await;
    assert!(!ids(&r).contains(&"error_threshold"), "{:#?}", r.findings);
    assert_eq!(
        r.steps[0].status_codes.get("401"),
        None,
        "{:?}",
        r.steps[0].status_codes
    );
    assert!(r.vu_refresh.unwrap().refreshes >= 4);
}
