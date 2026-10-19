//! Cookies, redirects, compresión y formularios (ADR-014), contra un servidor axum real.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
};

use axum::{
    Form, Json, Router,
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Redirect},
    routing::{any, get, post},
};
use crabfall_core::{
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
use tower_http::compression::CompressionLayer;

async fn server() -> String {
    let sessions = Arc::new(AtomicU64::new(0));
    let big: Value = json!({ "items": (0..500).map(|i| json!({ "id": i, "name": "producto de prueba" })).collect::<Vec<_>>() });
    let app = Router::new()
        // Cada login abre una sesión distinta: así se ve que cada VU tiene la suya.
        .route(
            "/login",
            post(move || {
                let n = sessions.fetch_add(1, Relaxed) + 1;
                async move {
                    (
                        [(header::SET_COOKIE, format!("sid=s{n}; Path=/; HttpOnly"))],
                        Json(json!({ "ok": true })),
                    )
                }
            }),
        )
        .route(
            "/me",
            get(|headers: HeaderMap| async move {
                let cookie = headers
                    .get(header::COOKIE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                match cookie.split("; ").find_map(|c| c.strip_prefix("sid=")) {
                    Some(sid) => Json(json!({ "sid": sid, "cookie": cookie })).into_response(),
                    None => StatusCode::UNAUTHORIZED.into_response(),
                }
            }),
        )
        .route("/old", get(|| async { Redirect::permanent("/new") }))
        .route("/new", get(|| async { "nuevo" }))
        .route("/chain/{n}", get(chain))
        .route("/loop", get(|| async { Redirect::temporary("/loop") }))
        .route(
            "/away",
            get(|| async { Redirect::temporary("http://10.255.255.1:9/x") }),
        )
        .route(
            "/submit-302",
            post(|| async { (StatusCode::FOUND, [(header::LOCATION, "/echo")]) }),
        )
        .route(
            "/submit-307",
            post(|| async { Redirect::temporary("/echo") }),
        )
        .route("/echo", any(echo))
        .route(
            "/form",
            post(|Form(fields): Form<HashMap<String, String>>| async move { Json(json!(fields)) }),
        )
        .route("/big", get(move || async move { Json(big) }))
        .layer(CompressionLayer::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{}", addr.port())
}

async fn chain(axum::extract::Path(n): axum::extract::Path<u32>) -> axum::response::Response {
    if n == 0 {
        "fin".into_response()
    } else {
        Redirect::to(&format!("/chain/{}", n - 1)).into_response()
    }
}

async fn echo(method: Method, headers: HeaderMap, body: String) -> Json<Value> {
    Json(json!({
        "method": method.as_str(),
        "body": body,
        "auth": headers.get(header::AUTHORIZATION).map(|v| v.to_str().unwrap()),
    }))
}

fn plan(base: &str, body: &str) -> Plan {
    let yaml = format!("version: 1\nname: http\n{body}");
    let s = parse_scenario(&yaml).unwrap_or_else(|r| panic!("{r:?}"));
    let env: Environment = serde_saphyr::from_str(&format!(
        "base_url: {base}\nallowed_hosts: [127.0.0.1, 10.255.255.1]\n"
    ))
    .unwrap();
    Plan::build(&s, &env, &Inputs::default()).unwrap_or_else(|e| panic!("{e:?}"))
}

/// Un flow de un solo paso; `request` va tal cual en el YAML.
fn one_step(base: &str, request: &str, checks: &str) -> Plan {
    plan(
        base,
        &format!(
            "flows:\n  main:\n    steps:\n      - name: step\n        request: {request}\n{checks}load:\n  executor: constant_rate\n  rate: 2/s\n  duration: 1s\n  max_vus: 1\n"
        ),
    )
}

async fn check_one(plan: &Plan) -> crabfall_core::check::CheckStep {
    let mut r = crabfall_core::check::check(plan).await.unwrap();
    r.steps.remove(0)
}

fn body(step: &crabfall_core::check::CheckStep) -> Value {
    serde_json::from_str(step.body.as_deref().unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_vu_keeps_its_own_session_cookie() {
    let base = server().await;
    let p = plan(
        &base,
        "vu_setup:
  - name: login
    request: { method: POST, url: /login }
flows:
  main:
    steps:
      - name: me
        request: { method: GET, url: /me }
        checks: [{ status: 200 }]
load: { executor: constant_vus, vus: 3, duration: 1s }
",
    );
    let r = engine::run(Arc::new(p), Arc::new(Progress::default())).await;
    let me = &r.steps[0];
    assert!(me.requests > 0);
    assert_eq!(me.non_2xx, 0, "sin la cookie de su login, /me da 401");
    assert_eq!(r.totals.checks_failed, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn cookies_false_sends_none() {
    let base = server().await;
    let p = plan(
        &base,
        "cookies: false
flows:
  main:
    steps:
      - name: login
        request: { method: POST, url: /login }
      - name: me
        request: { method: GET, url: /me }
load: { executor: constant_rate, rate: 2/s, duration: 1s, max_vus: 1 }
",
    );
    assert_eq!(p.http_summary().split(" · ").next(), Some("sin cookies"));
    let r = engine::run(Arc::new(p), Arc::new(Progress::default())).await;
    assert_eq!(
        r.steps[1].status_codes.get("401"),
        Some(&r.steps[1].requests)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cookie_header_of_the_scenario_is_kept_next_to_the_jar() {
    let base = server().await;
    let p = plan(
        &base,
        "flows:
  main:
    steps:
      - name: login
        request: { method: POST, url: /login }
      - name: me
        request: { method: GET, url: /me, headers: { cookie: theme=dark } }
load: { executor: constant_rate, rate: 1/s, duration: 1s, max_vus: 1 }
",
    );
    let r = crabfall_core::check::check(&p).await.unwrap();
    let me = body(&r.steps[1]);
    assert_eq!(me["cookie"], "theme=dark; sid=s1");
}

#[tokio::test(flavor = "multi_thread")]
async fn follows_redirects_and_reports_the_final_response() {
    let base = server().await;
    let step = check_one(&one_step(&base, "{ method: GET, url: /old }", "")).await;
    assert_eq!(step.status, Some(200));
    assert_eq!(step.body.as_deref(), Some("nuevo"));
    assert_eq!(step.redirects, [format!("{base}/new")]);

    let step = check_one(&one_step(&base, "{ method: GET, url: /chain/3 }", "")).await;
    assert_eq!(step.redirects.len(), 3);
    assert_eq!(step.body.as_deref(), Some("fin"));
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_limit_returns_the_3xx() {
    let base = server().await;
    let step = check_one(&one_step(
        &base,
        "{ method: GET, url: /old, redirects: 0 }",
        "",
    ))
    .await;
    assert_eq!(step.status, Some(308));
    assert!(step.redirects.is_empty());

    let step = check_one(&one_step(&base, "{ method: GET, url: /loop }", "")).await;
    assert_eq!(step.status, Some(307));
    assert_eq!(step.redirects.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_302_becomes_get_and_307_keeps_the_body() {
    let base = server().await;
    let step = check_one(&one_step(
        &base,
        "{ method: POST, url: /submit-302, json: { a: 1 } }",
        "",
    ))
    .await;
    assert_eq!(body(&step)["method"], "GET");
    assert_eq!(body(&step)["body"], "");

    let step = check_one(&one_step(
        &base,
        "{ method: POST, url: /submit-307, json: { a: 1 } }",
        "",
    ))
    .await;
    assert_eq!(body(&step)["method"], "POST");
    assert_eq!(body(&step)["body"], r#"{"a":1}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn check_stays_on_loopback_even_through_a_redirect() {
    let base = server().await;
    // 10.255.255.1 está en allowed_hosts, pero el check no sale de loopback ni por un redirect.
    let step = check_one(&one_step(&base, "{ method: GET, url: /away }", "")).await;
    let f = step.failure.unwrap();
    assert_eq!(f.kind, FailureKind::Connect);
    assert!(f.message.contains("fuera de loopback"), "{}", f.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_outside_allowed_hosts_fails() {
    let base = server().await;
    let yaml = "version: 1\nname: http\nflows:\n  main:\n    steps:\n      - name: step\n        request: { method: GET, url: /away }\nload: { executor: constant_rate, rate: 1/s, duration: 1s, max_vus: 1 }\n";
    let s = parse_scenario(yaml).unwrap();
    let env: Environment =
        serde_saphyr::from_str(&format!("base_url: {base}\nallowed_hosts: [127.0.0.1]\n")).unwrap();
    let p = Plan::build(&s, &env, &Inputs::default()).unwrap();
    let f = check_one(&p).await.failure.unwrap();
    assert_eq!(f.kind, FailureKind::Redirect);
    assert!(f.message.contains("10.255.255.1"), "{}", f.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn compressed_responses_are_decoded_but_bytes_are_the_wire() {
    let base = server().await;
    // Por defecto pide gzip y br: el servidor elige br, y checks y extract ven el JSON.
    let p = one_step(
        &base,
        "{ method: GET, url: /big }",
        "        checks: [{ json: \"$.items[499].id\", equals: 499 }]\n",
    );
    assert!(
        p.http_summary().ends_with("acepta gzip, br"),
        "{}",
        p.http_summary()
    );
    let step = check_one(&p).await;
    assert!(step.ok, "{step:?}");

    // Sin compresión (identity), el mismo JSON pesa varias veces más en el cable.
    let plain = one_step(
        &base,
        "{ method: GET, url: /big, headers: { accept-encoding: identity } }",
        "        checks: [{ json: \"$.items[499].id\", equals: 499 }]\n",
    );
    let gzip = one_step(
        &base,
        "{ method: GET, url: /big, headers: { accept-encoding: gzip } }",
        "        checks: [{ json: \"$.items[499].id\", equals: 499 }]\n",
    );
    let run =
        |p: Plan| async move { engine::run(Arc::new(p), Arc::new(Progress::default())).await };
    let (plain, gzip, br) = (run(plain).await, run(gzip).await, run(p).await);
    for r in [&plain, &gzip, &br] {
        assert_eq!(r.totals.checks_failed, 0);
    }
    let per =
        |r: &crabfall_core::metrics::RunResult| r.steps[0].bytes_received / r.steps[0].requests;
    assert!(
        per(&gzip) * 5 < per(&plain),
        "gzip {} vs {}",
        per(&gzip),
        per(&plain)
    );
    assert!(
        per(&br) * 5 < per(&plain),
        "br {} vs {}",
        per(&br),
        per(&plain)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn form_bodies_are_url_encoded() {
    let base = server().await;
    let p = one_step(
        &base,
        "{ method: POST, url: /form, form: { user: \"ana pérez\", age: 30, remember: true, q: \"a&b=c\" } }",
        "",
    );
    let step = check_one(&p).await;
    let got = body(&step);
    assert_eq!(got["user"], "ana pérez");
    assert_eq!(got["age"], "30");
    assert_eq!(got["remember"], "true");
    assert_eq!(got["q"], "a&b=c");
}

#[test]
fn signature_changes_with_redirects_and_cookies_only_when_set() {
    let base = "http://127.0.0.1:1";
    let env: Environment =
        serde_saphyr::from_str(&format!("base_url: {base}\nallowed_hosts: [127.0.0.1]\n")).unwrap();
    let canonical = |extra_root: &str, request: &str| {
        let yaml = format!(
            "version: 1\nname: s\n{extra_root}flows:\n  main:\n    steps:\n      - name: a\n        request: {request}\nload: {{ executor: constant_rate, rate: 1/s, duration: 1s, max_vus: 1 }}\n"
        );
        let s = parse_scenario(&yaml).unwrap();
        Plan::build(&s, &env, &Inputs::default())
            .unwrap()
            .canonical(&env)
    };
    let base_c = canonical("", "{ method: GET, url: /a }");
    assert!(base_c.get("cookies").is_none());
    assert!(base_c["flows"][0]["steps"][0].get("redirects").is_none());
    assert_eq!(
        base_c,
        canonical("", "{ method: GET, url: /a, redirects: 10 }")
    );
    assert_ne!(
        base_c,
        canonical("", "{ method: GET, url: /a, redirects: 0 }")
    );
    assert_ne!(
        base_c,
        canonical("cookies: false\n", "{ method: GET, url: /a }")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn authorization_does_not_follow_a_redirect_to_another_host() {
    // Escucha en todas las interfaces: 127.0.0.1 y 127.0.0.2 son dos hosts distintos.
    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app =
        Router::new()
            .route(
                "/cross",
                get(move || async move {
                    Redirect::temporary(&format!("http://127.0.0.2:{port}/echo"))
                }),
            )
            .route(
                "/same",
                get(move || async move {
                    Redirect::temporary(&format!("http://127.0.0.1:{port}/echo"))
                }),
            )
            .route("/echo", any(echo));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let env: Environment = serde_saphyr::from_str(&format!(
        "base_url: http://127.0.0.1:{port}\nallowed_hosts: [127.0.0.1, 127.0.0.2]\n"
    ))
    .unwrap();
    let auth_after = |url: &'static str| {
        let env = env.clone();
        async move {
            let yaml = format!(
                "version: 1\nname: s\nflows:\n  main:\n    steps:\n      - name: a\n        request: {{ method: GET, url: {url}, headers: {{ authorization: Bearer x }} }}\nload: {{ executor: constant_rate, rate: 1/s, duration: 1s, max_vus: 1 }}\n"
            );
            let p = Plan::build(&parse_scenario(&yaml).unwrap(), &env, &Inputs::default()).unwrap();
            body(&check_one(&p).await)["auth"].clone()
        }
    };
    assert_eq!(auth_after("/same").await, "Bearer x");
    assert_eq!(auth_after("/cross").await, Value::Null);
}
