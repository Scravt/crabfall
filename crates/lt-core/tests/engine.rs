//! Tests del motor contra un servidor local con comportamiento conocido.

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    routing::{any, get},
};
use lt_core::{
    config::Environment,
    engine::{
        self, Progress,
        http::FailureKind,
        plan::{Inputs, Plan},
    },
    validate::parse_scenario,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// Devuelve lo que recibió: target, header Authorization y body.
async fn echo(request: axum::extract::Request) -> Json<Value> {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    Json(json!({
        "target": parts.uri.to_string(),
        "auth": parts.headers.get("authorization").map(|v| v.to_str().unwrap().to_string()),
        "body": String::from_utf8_lossy(&body),
    }))
}

async fn server() -> String {
    let app = Router::new()
        .route("/ok", get(|| async { "ok" }))
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                "ok"
            }),
        )
        .route(
            "/missing",
            get(|| async { (axum::http::StatusCode::NOT_FOUND, "no") }),
        )
        .route("/echo/{*rest}", any(echo));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{}", addr.port())
}

fn environment(base_url: &str) -> Environment {
    serde_saphyr::from_str(&format!(
        "base_url: {base_url}\nallowed_hosts: [127.0.0.1]\n"
    ))
    .unwrap()
}

fn scenario(flows: &str, load: &str) -> lt_core::scenario::Scenario {
    let yaml =
        format!("version: 1\nname: test\nflows:\n{flows}load:\n  executor: constant_rate\n{load}");
    parse_scenario(&yaml).unwrap_or_else(|r| panic!("{r:?}"))
}

async fn run(flows: &str, load: &str, base_url: &str) -> lt_core::metrics::RunResult {
    lt_core::timer::high_resolution();
    let plan = Plan::build(
        &scenario(flows, load),
        &environment(base_url),
        &Default::default(),
    )
    .unwrap();
    engine::run(Arc::new(plan), Arc::new(Progress::default())).await
}

const TWO_FLOWS: &str = "  browse:
    weight: 3
    steps:
      - name: ok
        request: { method: GET, url: /ok }
  broken:
    steps:
      - name: missing
        request: { method: GET, url: /missing }
      - name: after_missing
        request: { method: GET, url: /ok }
";

#[tokio::test(flavor = "multi_thread")]
async fn keeps_rate_and_splits_flows_by_weight() {
    let base = server().await;
    let r = run(
        TWO_FLOWS,
        "  rate: 200/s\n  duration: 1s\n  max_vus: 50\n",
        &base,
    )
    .await;

    assert_eq!(r.totals.planned, 200);
    assert_eq!(r.totals.started, 200);
    assert_eq!(r.totals.dropped, 0);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);

    let by_name = |n: &str| r.steps.iter().find(|s| s.name == n).unwrap();
    assert_eq!(by_name("ok").requests, 150);
    assert_eq!(by_name("missing").requests, 50);
    assert_eq!(by_name("missing").non_2xx, 50);
    // Un 404 es una respuesta: la iteración sigue.
    assert_eq!(by_name("after_missing").requests, 50);
    assert_eq!(r.totals.requests, 250);
}

