//! `lt findings`: hallazgos de una corrida y resumen compacto para agentes.

use std::path::PathBuf;

use lt_core::project::Project;
use serde_json::{Value, json};

use crate::{JSON_FORMAT, display, exit};

pub fn cmd_findings(run: Option<&str>, as_json: bool) -> u8 {
    let Some(project) = std::env::current_dir()
        .ok()
        .and_then(|cwd| Project::discover(&cwd))
    else {
        eprintln!("✗ no encontré .loadtest/ en este directorio ni en los superiores");
        return exit::INVALID;
    };
    let file = match run_file(&project, run) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("✗ {e}");
            return exit::INVALID;
        }
    };
    let doc: Value = match std::fs::read_to_string(&file)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("✗ no se pudo leer {}: {e}", display(&file));
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

/// `.loadtest/runs/<id>/results.json`; sin id, la corrida más reciente.
fn run_file(project: &Project, run: Option<&str>) -> Result<PathBuf, String> {
    let runs = project.runs_dir();
    let id = match run {
        Some(id) if id != "latest" => id.to_string(),
        _ => std::fs::read_dir(&runs)
            .map_err(|_| "todavía no hay corridas en .loadtest/runs/".to_string())?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("results.json").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .max()
            .ok_or("todavía no hay corridas en .loadtest/runs/")?,
    };
    let file = runs.join(&id).join("results.json");
    if file.is_file() {
        Ok(file)
    } else {
        Err(format!("no existe la corrida `{id}`"))
    }
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
                "status_codes": s["status_codes"],
                "failures": s["failures"],
                "latency_ms": pick(&s["latency_ms"], &["p50", "p95", "p99", "max"]),
                "ttfb_ms": pick(&s["ttfb_ms"], &["p50", "p95", "p99"]),
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
        "load": doc["load"],
        "totals": doc["totals"],
        "thresholds": doc["thresholds"],
        "warnings": doc["warnings"],
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
        println!("Sin hallazgos: no se detectó ningún patrón conocido.");
        return;
    }
    println!("Hallazgos:");
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
            println!("   código: {}", location.join(" · "));
        }
        for cause in f["causes"].as_array().into_iter().flatten().take(3) {
            println!("   - {}", cause.as_str().unwrap_or_default());
        }
    }
}
