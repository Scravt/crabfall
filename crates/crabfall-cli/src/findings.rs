//! `crabfall findings`: hallazgos de una corrida y resumen compacto para agentes.

use crabfall_core::project::Project;
use serde_json::{Value, json};

use crate::{JSON_FORMAT, display, exit, runs};

pub fn cmd_findings(run: Option<&str>, as_json: bool) -> u8 {
    let Some(project) = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd))
    else {
        eprintln!(
            "{}",
            crabfall_core::tr!(
                "✗ no .crabfall/ found in this directory or its parents",
                "✗ no encontré .crabfall/ en este directorio ni en los superiores"
            )
        );
        return exit::INVALID;
    };
    let file = match runs::file(&project, run) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("✗ {e}");
            return exit::INVALID;
        }
    };
    let doc = match runs::read(&file) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("✗ {e}");
            return exit::INVALID;
        }
    };

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary(&doc, &file)).unwrap()
        );
    } else {
        println!(
            "{} · {} · {}",
            doc["run_id"].as_str().unwrap_or("?"),
            doc["scenario"].as_str().unwrap_or("?"),
            doc["environment"].as_str().unwrap_or("?")
        );
        print_findings(&doc["findings"]);
    }
    exit::OK
}

/// Lo que un agente necesita para analizar: sin series ni muestras crudas.
fn summary(doc: &Value, file: &std::path::Path) -> Value {
    let steps: Vec<Value> = doc["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            json!({
                "name": s["name"],
                "flow": s["flow"],
                "method": s["method"],
                "url": s["url"],
                "source": s["source"],
                "tags": s["tags"],
                "requests": s["requests"],
                "failed": s["failed"],
                "non_2xx": s["non_2xx"],
                "bytes_sent": s["bytes_sent"],
                "bytes_received": s["bytes_received"],
                "rate_limit": s["rate_limit"],
                "status_codes": s["status_codes"],
                "failures": s["failures"],
                "latency_ms": pick(&s["latency_ms"], &["p50", "p95", "p99", "max"]),
                "ttfb_ms": pick(&s["ttfb_ms"], &["p50", "p95", "p99"]),
                "receive_ms": pick(&s["receive_ms"], &["p50", "p95", "p99"]),
                "checks": s["checks"],
            })
        })
        .collect();
    json!({
        "format": JSON_FORMAT,
        "results_file": display(file),
        "run_id": doc["run_id"],
        "scenario": doc["scenario"],
        "environment": doc["environment"],
        "passed": doc["passed"],
        "aborted": doc["aborted"],
        "stopped_at_s": doc["stopped_at_s"],
        "load": doc["load"],
        "totals": doc["totals"],
        "thresholds": doc["thresholds"],
        "warnings": doc["warnings"],
        "diagnosis": doc["diagnosis"],
        "findings": doc["findings"],
        "steps": steps,
    })
}

fn pick(v: &Value, keys: &[&str]) -> Value {
    if v.is_null() {
        return Value::Null;
    }
    keys.iter()
        .map(|k| (k.to_string(), v[*k].clone()))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

pub fn print_findings(findings: &Value) {
    let list = findings.as_array().map(Vec::as_slice).unwrap_or_default();
    if list.is_empty() {
        println!(
            "{}",
            crabfall_core::tr!(
                "No findings: no known pattern was detected.",
                "Sin hallazgos: no se detectó ningún patrón conocido."
            )
        );
        return;
    }
    println!("{}", crabfall_core::i18n::pick("Findings:", "Hallazgos:"));
    for f in list {
        let mark = match f["severity"].as_str() {
            Some("high") => "✗✗",
            Some("medium") => "✗ ",
            _ => "· ",
        };
        println!("{mark} {}", f["title"].as_str().unwrap_or_default());
        let source = &f["source"];
        let location: Vec<&str> = ["file", "symbol", "operation_id"]
            .iter()
            .filter_map(|k| source[*k].as_str())
            .collect();
        if !location.is_empty() {
            println!(
                "{}",
                crabfall_core::tr!("   code: {}", "   código: {}", location.join(" · "))
            );
        }
        for cause in f["causes"].as_array().into_iter().flatten().take(3) {
            println!("   - {}", cause.as_str().unwrap_or_default());
        }
    }
}