#[tokio::test(flavor = "multi_thread")]
async fn drops_iterations_when_vus_run_out() {
    let base = server().await;
    let flows =
        "  main:\n    steps:\n      - name: slow\n        request: { method: GET, url: /slow }\n";
    let r = run(flows, "  rate: 50/s\n  duration: 1s\n  max_vus: 5\n", &base).await;

    // 5 VUs × (1 s / 300 ms) ≈ 15-20 iteraciones; el resto se descarta, no se demora.
    assert_eq!(r.totals.planned, 50);
    assert!(r.totals.dropped >= 25, "dropped = {}", r.totals.dropped);
    assert_eq!(r.totals.started + r.totals.dropped, 50);
    assert!(r.warnings[0].contains("descartadas"));
    let latency = r.steps[0].latency_ms.unwrap();
    assert!(
        latency.p50 >= 300.0 && latency.p50 < 400.0,
        "p50 = {}",
        latency.p50
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn classifies_connection_refused() {
    // Un puerto donde no escucha nadie.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    drop(listener);

    let flows = "  main:\n    steps:\n      - name: down\n        request: { method: GET, url: /ok }\n      - name: never\n        request: { method: GET, url: /ok }\n";
    // max_vus alto: en Windows un connect rechazado tarda ~2 s (reintentos de SYN).
    let r = run(
        flows,
        "  rate: 20/s\n  duration: 1s\n  max_vus: 50\n",
        &base,
    )
    .await;

    assert_eq!(r.totals.started, 20);
    assert_eq!(r.steps[0].failures.get(&FailureKind::Connect), Some(&20));
    // Si un paso falla sin respuesta, la iteración se corta.
    assert_eq!(r.steps[1].requests, 0);
}

#[test]
fn rejects_hosts_outside_allowlist_and_limits() {
    let flows = "  main:\n    steps:\n      - name: a\n        request: { method: GET, url: \"https://evil.example.com/x\" }\n      - name: b\n        request: { method: GET, url: /ok }\n";
    let s = scenario(flows, "  rate: 100/s\n  duration: 1s\n  max_vus: 5\n");
    let mut env = environment("http://127.0.0.1:1");
    env.limits.max_rps = Some(150.0);

    let errors = Plan::build(&s, &env, &Default::default()).err().unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("evil.example.com"), "{errors:?}");

    env.allowed_hosts.push("evil.example.com".into());
    let plan = Plan::build(&s, &env, &Default::default()).unwrap();
    // 100 it/s × 2 pasos = 200 req/s > 150.
    let violations = plan.limit_violations(&env);
    assert!(violations[0].contains("max_rps"), "{violations:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn check_runs_each_flow_once_and_shows_bodies() {
    let base = server().await;
    let s = scenario(
        TWO_FLOWS,
        "  rate: 1000/s\n  duration: 1h\n  max_vus: 1000\n",
    );
    let plan = Plan::build(&s, &environment(&base), &Default::default()).unwrap();

    let r = lt_core::check::check(&plan).await.unwrap();
    let names: Vec<_> = r.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["ok", "missing", "after_missing"]);
    assert_eq!(r.steps[0].body.as_deref(), Some("ok"));
    assert_eq!(r.steps[1].status, Some(404));
    assert!(!r.ok);
}

#[tokio::test]
async fn check_refuses_hosts_that_are_not_loopback() {
    let flows = "  main:\n    steps:\n      - name: a\n        request: { method: GET, url: /x }\n";
    let s = scenario(flows, "  rate: 1/s\n  duration: 1s\n  max_vus: 1\n");
    let env: Environment =
        serde_saphyr::from_str("base_url: http://192.0.2.1\nallowed_hosts: [192.0.2.1]\n").unwrap();
    let plan = Plan::build(&s, &env, &Default::default()).unwrap();

    let e = lt_core::check::check(&plan).await.unwrap_err();
    assert!(e.contains("loopback"), "{e}");
}

const ECHO: &str = "  main:
    steps:
      - name: echo
        request:
          method: POST
          url: /echo/items/{{id}}
          query: { by: \"{{who}}\" }
          json: { id: \"{{id}}\", label: \"n-{{id}}\", n: \"{{$randomInt(3,3)}}\" }
";

fn echo_scenario(defaults: &str) -> lt_core::scenario::Scenario {
    let yaml = format!(
        "version: 1\nname: echo\nvars: {{ id: 7, who: ana }}\n{defaults}flows:\n{ECHO}\
         load:\n  executor: constant_rate\n  rate: 1/s\n  duration: 1s\n  max_vus: 1\n"
    );
    parse_scenario(&yaml).unwrap_or_else(|r| panic!("{r:?}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn interpolates_vars_secrets_and_defaults() {
    let base = server().await;
    let s = echo_scenario("defaults: { headers: { Authorization: \"Bearer {{env.TOKEN}}\" } }\n");
    let inputs = Inputs {
        vars: [("who".to_string(), json!("bob maría"))].into(),
        secrets: [("TOKEN".to_string(), Some("t0k".to_string()))].into(),
        ..Default::default()
    };
    let plan = Plan::build(&s, &environment(&base), &inputs).unwrap();

    let r = lt_core::check::check(&plan).await.unwrap();
    let echoed: Value = serde_json::from_str(r.steps[0].body.as_deref().unwrap()).unwrap();
    // --var pisa a `vars`; los valores se codifican según dónde van.
    assert_eq!(echoed["target"], "/echo/items/7?by=bob+mar%C3%ADa");
    assert_eq!(echoed["auth"], "Bearer t0k");
    // "{{id}}" conserva el tipo número; "n-{{id}}" es un string.
    let body: Value = serde_json::from_str(echoed["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({ "id": 7, "label": "n-7", "n": 3 }));
}

#[test]
fn reports_undefined_variables_and_secrets() {
    let s = echo_scenario(
        "defaults: { headers: { X-A: \"{{nope}}\", X-B: \"{{env.MISSING}}\", X-C: \"{{env.EMPTY}}\" } }\n",
    );
    let inputs = Inputs {
        vars: Default::default(),
        secrets: [("EMPTY".to_string(), None)].into(),
        ..Default::default()
    };
    let errors = Plan::build(&s, &environment("http://127.0.0.1:1"), &inputs)
        .err()
        .unwrap()
        .join("\n");
    assert!(errors.contains("`{{nope}}` no está definida"), "{errors}");
    assert!(
        errors.contains("`{{env.MISSING}}` no está declarado"),
        "{errors}"
    );
    assert!(errors.contains("`EMPTY` no tiene valor"), "{errors}");
}

/// API mínima con login: POST /login → token; las demás rutas exigen `Bearer abc`.
async fn auth_server() -> String {
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    fn authorized(h: &HeaderMap) -> bool {
        h.get("authorization").is_some_and(|v| v == "Bearer abc")
    }
    let app = Router::new()
        .route(
            "/login",
            post(|Json(body): Json<Value>| async move {
                if body["password"] == "ok" {
                    (StatusCode::OK, Json(json!({ "data": { "token": "abc" } })))
                } else {
                    (StatusCode::UNAUTHORIZED, Json(json!({ "error": "no" })))
                }
            }),
        )
        .route(
            "/orders",
            post(|h: HeaderMap| async move {
                if !authorized(&h) {
                    return (
                        StatusCode::UNAUTHORIZED,
                        [("location", "")],
                        Json(json!({})),
                    );
                }
                (
                    StatusCode::CREATED,
                    [("location", "/orders/5")],
                    Json(json!({ "id": 5 })),
                )
            }),
        )
        .route(
            "/orders/{id}",
            get(
                |h: HeaderMap, axum::extract::Path(id): axum::extract::Path<u32>| async move {
                    match (authorized(&h), id) {
                        (false, _) => StatusCode::UNAUTHORIZED,
                        (true, 5) => StatusCode::OK,
                        _ => StatusCode::NOT_FOUND,
                    }
                },
            ),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{}", addr.port())
}

fn chain_scenario(password: &str, order_path: &str) -> lt_core::scenario::Scenario {
    let yaml = format!(
        "version: 1
name: chain
setup:
  - name: login
    request: {{ method: POST, url: /login, json: {{ password: {password} }} }}
    extract:
      token: {{ json: $.data.token }}
defaults:
  headers: {{ Authorization: \"Bearer {{{{token}}}}\" }}
flows:
  buy:
    steps:
      - name: create_order
        request: {{ method: POST, url: /orders }}
        extract:
          order_id: {{ json: {order_path} }}
          location: {{ header: location }}
      - name: get_order
        request: {{ method: GET, url: \"/orders/{{{{order_id}}}}\" }}
load:
  executor: constant_rate
  rate: 20/s
  duration: 1s
  max_vus: 10
"
    );
    parse_scenario(&yaml).unwrap_or_else(|r| panic!("{r:?}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_and_extract_chain_requests() {
    let base = auth_server().await;
    let plan = Plan::build(
        &chain_scenario("ok", "$.id"),
        &environment(&base),
        &Default::default(),
    )
    .unwrap();

    let check = lt_core::check::check(&plan).await.unwrap();
    assert!(check.ok, "{check:#?}");
    assert_eq!(check.steps[0].extracted["token"], "abc");
    assert_eq!(check.steps[1].extracted["order_id"], "5");
    assert!(check.steps[2].url.ends_with("/orders/5"));

    let r = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    assert!(r.aborted.is_none());
    assert_eq!(r.setup[0].extracted, ["token"]);
    assert_eq!(r.totals.requests, 40);
    assert_eq!(r.totals.failed + r.totals.non_2xx, 0, "{:#?}", r.steps);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_setup_aborts_without_load() {
    let base = auth_server().await;
    let plan = Plan::build(
        &chain_scenario("mal", "$.id"),
        &environment(&base),
        &Default::default(),
    )
    .unwrap();
    let r = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    assert!(
        r.aborted.as_deref().unwrap().contains("respondió 401"),
        "{:?}",
        r.aborted
    );
    assert_eq!(r.totals.requests, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_extract_stops_the_iteration() {
    let base = auth_server().await;
    let plan = Plan::build(
        &chain_scenario("ok", "$.missing"),
        &environment(&base),
        &Default::default(),
    )
    .unwrap();
    let r = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    let create = &r.steps[0];
    assert_eq!(create.failures.get(&FailureKind::Extract), Some(&20));
    assert!(create.error_samples[0].contains("$.missing no existe"));
    assert_eq!(r.steps[1].requests, 0);
}

#[test]
fn undefined_extract_variable_is_reported() {
    // `order_id` se usa antes de extraerse.
    let s = chain_scenario("ok", "$.id");
    let mut yaml = serde_json::to_value(&s).unwrap();
    let steps = yaml["flows"]["buy"]["steps"].as_array_mut().unwrap();
    steps.swap(0, 1);
    let swapped: lt_core::scenario::Scenario = serde_json::from_value(yaml).unwrap();
    let errors = Plan::build(
        &swapped,
        &environment("http://127.0.0.1:1"),
        &Default::default(),
    )
    .err()
    .unwrap();
    assert!(
        errors[0].contains("get_order") && errors[0].contains("order_id"),
        "{errors:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_and_thresholds() {
    let base = server().await;
    let yaml = "version: 1
name: t
flows:
  browse:
    weight: 3
    steps:
      - name: ok
        request: { method: GET, url: /ok }
        checks:
          - status: 200
          - body_contains: ok
  broken:
    steps:
      - name: missing
        request: { method: GET, url: /missing }
        checks:
          - status: 404
          - { json: $.id, exists: true }
load:
  executor: constant_rate
  rate: 100/s
  duration: 1s
  max_vus: 50
thresholds:
  - { metric: http.latency.p95, lt: 1s }
  - { metric: http.error_rate, lt: 1% }
  - { metric: http.error_rate, step: ok, lt: 1% }
  - { metric: checks.pass_rate, gt: 80% }
  - { metric: http.rps, gte: 1000 }
";
    let s = parse_scenario(yaml).unwrap_or_else(|r| panic!("{r:?}"));
    let plan = Plan::build(&s, &environment(&base), &Default::default()).unwrap();

    let check = lt_core::check::check(&plan).await.unwrap();
    assert!(check.steps[0].ok);
    // El 404 era esperado, pero `$.id` no existe: el paso falla por el check, no por el status.
    assert!(!check.steps[1].ok);
    assert_eq!(
        check.steps[1].checks[1].reason.as_deref(),
        Some("no existe")
    );

    let r = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    let passed: Vec<(&str, bool)> = r
        .thresholds
        .iter()
        .map(|t| (t.threshold.as_str(), t.passed))
        .collect();
    assert_eq!(
        passed,
        [
            ("http.latency.p95 < 1s", true),
            ("http.error_rate < 1%", false),
            ("http.error_rate{step=ok} < 1%", true),
            ("checks.pass_rate > 80%", true),
            ("http.rps >= 1000", false),
        ]
    );
    assert_eq!(r.thresholds[1].actual, Some(25.0)); // 25 de 100 respondieron 404
    assert!(!r.passed);
    assert_eq!((r.totals.checks_passed, r.totals.checks_failed), (175, 25));
}

#[test]
fn threshold_filters_must_match_a_step() {
    let flows = "  main:\n    steps:\n      - name: a\n        request: { method: GET, url: /x }\n";
    let yaml = format!(
        "version: 1\nname: t\nflows:\n{flows}load:\n  executor: constant_rate\n  rate: 1/s\n  duration: 1s\n  max_vus: 1\n\
         thresholds:\n  - {{ metric: http.rps, step: nope, gt: 1 }}\n"
    );
    let s = parse_scenario(&yaml).unwrap();
    let errors = Plan::build(&s, &environment("http://127.0.0.1:1"), &Default::default())
        .err()
        .unwrap();
    assert!(errors[0].contains("ningún paso"), "{errors:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ramping_rate_follows_its_stages() {
    let base = server().await;
    let flows =
        "  main:\n    steps:\n      - name: ok\n        request: { method: GET, url: /ok }\n";
    let yaml = format!(
        "version: 1\nname: ramp\nflows:\n{flows}load:\n  executor: ramping_rate\n  start_rate: 0/s\n  stages:\n\
         \x20   - {{ target: 100/s, duration: 1s }}\n    - {{ target: 0/s, duration: 1s }}\n  max_vus: 20\n"
    );
    let plan = Plan::build(
        &parse_scenario(&yaml).unwrap(),
        &environment(&base),
        &Default::default(),
    )
    .unwrap();
    let r = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    // Área del triángulo: (100/s × 2 s) / 2 = 100 iteraciones.
    assert_eq!(
        (r.totals.planned, r.totals.started, r.totals.dropped),
        (100, 100, 0)
    );
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
}

#[tokio::test(flavor = "multi_thread")]
async fn constant_vus_loops_and_respects_max_rps() {
    let base = server().await;
    let flows =
        "  main:\n    steps:\n      - name: slow\n        request: { method: GET, url: /slow }\n";
    let yaml = format!(
        "version: 1\nname: vus\nflows:\n{flows}load:\n  executor: constant_vus\n  vus: 2\n  duration: 1s\n"
    );
    let s = parse_scenario(&yaml).unwrap();

    // Sin tope: 2 VUs × (1 s / 300 ms) ≈ 6-8 iteraciones, una detrás de otra.
    let plan = Plan::build(&s, &environment(&base), &Default::default()).unwrap();
    assert_eq!(plan.max_request_rate, None);
    let r = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    assert!((6..=8).contains(&r.totals.started), "{}", r.totals.started);
    assert_eq!(r.totals.planned, r.totals.started);
    assert!(r.scheduler_lag_ms.is_none());

    // Con limits.max_rps = 2: a lo sumo ~2 inicios por segundo, y se avisa.
    let mut env = environment(&base);
    env.limits.max_rps = Some(2.0);
    let plan = Plan::build(&s, &env, &Default::default()).unwrap();
    assert!(plan.limit_violations(&env).is_empty());
    let r = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    assert!(r.totals.started <= 3, "{}", r.totals.started);
    assert!(
        r.warnings.iter().any(|w| w.contains("max_rps")),
        "{:?}",
        r.warnings
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn records_a_per_second_series() {
    let base = server().await;
    let flows =
        "  main:\n    steps:\n      - name: slow\n        request: { method: GET, url: /slow }\n";
    let r = run(
        flows,
        "  rate: 100/s\n  duration: 2s\n  max_vus: 100\n",
        &base,
    )
    .await;

    let s = &r.series;
    assert_eq!(s.points.iter().map(|p| p.started).sum::<u64>(), 200);
    assert_eq!(s.points.iter().map(|p| p.requests).sum::<u64>(), 200);
    assert!(
        s.points[0].started >= 95 && s.points[1].started >= 95,
        "{:?}",
        s.points
    );
    // Las respuestas de /slow terminan 300 ms después: la última ventana es la 2.
    assert_eq!(s.points.len(), 3);
    let slow = &s.steps[0];
    assert_eq!(slow.name, "slow");
    assert!(slow.points[1].p50.unwrap() >= 300.0);
}

const USERS_CSV: &[u8] = b"id,email\n1,ana@x.com\n2,beto@x.com\n";

/// `main` usa el dataset `users`; `other`, no.
fn data_scenario(setup: &str, extra: &str) -> (lt_core::scenario::Scenario, Inputs) {
    let yaml = format!(
        "version: 1\nname: data\ndata:\n  users: {{ file: data/users.csv }}\n{setup}flows:
  main:
    steps:
      - name: user
        request:
          method: POST
          url: /echo/u/{{{{users.id}}}}
          json: {{ email: \"{{{{users.email}}}}\"{extra} }}
        checks:
          - json: $.target
            equals: /echo/u/{{{{users.id}}}}
  other:
    steps:
      - name: plain
        request: {{ method: GET, url: /ok }}
load:\n  executor: constant_rate\n  rate: 40/s\n  duration: 1s\n  max_vus: 10\n"
    );
    let s = parse_scenario(&yaml).unwrap_or_else(|r| panic!("{r:?}"));
    let users = lt_core::data::Dataset::parse("users", &s.data["users"], USERS_CSV).unwrap();
    let inputs = Inputs {
        data: [("users".to_string(), users)].into(),
        ..Default::default()
    };
    (s, inputs)
}

#[test]
fn datasets_hand_rows_in_order_only_to_flows_that_use_them() {
    let (s, inputs) = data_scenario("", "");
    let plan = Plan::build(&s, &environment("http://127.0.0.1:1"), &inputs).unwrap();
    let ids: Vec<Value> = (0..3)
        .map(|_| plan.row_vars(&plan.flows[0])["users.id"].clone())
        .collect();
    assert_eq!(ids, [json!("1"), json!("2"), json!("1")]);
    assert_eq!(
        plan.row_vars(&plan.flows[0])["users.email"],
        json!("beto@x.com")
    );
    assert!(plan.row_vars(&plan.flows[1]).is_empty());
}

#[test]
fn approval_signs_the_dataset_contents() {
    let env = environment("http://127.0.0.1:1");
    let (s, inputs) = data_scenario("", "");
    let canonical = Plan::build(&s, &env, &inputs).unwrap().canonical(&env);
    assert_eq!(canonical["data"][0]["file"], "data/users.csv");
    assert_eq!(canonical["data"][0]["pick"], "sequential");

    let mut changed = inputs.clone();
    changed.data.insert(
        "users".into(),
        lt_core::data::Dataset::parse("users", &s.data["users"], b"id,email\n1,otro@x.com\n")
            .unwrap(),
    );
    let other = Plan::build(&s, &env, &changed).unwrap().canonical(&env);
    assert_ne!(canonical, other);

    // Sin `data`, la firma no cambia respecto de las versiones anteriores.
    let plain = Plan::build(&echo_scenario(""), &env, &Default::default()).unwrap();
    assert!(plain.canonical(&env).get("data").is_none());
}

#[test]
fn datasets_are_not_available_in_setup_and_columns_must_exist() {
    let setup =
        "setup:\n  - name: login\n    request: { method: GET, url: \"/login/{{users.id}}\" }\n";
    let (s, inputs) = data_scenario(setup, ", x: \"{{users.nope}}\"");
    let errors = Plan::build(&s, &environment("http://127.0.0.1:1"), &inputs)
        .err()
        .unwrap()
        .join("\n");
    assert!(
        errors.contains("login: `{{users.id}}`: los datasets no se pueden usar en `setup`"),
        "{errors}"
    );
    assert!(
        errors.contains(
            "`{{users.nope}}`: el dataset `users` no tiene la columna `nope` (tiene: id, email)"
        ),
        "{errors}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn each_iteration_uses_one_row_in_all_its_steps() {
    lt_core::timer::high_resolution();
    let base = server().await;
    let (s, inputs) = data_scenario("", "");
    let plan = Plan::build(&s, &environment(&base), &inputs).unwrap();

    let checked = lt_core::check::check(&plan).await.unwrap();
    assert!(checked.ok, "{checked:?}");
    let echoed: Value = serde_json::from_str(checked.steps[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(echoed["target"], "/echo/u/1");
    assert_eq!(echoed["body"], r#"{"email":"ana@x.com"}"#);

    let r = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    assert!(r.totals.checks_passed > 0);
    assert_eq!(r.totals.checks_failed, 0);
}

const THINK_FLOWS: &str = "  main:
    steps:
      - name: first
        request: { method: GET, url: /ok }
        think: 200ms
      - name: second
        request: { method: GET, url: /ok }
";

#[tokio::test(flavor = "multi_thread")]
async fn think_time_holds_the_vu_but_not_the_latency() {
    lt_core::timer::high_resolution();
    let base = server().await;
    let yaml = format!(
        "version: 1\nname: think\nflows:\n{THINK_FLOWS}\
         load:\n  executor: constant_vus\n  vus: 1\n  duration: 1s\n  graceful_stop: 1s\n"
    );
    let s = parse_scenario(&yaml).unwrap_or_else(|r| panic!("{r:?}"));
    let plan = Plan::build(&s, &environment(&base), &Default::default()).unwrap();
    assert_eq!(
        plan.think_summary().as_deref(),
        Some("después de first 200ms")
    );

    let r = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    // Un VU con 200 ms de pausa por iteración: unas 5 en un segundo, no cientos.
    assert!((3..=7).contains(&r.totals.started), "{:?}", r.totals);
    let second = r.steps.iter().find(|s| s.name == "second").unwrap();
    assert!(
        second.latency_ms.as_ref().unwrap().max < 100.0,
        "{second:?}"
    );
}

#[test]
fn approval_signs_think_time_only_when_there_is_one() {
    let env = environment("http://127.0.0.1:1");
    let build = |flows: &str| {
        let s = scenario(flows, "  rate: 1/s\n  duration: 1s\n  max_vus: 1\n");
        Plan::build(&s, &env, &Default::default())
            .unwrap()
            .canonical(&env)
    };
    let with = build(THINK_FLOWS);
    assert_eq!(with["flows"][0]["steps"][0]["think"], "200ms");
    assert!(with["flows"][0]["steps"][1].get("think").is_none());
    let without = build(&THINK_FLOWS.replace("        think: 200ms\n", ""));
    assert!(without["flows"][0]["steps"][0].get("think").is_none());
}
