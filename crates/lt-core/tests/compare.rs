//! Validación de la comparación con el motor real: el mismo servidor dos veces no
//! cambia, y un servidor más lento empeora de forma consistente segundo a segundo.

use std::{sync::Arc, time::Duration};

use axum::{Router, routing::get};
use lt_core::{
    compare::{Change, Run, Verdict, compare},
    config::Environment,
    engine::{self, Progress, plan::Plan},
    validate::parse_scenario,
};
use serde_json::json;
use tokio::net::TcpListener;

/// Servidor que tarda `delay_ms` en cada respuesta.
async fn serve(delay_ms: u64) -> String {
    let app = Router::new().route(
        "/work",
        get(move || async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            "ok"
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{}", addr.port())
}

/// Corre 7 s a 50/s y arma la corrida como la guarda `lt run`.
async fn run(id: &str, base_url: &str) -> Run {
    lt_core::timer::high_resolution();
    let yaml = "version: 1\nname: lab\nflows:\n  main:\n    steps:\n      - name: work\n        request: { method: GET, url: /work }\n\
                load:\n  executor: constant_rate\n  rate: 50/s\n  duration: 7s\n  max_vus: 50\n";
    let env: Environment = serde_saphyr::from_str(&format!(
        "base_url: {base_url}\nallowed_hosts: [127.0.0.1]\n"
    ))
    .unwrap();
    let plan = Plan::build(&parse_scenario(yaml).unwrap(), &env, &Default::default()).unwrap();
    let load = plan.load.canonical();
    let result = engine::run(Arc::new(plan), Arc::new(Progress::default())).await;
    let mut doc = json!({ "run_id": id, "scenario": "lab", "environment": "local", "load": load });
    let serde_json::Value::Object(result) = serde_json::to_value(&result).unwrap() else {
        unreachable!()
    };
    doc.as_object_mut().unwrap().extend(result);
    serde_json::from_value(doc).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_server_twice_does_not_change() {
    let url = serve(5).await;
    let a = run("a", &url).await;
    let b = run("b", &url).await;
    let c = compare(&a, &b).unwrap();

    assert!(c.same_load);
    assert!(c.paired_windows >= 6, "{}", c.paired_windows);
    assert!(
        matches!(c.steps[0].change, Change::Same | Change::Uncertain),
        "{c:#?}"
    );
    assert!(c.regressions.is_empty(), "{:?}", c.regressions);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slower_server_is_worse_and_a_faster_one_better() {
    let fast = serve(5).await;
    let slow = serve(30).await;
    let a = run("fast", &fast).await;
    let b = run("slow", &slow).await;

    let c = compare(&a, &b).unwrap();
    assert_eq!(c.steps[0].change, Change::Worse, "{c:#?}");
    assert_eq!(c.verdict, Verdict::Worse);
    let t = c.steps[0].sign_test.unwrap();
    assert!(t.p_value < 0.05 && t.worse > t.better, "{t:?}");

    let c = compare(&b, &a).unwrap();
    assert_eq!(c.steps[0].change, Change::Better, "{c:#?}");
    assert_eq!(c.verdict, Verdict::Better);
}
