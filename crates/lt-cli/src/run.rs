//! `lt run`: ejecuta un escenario y guarda los resultados en `.loadtest/runs/<id>/`.

use std::{
    io::IsTerminal,
    path::Path,
    sync::{Arc, atomic::Ordering::Relaxed},
    time::{Duration, Instant},
};

use lt_core::{
    engine::{self, Progress, plan::Plan},
    metrics::RunResult,
    project::Project,
    scenario::LoadPlan,
    timer,
};
use serde_json::{Value, json};

use crate::{
    JSON_FORMAT, display, exit,
    prepare::{Prepared, prepare},
};

pub fn cmd_run(target: &crate::Target, as_json: bool) -> u8 {
    let Prepared {
        project,
        env_name,
        env,
        path,
        plan,
    } = match prepare(&target.scenario, target.env.as_deref(), &target.vars) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let violations = plan.limit_violations(&env);
    if !violations.is_empty() {
        eprintln!("✗ {} supera los límites de `{env_name}`:", plan.scenario);
        for v in violations {
            eprintln!("  {v}");
        }
        return exit::REFUSED;
    }
    let plan = Arc::new(plan);

    let LoadPlan::ConstantRate {
        rate,
        duration,
        max_vus,
        graceful_stop,
    } = plan.load;
    let hosts: Vec<&str> = plan.hosts.iter().map(String::as_str).collect();
    eprintln!("{} → {env_name} ({})", plan.scenario, hosts.join(", "));
    eprintln!(
        "  constant_rate {rate} durante {}, max_vus {max_vus} · hasta {:.1} req/s",
        lt_core::units::HumanDuration(duration),
        plan.max_request_rate
    );

    let Ok(approval) = crate::approve::verify_for_run(&project, &plan, &env, &env_name) else {
        return exit::REFUSED;
    };
    eprintln!(
        "  aprobado por {} el {}",
        approval.approved_by, approval.approved_at
    );

    timer::high_resolution();
    let started_at = jiff::Zoned::now();
    let progress = Arc::new(Progress::default());
    let runtime = tokio::runtime::Runtime::new().expect("runtime de tokio");
    let result = runtime.block_on(async {
        let ticker = std::io::stderr()
            .is_terminal()
            .then(|| tokio::spawn(show_progress(progress.clone(), duration + graceful_stop)));
        let result = engine::run(plan.clone(), progress).await;
        if let Some(t) = ticker {
            t.abort();
            eprintln!();
        }
        result
    });

    let run_id = started_at.strftime("%Y%m%d-%H%M%S").to_string();
    let mut doc = results_document(&run_id, &started_at, &path, &env_name, &plan, &result);
    doc["approval"] = json!({
        "hash": approval.hash,
        "approved_by": approval.approved_by,
        "approved_at": approval.approved_at,
    });
    let saved = save(&project, &run_id, &doc);

    if as_json {
        println!("{}", serde_json::to_string_pretty(&doc).unwrap());
    } else {
        print_summary(&result);
    }
    match saved {
        Ok(file) => {
            eprintln!("resultados: {}", display(&file));
            if result.aborted.is_some() {
                exit::FAILED
            } else {
                exit::OK
            }
        }
        Err(e) => {
            eprintln!("✗ no se pudieron guardar los resultados: {e}");
            exit::RUNTIME
        }
    }
}

async fn show_progress(p: Arc<Progress>, total: Duration) {
    let start = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        eprint!(
            "\r  {:>4}s / {}s  iteraciones {}  descartadas {}  requests {}  fallas {}   ",
            start.elapsed().as_secs(),
            total.as_secs(),
            p.started.load(Relaxed),
            p.dropped.load(Relaxed),
            p.requests.load(Relaxed),
            p.failed.load(Relaxed),
        );
    }
}

fn results_document(
    run_id: &str,
    started_at: &jiff::Zoned,
    path: &Path,
    env_name: &str,
    plan: &Plan,
    result: &RunResult,
) -> Value {
    let LoadPlan::ConstantRate {
        rate,
        duration,
        max_vus,
        graceful_stop,
    } = plan.load;
    let mut doc = json!({
        "format": JSON_FORMAT,
        "run_id": run_id,
        "scenario": plan.scenario,
        "scenario_file": display(path),
        "environment": env_name,
        "started_at": started_at.timestamp().to_string(),
        "load": {
            "executor": "constant_rate",
            "rate": rate.to_string(),
            "duration": lt_core::units::HumanDuration(duration).to_string(),
            "max_vus": max_vus,
            "graceful_stop": lt_core::units::HumanDuration(graceful_stop).to_string(),
            "max_request_rate": plan.max_request_rate,
        },
    });
    let Value::Object(result) = serde_json::to_value(result).unwrap() else {
        unreachable!()
    };
    doc.as_object_mut().unwrap().extend(result);
    doc
}

fn save(project: &Project, run_id: &str, doc: &Value) -> std::io::Result<std::path::PathBuf> {
    project.ensure_gitignore()?;
    let dir = project.runs_dir().join(run_id);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("results.json");
    std::fs::write(&file, serde_json::to_string_pretty(doc).unwrap())?;
    Ok(file)
}

fn print_summary(r: &RunResult) {
    let t = &r.totals;
    println!();
    for s in &r.setup {
        let status = s.status.map_or("-".into(), |c| c.to_string());
        let extracted = if s.extracted.is_empty() {
            String::new()
        } else {
            format!("  → {}", s.extracted.join(", "))
        };
        println!(
            "setup  {}  {status}  {} ms{extracted}",
            s.name, s.duration_ms
        );
    }
    if let Some(reason) = &r.aborted {
        println!("✗ {reason}. No se generó carga.");
        return;
    }
    println!(
        "iteraciones  planificadas {}  iniciadas {}  descartadas {}  interrumpidas {}  ({}/s)",
        t.planned, t.started, t.dropped, t.interrupted, t.iteration_rate
    );
    println!(
        "requests     {}  fallidos {}  no-2xx {}  ({}/s)  duración {}s",
        t.requests, t.failed, t.non_2xx, t.request_rate, r.duration_s
    );
    println!();

    let width = r
        .steps
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!(
        "{:<width$} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "paso", "req", "fallas", "no-2xx", "p50", "p90", "p95", "p99", "p99.9", "max"
    );
    for s in &r.steps {
        print!(
            "{:<width$} {:>7} {:>7} {:>7}",
            s.name, s.requests, s.failed, s.non_2xx
        );
        match &s.latency_ms {
            Some(l) => println!(
                " {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2}",
                l.p50, l.p90, l.p95, l.p99, l.p999, l.max
            ),
            None => println!(" {:>8}", "-"),
        }
    }
    println!(
        "{:>width$}  latencia en ms, desde el instante planificado",
        ""
    );

    for s in &r.steps {
        let only_200 = s.status_codes.len() == 1 && s.status_codes.contains_key("200");
        if !only_200 && !s.status_codes.is_empty() {
            let codes: Vec<_> = s
                .status_codes
                .iter()
                .map(|(c, n)| format!("{c}×{n}"))
                .collect();
            println!("códigos  {}: {}", s.name, codes.join("  "));
        }
        if !s.failures.is_empty() {
            let kinds: Vec<_> = s
                .failures
                .iter()
                .map(|(k, n)| format!("{}×{n}", serde_json::to_value(k).unwrap().as_str().unwrap()))
                .collect();
            println!("fallas   {}: {}", s.name, kinds.join("  "));
            for e in &s.error_samples {
                println!("         {e}");
            }
        }
    }
    for w in &r.warnings {
        println!("AVISO: {w}");
    }
    println!();
}
