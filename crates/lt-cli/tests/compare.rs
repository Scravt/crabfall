//! `lt compare` desde el binario: elige la corrida base y sale con 1 si hay regresiones.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};

fn project() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lt-compare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".loadtest")).unwrap();
    std::fs::write(
        dir.join(".loadtest/config.yaml"),
        "version: 1\nenvironments:\n  local:\n    base_url: http://127.0.0.1:9\n    allowed_hosts: [127.0.0.1]\n",
    )
    .unwrap();
    dir
}

/// Guarda una corrida de 8 segundos con un paso `a` cuyo p95 es `p95` en cada segundo.
fn save_run(dir: &Path, id: &str, scenario: &str, p95: f64) {
    let points: Vec<Value> = (0..8)
        .map(|t| json!({ "t": t, "requests": 10, "errors": 0, "p50": p95 / 2.0, "p95": p95, "p99": p95 }))
        .collect();
    let global: Vec<Value> = (0..8)
        .map(|t| json!({ "t": t, "started": 10, "dropped": 0, "requests": 10, "errors": 0, "p50": p95, "p95": p95, "p99": p95 }))
        .collect();
    let doc = json!({
        "format": 1, "run_id": id, "scenario": scenario, "environment": "local",
        "load": { "executor": "constant_rate", "rate": "10/s", "duration": "8s" },
        "passed": true,
        "totals": { "planned": 80, "started": 80, "dropped": 0, "interrupted": 0, "requests": 80,
                    "failed": 0, "non_2xx": 0, "iteration_rate": 10.0, "request_rate": 10.0 },
        "steps": [{ "name": "a", "requests": 80, "failed": 0, "non_2xx": 0,
                    "latency_ms": { "count": 80, "mean": p95, "p50": p95 / 2.0, "p90": p95, "p95": p95,
                                    "p99": p95, "p999": p95, "max": p95 } }],
        "warnings": [], "findings": [],
        "series": { "window_s": 1, "points": global, "steps": [{ "name": "a", "points": points }] },
    });
    let run = dir.join(".loadtest/runs").join(id);
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("results.json"), doc.to_string()).unwrap();
}

fn lt(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lt"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn json_out(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

#[test]
fn compare_picks_the_previous_run_of_the_same_scenario() {
    let dir = project();
    save_run(&dir, "20260101-000000", "s", 10.0);
    save_run(&dir, "20260101-000100", "other", 10.0);
    save_run(&dir, "20260101-000200", "s", 40.0);

    // Sin argumentos: la última (`s`, más lenta) contra la anterior de `s`, salteando `other`.
    let out = lt(&dir, &["compare", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let c = json_out(&out);
    assert_eq!(c["base"]["run_id"], "20260101-000000");
    assert_eq!(c["new"]["run_id"], "20260101-000200");
    assert_eq!(c["verdict"], "worse");
    assert_eq!(c["steps"][0]["change"], "worse");

    // Al revés, mejora y sale con 0.
    let out = lt(&dir, &["compare", "20260101-000200", "20260101-000000"]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Veredicto: mejoró"), "{text}");

    // Una corrida que no existe es un error de uso.
    let out = lt(&dir, &["compare", "20991231-000000"]);
    assert_eq!(out.status.code(), Some(2));
}
